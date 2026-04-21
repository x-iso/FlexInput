pub mod layouts;

use flexinput_core::{Signal, SignalType};

pub struct SinkPin {
    pub id: &'static str,
    pub display_name: &'static str,
    pub signal_type: SignalType,
}

/// Static metadata about an available virtual device type (no connections made).
pub struct DeviceKind {
    pub kind_id: &'static str,
    pub display_name: &'static str,
    /// If false, only one instance may be active at a time.
    pub allows_multiple: bool,
}

/// A virtual output device that receives signals from the graph.
pub trait VirtualDevice: Send {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    /// Ordered input pin layout for the canvas sink node.
    fn sink_pins(&self) -> &'static [SinkPin];
    /// Accept one signal value destined for the named pin.
    fn send(&mut self, pin: &str, value: Signal);
    /// Commit the current state to the system (e.g. submit a HID report).
    fn flush(&mut self);
}

#[cfg(windows)]
pub mod windows;

/// List available virtual device *types* — no connections are made.
pub fn available_device_kinds() -> &'static [DeviceKind] {
    #[cfg(windows)]
    { return windows::DEVICE_KINDS; }
    #[allow(unreachable_code)]
    &[]
}

/// Instantiate a virtual device by kind ID and instance index.
/// Called only when the user explicitly adds a device.
pub fn create_device(kind_id: &str, instance: usize) -> Box<dyn VirtualDevice> {
    #[cfg(windows)]
    return windows::create_device(kind_id, instance);
    #[cfg(not(windows))]
    panic!("No virtual devices on this platform: {kind_id} #{instance}")
}
