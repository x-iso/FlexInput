pub mod layouts;
pub mod driver_availability;

use flexinput_core::{Signal, SignalType};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

// ── Virtual-mouse physical-suppression config ───────────────────────────────
//
// The Virtual Keyboard & Mouse device pauses its pointer motion for a short
// window whenever another mouse is used — a physical one, a touchpad, another
// program's input — so a stick-driven cursor doesn't fight it on the desktop.
// `physical_mouse` tells them apart by the device Raw Input reports each event
// from. (It used to guess from the cursor moving "on its own", which games that
// warp the cursor tripped constantly.) These process-global atomics let the UI
// tune it (the mouse thread reads them every tick) — a global rather than a
// parameter because the device-build path is deep and signature-stable.

/// User setting: master enable for physical-mouse suppression. Default on
/// (desktop-friendly). When off, the virtual mouse always wins.
pub static MOUSE_SUPPRESSION_ENABLED: AtomicBool = AtomicBool::new(true);

/// User setting: how long (ms) a detected physical-mouse move blocks the
/// virtual mouse. Default 500. Lower = the virtual mouse recovers faster after
/// a stray cursor event.
pub static MOUSE_SUPPRESSION_RELEASE_MS: AtomicU32 = AtomicU32::new(500);

/// Runtime flag set by the I/O thread: true while a virtual gamepad is active
/// alongside the keyboard/mouse device ("mixed mode"). Forces suppression off
/// regardless of the user setting so gamepad+mouse aiming stays smooth.
pub static MOUSE_MIXED_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set the master physical-mouse-suppression enable. Called by the UI at
/// startup and whenever the setting changes.
pub fn set_mouse_suppression_enabled(on: bool) {
    MOUSE_SUPPRESSION_ENABLED.store(on, Ordering::Relaxed);
}

/// Set the suppression block window in ms (clamped to a sane 50..=2000 band).
pub fn set_mouse_suppression_release_ms(ms: u32) {
    MOUSE_SUPPRESSION_RELEASE_MS.store(ms.clamp(50, 2000), Ordering::Relaxed);
}

/// Mark whether mixed mode (virtual gamepad + keyboard/mouse) is active. Called
/// by the I/O thread each tick.
pub fn set_mouse_mixed_mode_active(on: bool) {
    MOUSE_MIXED_MODE_ACTIVE.store(on, Ordering::Relaxed);
}

/// Effective suppression state read by the mouse thread: the user setting AND
/// NOT mixed mode. The configured block window (ms) rides alongside.
pub fn mouse_suppression_effective() -> (bool, u32) {
    let on = MOUSE_SUPPRESSION_ENABLED.load(Ordering::Relaxed)
        && !MOUSE_MIXED_MODE_ACTIVE.load(Ordering::Relaxed);
    (on, MOUSE_SUPPRESSION_RELEASE_MS.load(Ordering::Relaxed).clamp(50, 2000))
}

/// The rate the I/O loop flushes the virtual devices at (Hz), and the user's
/// output-rate setting behind it (0 = same as polling). Set by the I/O thread;
/// read by the virtual mouse for its catch-up clamp and by the SendInput mouse
/// thread, which runs its own loop.
static OUTPUT_HZ_EFFECTIVE: AtomicU32 = AtomicU32::new(500);
static OUTPUT_HZ_SETTING: AtomicU32 = AtomicU32::new(0);

/// Called by the I/O thread with the output-rate setting (0 = same as polling)
/// and the rate it actually flushes at.
pub fn set_output_rate(setting: u32, effective: u32) {
    OUTPUT_HZ_SETTING.store(setting, Ordering::Relaxed);
    OUTPUT_HZ_EFFECTIVE.store(effective.max(1), Ordering::Relaxed);
}

/// The rate the virtual devices are flushed at.
pub(crate) fn output_hz() -> u32 {
    OUTPUT_HZ_EFFECTIVE.load(Ordering::Relaxed).max(1)
}

/// The SendInput mouse thread's rate: the output rate when the user picked one,
/// else its own 1 kHz (finer than the default polling rate, for smooth slow aim
/// — what it always ran at before the output rate was a setting).
pub(crate) fn sendinput_mouse_hz() -> u32 {
    match OUTPUT_HZ_SETTING.load(Ordering::Relaxed) {
        0 => 1000,
        hz => hz,
    }
}

/// Pointer-motion reports the virtual mouse has sent since start (a HID report
/// on HIDMaestro, a relative move on SendInput). The UI differences it into a
/// live reports/s, to compare with what Windows actually receives.
static MOUSE_REPORTS_SENT: AtomicU64 = AtomicU64::new(0);

pub(crate) fn count_mouse_report() {
    MOUSE_REPORTS_SENT.fetch_add(1, Ordering::Relaxed);
}

/// See [`MOUSE_REPORTS_SENT`].
pub fn mouse_reports_sent() -> u64 {
    MOUSE_REPORTS_SENT.load(Ordering::Relaxed)
}

// ── Mixed-output braiding (experimental) ────────────────────────────────────
//
// Probes games whose input arbiter behaves differently with mixed virtual
// gamepad + mouse output. Instead of muting either stream, braiding makes the
// two SUBMIT in strict alternation so a gamepad HID submit and a mouse
// SendInput never land in the same instant. It's a shared TURN TOKEN, not a
// wall-clock schedule: each output thread fires only on its turn, then passes
// the token to the other. Neither stream is zeroed — the mouse keeps
// accumulating between its turns, so no motion is lost, and an idle mouse just
// passes its turn so the gamepad is never chopped. An optional minimum spacing
// (`BRAID_HALF_US`) paces the alternation; 0 = real-time (alternate as fast as
// the threads tick, limited only by the polling/mouse rate). Off by default.

/// Master enable for mixed-output braiding.
pub static BRAID_ENABLED: AtomicBool = AtomicBool::new(false);
/// Minimum spacing between consecutive (alternating) packets, in µs. 0 =
/// real-time: pacing limited only by the output thread rates, but packets still
/// never coincide. Non-zero throttles each lane to `1 / (2 · half)`.
pub static BRAID_HALF_US: AtomicU32 = AtomicU32::new(0);
/// Whose turn it is to submit: 0 = gamepad, 1 = mouse.
static BRAID_TURN: AtomicU8 = AtomicU8::new(0);
/// Epoch-relative µs of the last token flip — drives the min-spacing throttle.
static BRAID_LAST_FLIP_US: AtomicU64 = AtomicU64::new(0);
/// Shared monotonic epoch so both threads measure the same timeline.
static BRAID_EPOCH: OnceLock<Instant> = OnceLock::new();

/// Set the braid enable. Called by the UI.
pub fn set_braid_enabled(on: bool) {
    BRAID_ENABLED.store(on, Ordering::Relaxed);
}
/// Set pacing by per-lane submit rate (Hz). 0 = real-time (fastest, no minimum
/// spacing). Otherwise each lane submits at `rate` Hz, the two interleaved a
/// half-period apart. (half = 1/(2·rate) → 500_000/rate µs.)
pub fn set_braid_rate_hz(rate: u32) {
    let half = if rate == 0 { 0 } else { 500_000 / rate.max(1) };
    BRAID_HALF_US.store(half, Ordering::Relaxed);
}

fn braid_now_us() -> u64 {
    BRAID_EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// Core turn-claim. `me` is this caller's token value (0 = gamepad, 1 = mouse).
/// Returns true if it's our turn and the min spacing has elapsed — in which case
/// we atomically claim the turn and pass the token to the other side.
fn braid_try(me: u8) -> bool {
    let half = BRAID_HALF_US.load(Ordering::Relaxed) as u64;
    if half > 0 && braid_now_us().saturating_sub(BRAID_LAST_FLIP_US.load(Ordering::Relaxed)) < half {
        return false;
    }
    if BRAID_TURN.compare_exchange(me, me ^ 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        BRAID_LAST_FLIP_US.store(braid_now_us(), Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// Gamepad (I/O) thread: should it submit (flush) its report this tick? Always
/// true when braiding is off; otherwise true only on the gamepad's turn.
pub fn braid_try_gamepad() -> bool {
    if !BRAID_ENABLED.load(Ordering::Relaxed) { return true; }
    braid_try(0)
}

/// Mouse (keymouse) thread: should it emit its accumulated motion this tick?
/// Always true when braiding is off; otherwise true only on the mouse's turn.
pub fn braid_try_mouse() -> bool {
    if !BRAID_ENABLED.load(Ordering::Relaxed) { return true; }
    braid_try(1)
}

/// Notches a `scroll_up/down/left/right` value asks for. The engine delivers
/// these pins as a count of off→on edges since the last read (a `Float`, see
/// `flexinput_engine::is_click_pin`), so a held gate is one notch, not one per
/// I/O tick. A bare `Bool(true)` still counts as one.
pub(crate) fn scroll_notches(value: Signal) -> i32 {
    match value {
        Signal::Float(n) => n.round() as i32,
        Signal::Bool(true) => 1,
        _ => 0,
    }
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

    /// Inject a synthetic rumble level so the NEXT `poll_outputs` reports it —
    /// used by the UI "ping" on an own-virtual pad to make its feedback flow
    /// through AutoMap to the mapped physical pad exactly as a game's rumble
    /// would. The caller controls duration (it sets the level each tick while
    /// the ping is active and `(0,0)` when it ends), so this sets the level
    /// directly without any auto-decay. Default: no-op (devices with no feedback
    /// don't ping). Only the HIDMaestro companion (XInput) path implements it.
    fn inject_rumble_ping(&mut self, _strong: f32, _weak: f32) {}

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

#[cfg(any(windows, target_os = "linux"))]
mod sony_feedback;

#[cfg(windows)]
pub mod composite_device;

#[cfg(any(windows, target_os = "linux"))]
pub mod keymouse_hm;

#[cfg(windows)]
mod physical_mouse;

/// No physical-mouse detection off Windows yet: the virtual mouse always moves.
#[cfg(not(windows))]
mod physical_mouse {
    pub(crate) fn suppressing() -> bool {
        false
    }
}

#[cfg(target_os = "linux")]
mod linux;

/// One sink-pin write into the encoder's accumulator. Vec2 pins decompose into
/// the x/y entries; scalar pins pass both views and PinState picks the one it
/// needs. Shared by every backend that encodes through a HIDMaestro profile.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn apply_pin(pins: &mut flexinput_hidmaestro::encode::PinState, pin: &str, value: Signal) {
    match (pin, value) {
        ("left_stick", Signal::Vec2(v)) => {
            pins.set("left_stick_x", v.x, true);
            pins.set("left_stick_y", v.y, true);
        }
        ("right_stick", Signal::Vec2(v)) => {
            pins.set("right_stick_x", v.x, true);
            pins.set("right_stick_y", v.y, true);
        }
        ("dpad", Signal::Vec2(v)) => {
            pins.set("dpad_left", 0.0, v.x < -0.5);
            pins.set("dpad_right", 0.0, v.x > 0.5);
            pins.set("dpad_up", 0.0, v.y > 0.5);
            pins.set("dpad_down", 0.0, v.y < -0.5);
        }
        _ => pins.set(pin, value.as_float(), value.as_bool()),
    }
}

/// Physical keys held right now (Raw Input, whatever has focus; our own virtual
/// keyboard and injected keys left out), or `None` when that can't be known.
#[cfg(windows)]
pub use physical_mouse::keyboard::physical_keys_held;
#[cfg(not(windows))]
pub fn physical_keys_held() -> Option<Vec<&'static str>> { None }

/// List available virtual device *types* — no connections are made.
pub fn available_device_kinds() -> &'static [DeviceKind] {
    #[cfg(windows)]
    { return windows::DEVICE_KINDS; }
    #[cfg(target_os = "linux")]
    { return linux::DEVICE_KINDS; }
    #[allow(unreachable_code)]
    &[]
}

/// Each pad that can carry USB audio, and the composite kind that does: the
/// same controller with its speaker / haptics and microphone interfaces, the
/// path a game's authored haptic waveforms take. Not offered as kinds of their
/// own — a pad's "Audio & Haptics" option switches between the two.
pub const USB_AUDIO_VARIANTS: &[(&str, &str)] = &[
    ("virtual.hm.dualsense", "virtual.usb.dualsense"),
    ("virtual.hm.dualsense_edge", "virtual.usb.dualsense_edge"),
];

/// Whether `kind_id` names a device this crate can build: a listed kind, or the
/// USB audio variant of a listed pad (Windows only: the composite is usbip-win2).
pub fn is_known_kind(kind_id: &str) -> bool {
    let listed = |id: &str| available_device_kinds().iter().any(|k| k.kind_id == id);
    listed(kind_id)
        || cfg!(windows)
            && USB_AUDIO_VARIANTS.iter().any(|(plain, usb)| *usb == kind_id && listed(plain))
}

/// Whether the device `dev_id` is a pad's USB audio variant.
pub fn has_usb_audio(dev_id: &str) -> bool {
    let kind = kind_prefix(dev_id);
    USB_AUDIO_VARIANTS.iter().any(|(_, usb)| *usb == kind)
}

/// The id of the same pad with USB audio switched the other way, keeping its
/// instance suffix — or `None` for a device that has no USB audio variant.
pub fn usb_audio_toggled_id(dev_id: &str) -> Option<String> {
    let kind = kind_prefix(dev_id);
    let suffix = &dev_id[kind.len()..];
    USB_AUDIO_VARIANTS.iter().find_map(|&(plain, usb)| {
        if kind == plain {
            Some(format!("{usb}{suffix}"))
        } else if kind == usb {
            Some(format!("{plain}{suffix}"))
        } else {
            None
        }
    })
}

/// Instantiate a virtual device by kind ID and instance index.
/// Called only when the user explicitly adds a device.
pub fn create_device(kind_id: &str, instance: usize) -> Box<dyn VirtualDevice> {
    #[cfg(windows)]
    return windows::create_device(kind_id, instance);
    #[cfg(target_os = "linux")]
    return linux::create_device(kind_id, instance);
    #[cfg(not(any(windows, target_os = "linux")))]
    panic!("No virtual devices on this platform: {kind_id} #{instance}")
}

/// Static pin metadata (sink pins, source pins, display name) for a kind without
/// building the device or touching any OS resource. Lets the UI add a canvas
/// sink node instantly; the device itself is built asynchronously and installed
/// into the pool by the device-ops worker. `None` for an unknown kind (every
/// kind on a platform with no virtual devices yet).
pub fn kind_pin_metadata(
    kind_id: &str,
    instance: usize,
) -> Option<(&'static [SinkPin], &'static [SourcePin], String)> {
    #[cfg(windows)]
    return windows::kind_pin_metadata(kind_id, instance);
    #[cfg(target_os = "linux")]
    return linux::kind_pin_metadata(kind_id, instance);
    #[cfg(not(any(windows, target_os = "linux")))]
    { let _ = (kind_id, instance); None }
}

/// Best-effort: extract the kind prefix (e.g. "virtual.xinput",
/// "virtual.hm.ds4") from a full device id like "virtual.xinput.2" or
/// "virtual.hm.ds4.1". HIDMaestro kinds and the USB composite ones live in
/// 3-segment namespaces (`virtual.hm.<model>`, `virtual.usb.<model>`); all
/// other kinds are 2-segment. Used by the UI to identify which kind a device id
/// belongs to (output-card toggles, physical-list filtering, etc.).
pub fn kind_prefix(dev_id: &str) -> String {
    let segs = if dev_id.starts_with("virtual.hm.") || dev_id.starts_with("virtual.usb.") { 3 } else { 2 };
    dev_id.split('.').take(segs).collect::<Vec<_>>().join(".")
}

#[cfg(test)]
mod usb_audio_tests {
    use super::*;

    #[test]
    fn toggling_usb_audio_swaps_the_kind_and_keeps_the_instance() {
        assert_eq!(usb_audio_toggled_id("virtual.hm.dualsense").as_deref(), Some("virtual.usb.dualsense"));
        assert_eq!(usb_audio_toggled_id("virtual.usb.dualsense.2").as_deref(), Some("virtual.hm.dualsense.2"));
        assert_eq!(
            usb_audio_toggled_id("virtual.hm.dualsense_edge.1").as_deref(),
            Some("virtual.usb.dualsense_edge.1")
        );
        assert_eq!(usb_audio_toggled_id("virtual.hm.ds4"), None);
        assert_eq!(usb_audio_toggled_id("virtual.keymouse"), None);
        assert!(has_usb_audio("virtual.usb.dualsense_edge.3"));
        assert!(!has_usb_audio("virtual.hm.dualsense"));
    }

    #[test]
    fn composite_kinds_are_known_but_not_listed() {
        assert_eq!(kind_prefix("virtual.usb.dualsense.4"), "virtual.usb.dualsense");
        for (_, usb) in USB_AUDIO_VARIANTS {
            // The composite is usbip-win2: Windows only.
            assert_eq!(is_known_kind(usb), cfg!(windows));
            assert!(!available_device_kinds().iter().any(|k| k.kind_id == *usb));
        }
        assert!(is_known_kind("virtual.hm.dualsense"));
        assert!(!is_known_kind("virtual.usb.ds4"));
    }
}
