//! Background gamepad-shortcut chord watcher, driven by the shared
//! `proc_device_signals` map.
//!
//! This is the path that lets gamepad shortcuts fire while a *game* holds
//! focus: the in-app evaluator (`process_shortcut_chords`) is focus-gated to
//! nav mode, so it can only fire while FlexInput is foreground. This watcher
//! runs on its own thread and reads the same signal map the I/O thread
//! publishes, so it detects the user's assigned chords regardless of focus.
//!
//! Which shortcuts this thread owns depends on the "only in gamepad navigation"
//! setting, resolved by the UI when it republishes [`ChordWatchConfig`]:
//!  * The **config-overlay** chord is ALWAYS watched here (its whole purpose is
//!    to be summonable mid-game).
//!  * The other four (see-through / panic / info-overlay / pin) are watched
//!    here only when nav-only is OFF; when it's ON the UI hands us `None` for
//!    them and the focus-gated nav path handles them instead.
//!
//! Why the shared map and not a private `Gilrs` instance: DualSense's PS button
//! and Switch's Home aren't exposed through gilrs's standard XInput/HID mapping
//! on Windows — only through the raw HID parsing the I/O thread does in
//! `flexinput-devices::gilrs_backend`, which publishes to `proc_device_signals`.
//! Reading the same map gets correct detection for every surfaced controller.
//!
//! Detection: a chord is "held" when every button in its combo is pressed on
//! ONE non-virtual device (FlexInput's own virtual pads `gilrs:<kind>:v<N>` are
//! excluded — a mapped press loops back through them and would double-fire).
//! Firing follows the configured press mode via the shared
//! [`crate::gamepad_nav::chord_fire`] helper, so it matches the in-app shortcut
//! semantics exactly (`down` / `long` / `double` + `gap_ms`). A startup grace
//! window and a per-target post-fire refractory absorb BT-handshake noise and
//! duplicate-device echoes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use flexinput_core::Signal;

use crate::gamepad_nav::{chord_fire, ChordFireState};

/// One assigned shortcut chord: the buttons plus its press mode / time gap.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShortcutSpec {
    /// The chord buttons (canonical pin ids, e.g. `["btn_lb", "btn_rb"]`), or a
    /// single allowed button (`btn_guide` / `btn_capture`).
    pub combo: Vec<String>,
    /// Press mode: `"down"` | `"long"` | `"double"` (see `chord_fire`).
    pub mode: String,
    /// Time gap (ms) the mode reads (hold time / inter-tap gap).
    pub gap_ms: f32,
}

/// Live config for the watcher, shared with the UI thread (which republishes it
/// whenever a binding changes). A `None` target is not watched here — either
/// unassigned, or (for the four window shortcuts) handled by the nav path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChordWatchConfig {
    pub seethrough: Option<ShortcutSpec>,
    pub panic: Option<ShortcutSpec>,
    pub overlay: Option<ShortcutSpec>,
    pub pin: Option<ShortcutSpec>,
    pub config: Option<ShortcutSpec>,
}

/// The five toggle flags the watcher raises; the UI loop consumes each once per
/// frame and applies the corresponding action. Shared (each is its own `Arc`)
/// with the keyboard-hotkey listeners, which raise the same flags.
#[derive(Clone)]
pub struct ShortcutToggles {
    pub seethrough: Arc<AtomicBool>,
    pub panic: Arc<AtomicBool>,
    pub overlay: Arc<AtomicBool>,
    pub pin: Arc<AtomicBool>,
    pub config: Arc<AtomicBool>,
}

const POLL_INTERVAL: Duration = Duration::from_millis(33); // ~30 Hz
/// Post-fire refractory. A physical press mapped through to a virtual pad can
/// come back as a second edge on another device a poll or two later (and this
/// machine has been seen surfacing outright duplicate pads); the window absorbs
/// that echo. Longer than any loopback latency, shorter than a human re-toggle.
const TOGGLE_REFRACTORY_MS: u64 = 400;
/// Grace window after the thread starts before any fire is honored. Covers a
/// controller present at launch whose BT handshake / first raw-HID reports can
/// momentarily latch buttons true.
const STARTUP_GRACE_MS: u64 = 750;

/// Per-target edge/timing state kept across poll iterations.
#[derive(Default)]
struct TargetWatch {
    fire_state: ChordFireState,
    last_toggle: Option<Instant>,
}

type SignalMap = HashMap<(String, String), Signal>;

/// True when some single non-virtual device holds every button in `combo`.
fn combo_held(snap: &SignalMap, combo: &[String]) -> bool {
    if combo.is_empty() { return false; }
    // Tally, per device, how many of the combo's buttons are currently true.
    let mut per_dev: HashMap<&str, usize> = HashMap::new();
    for ((dev, sig), val) in snap.iter() {
        if crate::app::is_own_virtual_gilrs_id(dev) { continue; }
        if !matches!(val, Signal::Bool(true)) { continue; }
        if combo.iter().any(|p| p == sig) {
            *per_dev.entry(dev.as_str()).or_insert(0) += 1;
        }
    }
    per_dev.values().any(|&n| n >= combo.len())
}

/// Evaluate one target: if its chord fires (past grace + refractory), raise the
/// toggle flag. Resets edge state when the target is unassigned.
fn service_target(
    tw: &mut TargetWatch,
    spec: &Option<ShortcutSpec>,
    toggle: &AtomicBool,
    snap: &SignalMap,
    started_at: Instant,
) {
    let Some(spec) = spec else {
        // Unassigned (or handled elsewhere): clear state so a later re-assign
        // starts from a clean edge instead of swallowing the first press.
        *tw = TargetWatch::default();
        return;
    };
    if spec.combo.is_empty() { return; }
    let held = combo_held(snap, &spec.combo);
    let now = Instant::now();
    if chord_fire(&mut tw.fire_state, held, &spec.mode, spec.gap_ms, now) {
        let in_grace = now.duration_since(started_at)
            < Duration::from_millis(STARTUP_GRACE_MS);
        let in_refractory = tw.last_toggle
            .map(|t| now.duration_since(t) < Duration::from_millis(TOGGLE_REFRACTORY_MS))
            .unwrap_or(false);
        if !in_grace && !in_refractory {
            toggle.store(true, Ordering::Relaxed);
            tw.last_toggle = Some(now);
        }
    }
}

pub fn spawn_chord_watcher(
    config: Arc<RwLock<ChordWatchConfig>>,
    toggles: ShortcutToggles,
    proc_device_signals: flexinput_engine::ArcSignals,
) {
    std::thread::Builder::new()
        .name("gamepad-shortcut-watcher".into())
        .spawn(move || {
            let started_at = Instant::now();
            let mut w_seethrough = TargetWatch::default();
            let mut w_panic = TargetWatch::default();
            let mut w_overlay = TargetWatch::default();
            let mut w_pin = TargetWatch::default();
            let mut w_config = TargetWatch::default();

            loop {
                let cfg = config.read().map(|c| c.clone()).unwrap_or_default();

                // Snapshot the signal map. With the ArcSwap publish model the
                // load is a refcount bump and iteration walks the snapshot
                // without contending the I/O thread.
                let snap = proc_device_signals.load_full();

                service_target(&mut w_seethrough, &cfg.seethrough, &toggles.seethrough, &snap, started_at);
                service_target(&mut w_panic, &cfg.panic, &toggles.panic, &snap, started_at);
                service_target(&mut w_overlay, &cfg.overlay, &toggles.overlay, &snap, started_at);
                service_target(&mut w_pin, &cfg.pin, &toggles.pin, &snap, started_at);
                service_target(&mut w_config, &cfg.config, &toggles.config, &snap, started_at);

                std::thread::sleep(POLL_INTERVAL);
            }
        })
        .expect("failed to spawn gamepad-shortcut-watcher thread");
}
