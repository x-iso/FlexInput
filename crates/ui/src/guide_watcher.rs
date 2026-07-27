//! Background config-overlay chord watcher, driven by the shared
//! `proc_device_signals` map.
//!
//! This is the ONE path that can summon the config overlay while a *game*
//! holds focus: the in-app gamepad shortcut evaluators
//! (`process_shortcut_chords` / `check_shortcut_chords_global`) are focus-gated,
//! so they can't fire while another window is foreground. This watcher runs on
//! its own thread and reads the same signal map the I/O thread publishes, so it
//! detects the user's assigned config-overlay chord regardless of focus.
//!
//! Why the shared map and not a private `Gilrs` instance: DualSense's PS button
//! and Switch's Home aren't exposed through gilrs's standard XInput/HID mapping
//! on Windows — only through the raw HID parsing the I/O thread does in
//! `flexinput-devices::gilrs_backend`, which publishes to `proc_device_signals`.
//! Reading the same map gets correct detection for every surfaced controller.
//!
//! Detection:
//!  * The chord is "held" when every button in `combo` is pressed on ONE
//!    non-virtual device (FlexInput's own virtual pads `gilrs:<kind>:v<N>` are
//!    excluded — a mapped press loops back through them and would double-fire).
//!  * Firing follows the configured press `mode` via the shared
//!    [`crate::gamepad_nav::chord_fire`] helper, so it matches the in-app
//!    shortcut semantics exactly (`down` / `long` / `double` + `gap_ms`).
//!  * A startup grace window suppresses fires for the first fraction of a
//!    second (covers a controller's BT-handshake / initial raw-HID garbage),
//!    and a short post-fire refractory absorbs duplicate-device echoes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use flexinput_core::Signal;

use crate::gamepad_nav::{chord_fire, ChordFireState};

/// Live config for the background chord watcher, shared with the UI thread
/// (which republishes it whenever the user re-binds the chord / mode / gap).
#[derive(Debug, Clone, Default)]
pub struct ChordWatchConfig {
    /// True while a config-overlay chord is assigned and should be watched.
    pub enabled: bool,
    /// The chord buttons (canonical pin ids, e.g. `["btn_lb", "btn_rb"]`).
    pub combo: Vec<String>,
    /// Press mode: `"down"` | `"long"` | `"double"` (see `chord_fire`).
    pub mode: String,
    /// Time gap (ms) the mode reads (hold time / inter-tap gap).
    pub gap_ms: f32,
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

pub fn spawn_chord_watcher(
    config: Arc<RwLock<ChordWatchConfig>>,
    toggle_requested: Arc<AtomicBool>,
    proc_device_signals: flexinput_engine::ArcSignals,
) {
    std::thread::Builder::new()
        .name("config-chord-watcher".into())
        .spawn(move || {
            let started_at = Instant::now();
            let mut fire_state = ChordFireState::default();
            let mut last_toggle_at: Option<Instant> = None;

            loop {
                let cfg = config.read().map(|c| c.clone()).unwrap_or_default();

                if !cfg.enabled || cfg.combo.is_empty() {
                    // Reset edge state so a re-enable starts clean (no stale
                    // "was held" that would swallow the first press).
                    fire_state = ChordFireState::default();
                    std::thread::sleep(POLL_INTERVAL);
                    continue;
                }

                // Snapshot the signal map. With the ArcSwap publish model the
                // load is a refcount bump and iteration walks the snapshot
                // without contending the I/O thread.
                let snap = proc_device_signals.load_full();

                // The chord is held when some single non-virtual device sees
                // every combo button true. Tally matching held pins per device.
                let held = {
                    let mut per_dev: HashMap<&str, usize> = HashMap::new();
                    for ((dev, sig), val) in snap.iter() {
                        if crate::app::is_own_virtual_gilrs_id(dev) { continue; }
                        if !matches!(val, Signal::Bool(true)) { continue; }
                        if cfg.combo.iter().any(|p| p == sig) {
                            *per_dev.entry(dev.as_str()).or_insert(0) += 1;
                        }
                    }
                    per_dev.values().any(|&n| n >= cfg.combo.len())
                };

                let now = Instant::now();
                let fired = chord_fire(&mut fire_state, held, &cfg.mode, cfg.gap_ms, now);
                if fired {
                    let in_grace = now.duration_since(started_at)
                        < Duration::from_millis(STARTUP_GRACE_MS);
                    let in_refractory = last_toggle_at
                        .map(|t| now.duration_since(t)
                            < Duration::from_millis(TOGGLE_REFRACTORY_MS))
                        .unwrap_or(false);
                    if !in_grace && !in_refractory {
                        toggle_requested.store(true, Ordering::Relaxed);
                        last_toggle_at = Some(now);
                    }
                }

                std::thread::sleep(POLL_INTERVAL);
            }
        })
        .expect("failed to spawn config-chord-watcher thread");
}
