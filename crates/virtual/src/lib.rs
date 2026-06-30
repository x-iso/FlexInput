pub mod layouts;
pub mod driver_availability;

use flexinput_core::{Signal, SignalType};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

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

// ── Virtual-mouse physical-suppression config ───────────────────────────────
//
// The Virtual Keyboard & Mouse device blocks its synthetic mouse motion for a
// short window whenever it sees the OS cursor move "on its own" (i.e. a real
// mouse the user is also holding), so a stick-driven cursor doesn't fight the
// physical one on the desktop. In games this heuristic is a liability — many
// titles warp/recenter the cursor every frame, which the heuristic misreads as
// physical input and uses to repeatedly stall the virtual mouse, producing the
// "jerky in-game" feel. These process-global atomics let the UI tune it (the
// mouse thread reads them every tick), following the `REQUESTED_POLL_HZ`
// pattern — the device-build path is deep and signature-stable.

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
