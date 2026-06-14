pub mod layouts;
pub mod driver_availability;

use flexinput_core::{Signal, SignalType};

pub struct SinkPin {
    pub id: &'static str,
    pub display_name: &'static str,
    pub signal_type: SignalType,
}

pub struct SourcePin {
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
    /// Zero all outputs and flush — called every frame while the tab is bypassed
    /// so lingering inputs (drifting sticks, held keys) are released immediately.
    fn reset_outputs(&mut self) {}
    /// Returns false if the underlying system resource is unavailable
    /// (e.g. ViGEmBus not installed, enigo init failed).
    fn is_connected(&self) -> bool { true }
    /// Output pins this device exposes back into the graph (e.g. rumble from games).
    /// Empty by default — only devices with feedback signals implement this.
    fn source_pins(&self) -> &'static [SourcePin] { &[] }
    /// Poll latest output values from the OS/game (e.g. rumble motor speeds).
    /// Returns (pin_id, signal) pairs; called each I/O frame after flush().
    fn poll_outputs(&mut self) -> Vec<(&'static str, Signal)> { vec![] }

    /// Relinquish OS ownership so this device's `Drop` does NOT tear the
    /// underlying virtual node down — the node is intentionally left alive past
    /// app exit for reclaim on next launch. Called on a clean shutdown when the
    /// user enabled "keep virtual controllers alive".
    ///
    /// Default: no-op. Only HIDMaestro devices override it (they own a
    /// helper-created PnP node that survives the helper process). ViGEmBus
    /// targets (XInput/DS4) cannot persist — ViGEmBus auto-removes them when the
    /// creating process exits — so they ignore this and are recreated next run.
    fn persist_on_drop(&mut self) {}
}

#[cfg(windows)]
pub mod windows;

#[cfg(windows)]
pub mod hidmaestro_device;

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

/// Static pin metadata (sink pins, source pins, display name) for a kind without
/// building the device or touching any OS resource. Lets the UI add a canvas
/// sink node instantly; the device itself is built asynchronously and installed
/// into the pool by the device-ops worker. `None` for an unknown kind.
#[cfg(windows)]
pub fn kind_pin_metadata(
    kind_id: &str,
    instance: usize,
) -> Option<(&'static [SinkPin], &'static [SourcePin], String)> {
    windows::kind_pin_metadata(kind_id, instance)
}

/// Best-effort: extract the kind prefix (e.g. "virtual.xinput") from a
/// full device id like "virtual.xinput.2". Used by the UI to identify
/// FlexInput's own virtuals when filtering the physical-devices list.
pub fn kind_prefix(dev_id: &str) -> String {
    dev_id.split('.').take(2).collect::<Vec<_>>().join(".")
}
