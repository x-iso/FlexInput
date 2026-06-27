pub mod gamepad;
pub mod gilrs_backend;
pub mod gyro;
pub mod haptic_pcm;
pub mod hidhide;
pub mod identification;
pub mod layouts;
pub mod midi;
pub mod spectrum;

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
}

pub fn init_backends() -> Vec<Box<dyn DeviceBackend>> {
    let mut backends: Vec<Box<dyn DeviceBackend>> = Vec::new();
    if let Some(b) = GilrsBackend::try_new() {
        backends.push(Box::new(b));
    }
    backends
}
