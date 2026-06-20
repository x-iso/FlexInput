pub mod layouts;
pub mod driver_availability;

use flexinput_core::{Signal, SignalType};
use std::sync::atomic::{AtomicU32, Ordering};

/// App-wide desired polling rate (Hz), mirrored from the UI's `polling_hz`
/// setting. HIDMaestro XInput devices read this at create time to set the XUSB
/// companion's input-pump period (`PollIntervalMs = round(1000/hz)`), so the
/// virtual Xbox pad delivers XInput at the configured rate instead of the
/// driver's fixed 125Hz default. A global (not a `create_device` parameter)
/// because the device-build path is deep and signature-stable. 0 = unset =>
/// devices fall back to the driver default. See `requested_poll_interval_ms`.
pub static REQUESTED_POLL_HZ: AtomicU32 = AtomicU32::new(0);

/// Set the desired polling rate (Hz) read by HIDMaestro XInput device creation.
/// Called by the UI at startup and whenever the polling-rate setting changes.
pub fn set_requested_poll_hz(hz: u32) {
    REQUESTED_POLL_HZ.store(hz, Ordering::Relaxed);
}

/// The XUSB companion input-pump period in ms derived from [`REQUESTED_POLL_HZ`],
/// clamped to 1..=8 (1000..125 Hz). Returns 0 when unset, meaning "use the
/// driver default" — the create IPC treats 0 as "don't write PollIntervalMs".
pub fn requested_poll_interval_ms() -> u32 {
    let hz = REQUESTED_POLL_HZ.load(Ordering::Relaxed);
    if hz == 0 {
        return 0;
    }
    // round(1000/hz), clamped to the supported whole-ms band.
    let ms = ((1000.0 / hz as f32).round() as u32).clamp(1, 8);
    ms
}

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

/// Best-effort: extract the kind prefix (e.g. "virtual.xinput",
/// "virtual.hm.ds4") from a full device id like "virtual.xinput.2" or
/// "virtual.hm.ds4.1". HIDMaestro kinds live in a 3-segment namespace
/// (`virtual.hm.<model>`); all other kinds are 2-segment. Used by the UI to
/// identify which kind a device id belongs to (output-card toggles, physical-list
/// filtering, etc.).
pub fn kind_prefix(dev_id: &str) -> String {
    let segs = if dev_id.starts_with("virtual.hm.") { 3 } else { 2 };
    dev_id.split('.').take(segs).collect::<Vec<_>>().join(".")
}
