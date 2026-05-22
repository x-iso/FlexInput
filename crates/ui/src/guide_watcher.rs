//! Pin-trigger watcher driven by the shared `proc_device_signals` map.
//!
//! Why the shared map and not a private `Gilrs` instance: DualSense's
//! PS button isn't exposed through gilrs's standard XInput/HID mapping
//! on Windows — only through the raw HID parsing the I/O thread does
//! in `flexinput-devices::gilrs_backend`. That parsing publishes the
//! result to `proc_device_signals` keyed by
//! `(device_id, "btn_guide")`. By reading the same map we get correct
//! detection for DualSense, Switch Pro, XInput pads via ViGEm, and any
//! other controller surfaced in the canvas.
//!
//! Detection:
//!  * Watches the `btn_guide` signal across every device.
//!  * Rising-edge taps trigger `toggle_requested` (single OR double-tap
//!    per `require_double_tap`).
//!  * If `chord_signal` is `Some`, BOTH the guide AND that signal
//!    (e.g. `"btn_lb"`) must be pressed at the moment the guide tap
//!    fires. Lets the user disambiguate from Steam / Game Bar
//!    single-press Guide handling.
//!
//! AutoMap-style chord learn:
//!  * `learn_chord` (atomic bool) — when set true by the Settings UI,
//!    the watcher captures the next rising-edge button signal it sees
//!    on any device (other than `btn_guide`) and stores its name in
//!    `learned_chord`. The UI clears `learn_chord` once it consumes
//!    the result.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use flexinput_core::Signal;

#[derive(Debug, Clone, Default)]
pub struct GuideWatchConfig {
    pub enabled: bool,
    pub require_double_tap: bool,
    /// If set, this signal (e.g. `"btn_lb"`) must also be true on the
    /// same device when the guide tap fires. None = no chord.
    pub chord_signal: Option<String>,
}

const POLL_INTERVAL: Duration = Duration::from_millis(33); // ~30 Hz
const DOUBLE_TAP_MS: u64 = 300;

/// Buttons valid as chord candidates. Only Bool signals are considered;
/// `btn_guide` is excluded so we don't accidentally bind guide-as-chord.
fn is_chord_candidate(signal_name: &str) -> bool {
    signal_name != "btn_guide"
        && signal_name.starts_with("btn_")
}

pub fn spawn_guide_watcher(
    config: Arc<RwLock<GuideWatchConfig>>,
    toggle_requested: Arc<AtomicBool>,
    proc_device_signals: flexinput_engine::ArcSignals,
    learn_chord: Arc<AtomicBool>,
    learned_chord: Arc<Mutex<Option<String>>>,
) {
    std::thread::Builder::new()
        .name("pin-guide-watcher".into())
        .spawn(move || {
            // Per-device rising-edge state for the guide button.
            let mut guide_prev: HashMap<String, bool> = HashMap::new();
            // For learn-chord mode: previous state of every Bool signal
            // so we can spot a fresh rising edge of a non-guide button.
            let mut learn_prev: HashMap<(String, String), bool> = HashMap::new();
            let mut last_tap_at: Option<Instant> = None;

            loop {
                let cfg = config.read().map(|c| c.clone()).unwrap_or_default();
                let learning = learn_chord.load(Ordering::Relaxed);

                // Snapshot the signal map. With the ArcSwap publish
                // model the load is a refcount bump and the iteration
                // walks the snapshot without contending the I/O thread.
                let snap = proc_device_signals.load_full();
                let snapshot: Vec<((String, String), Signal)> = snap.iter()
                    .map(|(k, v)| (k.clone(), *v))
                    .collect();

                if learning {
                    // Look for a rising-edge button press on any device,
                    // excluding the guide itself. First one wins.
                    let mut found: Option<String> = None;
                    for ((dev, sig), val) in &snapshot {
                        if let Signal::Bool(now) = val {
                            if is_chord_candidate(sig) {
                                let key = (dev.clone(), sig.clone());
                                let was = *learn_prev.get(&key).unwrap_or(&false);
                                if *now && !was && found.is_none() {
                                    found = Some(sig.clone());
                                }
                                learn_prev.insert(key, *now);
                            }
                        }
                    }
                    if let Some(name) = found {
                        if let Ok(mut g) = learned_chord.lock() {
                            *g = Some(name);
                        }
                        learn_chord.store(false, Ordering::Relaxed);
                    }
                } else {
                    learn_prev.clear();
                }

                if cfg.enabled {
                    for ((dev, sig), val) in &snapshot {
                        if sig != "btn_guide" { continue; }
                        let Signal::Bool(now_pressed) = val else { continue; };
                        let was_pressed = *guide_prev.get(dev).unwrap_or(&false);
                        if *now_pressed && !was_pressed {
                            // Chord gate: if configured, the chord
                            // signal must also be true on this same
                            // device at the moment the guide rises.
                            if let Some(chord) = cfg.chord_signal.as_deref() {
                                let chord_held = snapshot.iter().any(|((d, s), v)| {
                                    d == dev && s == chord && matches!(v, Signal::Bool(true))
                                });
                                if !chord_held {
                                    guide_prev.insert(dev.clone(), *now_pressed);
                                    continue;
                                }
                            }
                            let now = Instant::now();
                            if cfg.require_double_tap {
                                let is_double = last_tap_at
                                    .map(|t| now.duration_since(t)
                                        < Duration::from_millis(DOUBLE_TAP_MS))
                                    .unwrap_or(false);
                                if is_double {
                                    toggle_requested.store(true, Ordering::Relaxed);
                                    last_tap_at = None;
                                } else {
                                    last_tap_at = Some(now);
                                }
                            } else {
                                toggle_requested.store(true, Ordering::Relaxed);
                            }
                        }
                        guide_prev.insert(dev.clone(), *now_pressed);
                    }
                } else {
                    guide_prev.clear();
                    last_tap_at = None;
                }

                std::thread::sleep(POLL_INTERVAL);
            }
        })
        .expect("failed to spawn pin-guide-watcher thread");
}
