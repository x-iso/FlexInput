pub mod gamepad;
pub mod gilrs_backend;
pub mod identification;
pub mod layouts;

use flexinput_core::{Signal, SignalType};

pub use gilrs_backend::GilrsBackend;
pub use identification::ControllerKind;

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
}

pub trait DeviceBackend: Send {
    fn enumerate(&self) -> Vec<PhysicalDevice>;
    fn poll(&mut self) -> Vec<(String, String, Signal)>;
}

pub fn init_backend() -> Option<Box<dyn DeviceBackend>> {
    GilrsBackend::try_new().map(|b| Box::new(b) as Box<dyn DeviceBackend>)
}
