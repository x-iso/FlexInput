pub mod gamepad;
pub mod gilrs_backend;
pub mod gyro;
pub mod hidhide;
pub mod identification;
pub mod layouts;
pub mod midi;

use flexinput_core::{Signal, SignalType};

pub use gilrs_backend::GilrsBackend;
pub use hidhide::HidHideClient;
pub use identification::ControllerKind;
pub use midi::MidiBackend;

pub struct DevicePin {
    pub id: String,
    pub display_name: String,
    pub signal_type: SignalType,
}

pub struct PhysicalDevice {
    pub id: String,
    pub display_name: String,
    pub kind: ControllerKind,
    pub outputs: Vec<DevicePin>,
    pub inputs: Vec<DevicePin>,
    /// Windows device instance path (e.g. `HID\VID_054C&PID_09CC\5&...`),
    /// used for HidHide blacklist operations. None if unavailable.
    pub instance_path: Option<String>,
}

pub trait DeviceBackend: Send {
    fn enumerate(&mut self) -> Vec<PhysicalDevice>;
    fn poll(&mut self) -> Vec<(String, String, Signal)>;
    /// Route a signal to a physical output pin (e.g. MIDI CC send).
    /// Backends that don't support output can ignore this.
    fn send(&mut self, _device_id: &str, _pin_id: &str, _signal: Signal) {}
}

pub fn init_backends() -> Vec<Box<dyn DeviceBackend>> {
    let mut backends: Vec<Box<dyn DeviceBackend>> = Vec::new();
    if let Some(b) = GilrsBackend::try_new() {
        backends.push(Box::new(b));
    }
    backends
}
