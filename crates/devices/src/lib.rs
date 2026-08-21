pub mod gamepad;
pub mod gilrs_backend;
pub mod classic_bt;
pub mod gyro;
pub mod haptic_pcm;
pub mod hidhide;
pub mod identification;
pub mod layouts;
pub mod midi;
pub mod sdl_backend;
pub mod spectrum;

#[cfg(feature = "joycon2")]
pub mod joycon2_backend;

#[cfg(windows)]
mod dualsense_haptic;

#[cfg(windows)]
pub mod loopback_haptic;

#[cfg(windows)]
pub mod loopback_manager;

use flexinput_core::{Signal, SignalType};

pub use gilrs_backend::{
    probe_xinput_slots, probe_xinput_slots_cached, GilrsBackend, XInputSlotInfo,
};
pub use hidhide::HidHideClient;
pub use identification::ControllerKind;
pub use midi::MidiBackend;

#[derive(Clone)]
pub struct DevicePin {
    pub id: String,
    pub display_name: String,
    pub signal_type: SignalType,
}

#[derive(Clone)]
pub struct PhysicalDevice {
    pub id: String,
    pub display_name: String,
    pub kind: ControllerKind,
    pub outputs: Vec<DevicePin>,
    pub inputs: Vec<DevicePin>,
    /// Windows device instance path (e.g. `HID\VID_054C&PID_09CC\5&...`),
    /// used for HidHide blacklist operations. None if unavailable.
    pub instance_path: Option<String>,
    /// USB vendor/product id for a gamepad (gilrs pads). `None` for devices
    /// without one (MIDI). Used to resolve the HID instance id off-thread for
    /// HidHide masking, so a slow SetupAPI lookup never runs on the I/O loop.
    pub vid: Option<u16>,
    pub pid: Option<u16>,
}

pub trait DeviceBackend: Send {
    fn enumerate(&mut self) -> Vec<PhysicalDevice>;
    fn poll(&mut self) -> Vec<(String, String, Signal)>;
    /// Route a signal to a physical output pin (e.g. MIDI CC send).
    /// Backends that don't support output can ignore this.
    fn send(&mut self, _device_id: &str, _pin_id: &str, _signal: Signal) {}
    /// Drain accumulated raw-event counts per device id. Used by the I/O
    /// thread to compute live per-device polling rates. Default: empty
    /// (backends that don't track events are reported as 0 Hz).
    fn take_event_counts(&mut self) -> Vec<(String, u32)> { Vec::new() }
    /// Configure the snap-back outlier-spike filter for a specific
    /// physical device. Called from the I/O loop each tick with the
    /// current UI settings (cheap no-op if unchanged). Backends without
    /// a raw IMU stream should ignore this.
    fn set_spike_filter(&mut self, _device_id: &str, _enabled: bool, _sensitivity_pct: f32) {}
    /// Global "route every pad through SDL" switch, pushed from the I/O loop
    /// each tick (cheap no-op when unchanged). When on, the SDL backend claims
    /// ALL gamepads (not just the ones kind-detect calls `Generic`) and the
    /// gilrs backend emits + enumerates nothing, so a controller's inputs come
    /// entirely from SDL. Primarily a verification switch — it lets a pad with a
    /// native parser (DualSense / Switch Pro) be read through SDL and compared
    /// against its canonical-correct native path. Changes device IDs
    /// (`gilrs:…` → `sdl:…`), so existing wiring does not follow the switch.
    fn set_sdl_all_pads(&mut self, _on: bool) {}
    /// Arm Joy-Con 2's Bluetooth pairing handshake, pushed from the I/O loop
    /// each tick like `set_sdl_all_pads`. Only the BLE backend acts on it.
    ///
    /// Off by default and deliberately opt-in: finalising the handshake writes
    /// the host address and link key into the controller's flash, which holds
    /// only two host slots, so pairing to a PC can evict a console's entry.
    /// With it off the controller still streams input — it just won't
    /// wake-and-reconnect on a button press, so the Sync button is needed each
    /// session.
    fn set_joycon2_pairing(&mut self, _on: bool) {}
    /// Hand a device its measured resting gyro drift, in **degrees per second
    /// on the device's own rate axes**, or `None` to fall back to whatever the
    /// backend compiled in.
    ///
    /// ⭐ Separate from the `gyro_offset` calibration the engine applies to the
    /// pins, and NOT a duplicate of it. Some devices integrate an orientation
    /// internally from the same rate; for those, a correction applied to the
    /// output pins arrives too late to stop the integrated estimate drifting,
    /// and only the device layer can subtract it early enough to fix both.
    /// Backends without an internal integrator can ignore this.
    fn set_gyro_drift(&mut self, _device_id: &str, _drift: Option<[f32; 3]>) {}
}

pub fn init_backends() -> Vec<Box<dyn DeviceBackend>> {
    let mut backends: Vec<Box<dyn DeviceBackend>> = Vec::new();
    if let Some(b) = GilrsBackend::try_new() {
        backends.push(Box::new(b));
    }
    // SDL is a sibling source for pads gilrs/kind-detect classify as `Generic`
    // (Steam Controller, 8BitDo, third-party). It self-inits lazily on the first
    // poll (on the device-io thread, per SDL's threading rule) and enumerates
    // ONLY `Generic` pads, so it never double-surfaces a controller gilrs owns.
    // Pushed AFTER gilrs so gilrs's tuned paths take precedence in iteration.
    backends.push(Box::new(sdl_backend::SdlBackend::new()));
    // Joy-Con 2 over BLE. Cannot overlap with the two above: Windows binds no
    // driver to these controllers, so gilrs and SDL never see them and no
    // dedup is needed. Starts its own thread and enumerates nothing until a
    // controller has finished connecting, so it is free to add unconditionally.
    //
    // Pairing defaults OFF here: the LTK handshake writes the host address into
    // controller flash, which has only two slots and can evict a console's
    // entry. The UI turns it on explicitly via `set_pairing_enabled`.
    #[cfg(feature = "joycon2")]
    backends.push(Box::new(joycon2_backend::Joycon2Backend::new(false)));
    // Bluetooth Classic gamepads on our own dongle. Enumerates nothing until a
    // controller with a STORED LINK KEY connects, so on a machine that has
    // never run the pairing tool it costs one idle thread and touches no radio
    // at all — see `classic_bt`, which also explains why it yields the dongle
    // to the Joy-Con 2 hub rather than competing for it.
    backends.push(Box::new(classic_bt::ClassicBtBackend::new()));
    backends
}
