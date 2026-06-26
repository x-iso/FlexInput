//! Lifecycle manager for per-node WASAPI loopback captures.
//!
//! The audio-capture thread ([`crate::loopback_haptic::LoopbackHaptic`]) is a
//! long-lived OS thread; it must NOT be created/destroyed by the engine's
//! stateless per-tick eval. Instead the processing thread owns one
//! [`LoopbackManager`] and calls [`LoopbackManager::reconcile`] once per wakeup
//! with the current set of Audio Stream Haptics nodes; the manager opens /
//! replaces / drops captures as targets change, and publishes each node's latest
//! `(amp, freq)` into a process-global map the eval block reads via
//! [`latest_params`].
//!
//! The reconcile API takes only primitives (`node uid` + [`LoopbackTarget`]), so
//! this crate stays a dependency leaf — the engine extracts the target list from
//! its graph snapshot and hands it over.

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::loopback_haptic::{LoopbackHaptic, LoopbackParams, LoopbackTarget, ScopePoint};
use crate::spectrum::N_BANDS;

/// What a node wants to capture, in a form the engine can build without touching
/// PIDs: a specific process by NAME (re-resolved to a live PID here), the
/// foreground app, or the whole system mix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureRequest {
    /// Capture the process whose exe name matches (case-insensitive). The name is
    /// what gets persisted; the PID is resolved live (and re-resolved if the app
    /// restarts under a new PID).
    ProcessName { name: String, include_tree: bool },
    /// Capture whichever process owns the foreground window right now.
    Focused { include_tree: bool },
    /// Capture the system mix (default render endpoint loopback).
    System,
}

/// The concrete target a request resolved to, used to detect when a running
/// capture must be torn down and reopened (e.g. focused app changed, or a named
/// process restarted under a new PID).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResolvedTarget {
    Process { pid: u32, include_tree: bool },
    System,
    /// Wanted a process but none is currently running — no capture open.
    Unresolved,
}

struct Entry {
    request: CaptureRequest,
    resolved: ResolvedTarget,
    capture: Option<LoopbackHaptic>,
    /// Throttle re-resolution of name→PID / foreground PID (it enumerates procs).
    last_resolve: Instant,
}

/// Owned by the processing thread; reconciles captures against the live node set.
#[derive(Default)]
pub struct LoopbackManager {
    entries: HashMap<usize, Entry>,
}

/// How often to re-resolve a process name / foreground PID. Process enumeration
/// is a Toolhelp snapshot — cheap, but no need to do it every wakeup.
const RESOLVE_INTERVAL: Duration = Duration::from_millis(500);

impl LoopbackManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconcile the live captures against `requests` (the current Audio Stream
    /// Haptics nodes, keyed by node uid). Opens new captures, reopens on target
    /// change, drops captures for nodes no longer present, and publishes each
    /// node's latest params to the global map. Call once per proc-thread wakeup.
    ///
    /// `release_ms` + `volume` are applied live to the open capture (no reopen) so
    /// dragging the node's Release / Volume sliders retunes without dropping audio.
    /// Volume is input gain BEFORE detection (lets a hot source be dialed back).
    pub fn reconcile(&mut self, requests: &[(usize, CaptureRequest, f32, f32, f32)]) {
        let now = Instant::now();

        // Drop entries whose node disappeared.
        let live: std::collections::HashSet<usize> = requests.iter().map(|(uid, _, _, _, _)| *uid).collect();
        self.entries.retain(|uid, _| live.contains(uid));

        for (uid, request, release_ms, volume, crossover_pos) in requests {
            let entry = self.entries.entry(*uid).or_insert_with(|| Entry {
                request: request.clone(),
                resolved: ResolvedTarget::Unresolved,
                capture: None,
                last_resolve: now - RESOLVE_INTERVAL, // force first resolve
            });

            // If the request itself changed (mode/name/tree), force a re-resolve.
            let request_changed = &entry.request != request;
            if request_changed {
                entry.request = request.clone();
            }

            // Re-resolve the desired concrete target, throttled (but immediately
            // on a request change). System mode needs no enumeration.
            let due = request_changed || now.duration_since(entry.last_resolve) >= RESOLVE_INTERVAL;
            if due {
                entry.last_resolve = now;
                let desired = resolve(request);
                if desired != entry.resolved {
                    // Target changed → tear down + reopen (drop joins the thread).
                    entry.capture = None;
                    entry.resolved = desired;
                    entry.capture = open(desired);
                }
            }

            // Apply the live Release + Volume + Crossover settings (no reopen).
            if let Some(cap) = entry.capture.as_ref() {
                cap.set_release_ms(*release_ms);
                cap.set_volume(*volume);
                cap.set_crossover_pos(*crossover_pos);
            }

            // Publish latest params (or zeros when no capture is open).
            let params = entry
                .capture
                .as_ref()
                .map(|c| c.params())
                .unwrap_or_default();
            set_params(*uid, params);

            // Publish the EF oscilloscope ring for the node body to draw.
            let scope = entry
                .capture
                .as_ref()
                .map(|c| c.scope_snapshot())
                .unwrap_or_default();
            set_scope(*uid, scope);

            // Publish the log-band audio spectrum for the spectrum view + bands.
            let spectrum = entry
                .capture
                .as_ref()
                .map(|c| c.spectrum_snapshot())
                .unwrap_or([0.0; N_BANDS]);
            set_spectrum(*uid, spectrum);
        }

        // Clear published params for nodes that vanished this round.
        retain_params(&live);
        retain_scope(&live);
        retain_spectrum(&live);
    }
}

fn resolve(request: &CaptureRequest) -> ResolvedTarget {
    use crate::loopback_haptic::process;
    match request {
        CaptureRequest::System => ResolvedTarget::System,
        CaptureRequest::ProcessName { name, include_tree } => match process::pid_for_name(name) {
            Some(pid) => ResolvedTarget::Process { pid, include_tree: *include_tree },
            None => ResolvedTarget::Unresolved,
        },
        CaptureRequest::Focused { include_tree } => match process::foreground_pid() {
            Some(pid) => ResolvedTarget::Process { pid, include_tree: *include_tree },
            None => ResolvedTarget::Unresolved,
        },
    }
}

fn open(target: ResolvedTarget) -> Option<LoopbackHaptic> {
    match target {
        ResolvedTarget::System => LoopbackHaptic::open_system(),
        ResolvedTarget::Process { pid, include_tree } => {
            LoopbackHaptic::open_target(LoopbackTarget::Process { pid, include_tree })
        }
        ResolvedTarget::Unresolved => None,
    }
}

// ── process-global published params (read by the eval block) ───────────────────

static PARAMS: RwLock<Option<HashMap<usize, LoopbackParams>>> = RwLock::new(None);

fn set_params(uid: usize, params: LoopbackParams) {
    let mut guard = PARAMS.write().unwrap();
    guard.get_or_insert_with(HashMap::new).insert(uid, params);
}

fn retain_params(live: &std::collections::HashSet<usize>) {
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

// ── EF oscilloscope ring (read by the node body) ───────────────────────────────

static SCOPE: RwLock<Option<HashMap<usize, Vec<ScopePoint>>>> = RwLock::new(None);

fn set_scope(uid: usize, points: Vec<ScopePoint>) {
    let mut guard = SCOPE.write().unwrap();
    guard.get_or_insert_with(HashMap::new).insert(uid, points);
}

fn retain_scope(live: &std::collections::HashSet<usize>) {
    let mut guard = SCOPE.write().unwrap();
    if let Some(map) = guard.as_mut() {
        map.retain(|uid, _| live.contains(uid));
    }
}

/// Latest EF oscilloscope ring (oldest → newest) for an Audio Stream Haptics node,
/// or empty if no capture is open. Cheap-ish clone; called once per UI frame.
pub fn latest_scope(uid: usize) -> Vec<ScopePoint> {
    SCOPE
        .read()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&uid).cloned())
        .unwrap_or_default()
}

// ── log-band audio spectrum (read by the spectrum view + multi-band engine) ─────

static SPECTRUM: RwLock<Option<HashMap<usize, [f32; N_BANDS]>>> = RwLock::new(None);

fn set_spectrum(uid: usize, bands: [f32; N_BANDS]) {
    let mut guard = SPECTRUM.write().unwrap();
    guard.get_or_insert_with(HashMap::new).insert(uid, bands);
}

fn retain_spectrum(live: &std::collections::HashSet<usize>) {
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
