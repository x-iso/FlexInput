//! What the Audio Stream Haptics captures publish for the engine to read: each
//! node's latest per-side `(amp, freq)` and its log-band spectrum, keyed by the
//! node's effective uid.
//!
//! The captures themselves are WASAPI and Windows-only
//! ([`crate::loopback_manager`]), but the eval block reads this on every
//! platform. Where nothing captures, every read is zeros — no audio — and engine
//! tests can publish params here to drive the module anywhere.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::spectrum::N_BANDS;

/// Per-side amplitude (0–1) and normalized carrier frequency (0–1) recovered
/// from the captured audio. Frequency normalization matches the `*_freq` pin
/// convention used elsewhere (0 = [`crate::haptic_pcm::FREQ_MIN_HZ`], 1 = MAX).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoopbackParams {
    pub l_amp: f32,
    pub l_freq: f32,
    pub r_amp: f32,
    pub r_freq: f32,
}

// ── per-node params (read by the eval block) ────────────────────────────────────

static PARAMS: RwLock<Option<HashMap<usize, LoopbackParams>>> = RwLock::new(None);

pub(crate) fn set_params(uid: usize, params: LoopbackParams) {
    let mut guard = PARAMS.write().unwrap();
    guard.get_or_insert_with(HashMap::new).insert(uid, params);
}

/// Publish captured params for a node as if its capture had produced them, so
/// engine tests can drive Audio Stream Haptics without a WASAPI capture.
#[doc(hidden)]
pub fn set_params_for_test(uid: usize, params: LoopbackParams) {
    set_params(uid, params);
}

#[cfg(windows)]
pub(crate) fn retain_params(live: &std::collections::HashSet<usize>) {
    let mut guard = PARAMS.write().unwrap();
    if let Some(map) = guard.as_mut() {
        map.retain(|uid, _| live.contains(uid));
    }
}

/// Latest captured `(amp, freq)` for an Audio Stream Haptics node, or zeros if
/// the node has no open capture this tick. Cheap read; safe from the eval thread.
pub fn latest_params(uid: usize) -> LoopbackParams {
    PARAMS
        .read()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&uid).copied())
        .unwrap_or_default()
}

// ── log-band audio spectrum (read by the spectrum view + multi-band engine) ─────

static SPECTRUM: RwLock<Option<HashMap<usize, [f32; N_BANDS]>>> = RwLock::new(None);

#[cfg(windows)]
pub(crate) fn set_spectrum(uid: usize, bands: [f32; N_BANDS]) {
    let mut guard = SPECTRUM.write().unwrap();
    guard.get_or_insert_with(HashMap::new).insert(uid, bands);
}

#[cfg(windows)]
pub(crate) fn retain_spectrum(live: &std::collections::HashSet<usize>) {
    let mut guard = SPECTRUM.write().unwrap();
    if let Some(map) = guard.as_mut() {
        map.retain(|uid, _| live.contains(uid));
    }
}

/// Latest log-band audio spectrum for an Audio Stream Haptics node, or all-zero if
/// no capture is open. Cheap copy; read by the node's spectrum view and the engine.
pub fn latest_spectrum(uid: usize) -> [f32; N_BANDS] {
    SPECTRUM
        .read()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&uid).copied())
        .unwrap_or([0.0; N_BANDS])
}
