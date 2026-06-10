use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use flexinput_core::Signal;

use crate::eval::{eval_graph_tick, TickOutput};
use crate::graph::ProcessingGraph;
use crate::state::NodeState;

/// Type alias for the atomically-swappable `ProcessingGraph` snapshot.
/// The UI writes new snapshots via `store(Arc::new(g))`; the proc thread
/// reads via `load()` which is a cheap refcount bump — no clone needed.
pub type ArcGraph = Arc<ArcSwap<ProcessingGraph>>;

/// Type alias for the atomically-swappable device-signal map. Same
/// pattern as `ArcGraph` — the I/O thread publishes a fresh map per poll
/// cycle; consumers (proc thread, UI) read by refcount bump.
pub type ArcSignals = Arc<ArcSwap<HashMap<(String, String), Signal>>>;

/// Build a fresh `ArcGraph` initialized with an empty graph.
pub fn new_arc_graph() -> ArcGraph {
    Arc::new(ArcSwap::from_pointee(ProcessingGraph::default()))
}

/// Build a fresh `ArcSignals` initialized with an empty map.
pub fn new_arc_signals() -> ArcSignals {
    Arc::new(ArcSwap::from_pointee(HashMap::new()))
}

/// Default processing rate (Hz). Runtime-tunable via the `sample_rate` atomic
/// passed to `spawn_processing_thread`.
pub const DEFAULT_SAMPLE_RATE: u32 = 2000;

/// Process-global live sample rate. Mirrors the atomic handed to
/// `spawn_processing_thread`, so read-only consumers (oscilloscope window
/// sizing, etc.) don't have to thread the `Arc<AtomicU32>` through every
/// rendering layer. Updated atomically inside the processing loop.
static LIVE_SAMPLE_RATE: AtomicU32 = AtomicU32::new(DEFAULT_SAMPLE_RATE);

/// Read the currently-active processing rate. Cheap relaxed load — safe to
/// call from per-frame UI code.
pub fn current_sample_rate() -> u32 {
    LIVE_SAMPLE_RATE.load(Ordering::Relaxed)
}

/// Measured I/O thread loop rate (Hz). Updated by the device-io loop after
/// each iteration as a rolling EMA. Used as a fallback/global indicator.
static LIVE_IO_RATE: AtomicU32 = AtomicU32::new(0);

/// Read the currently-measured device I/O polling rate (Hz).
pub fn current_io_rate() -> u32 {
    LIVE_IO_RATE.load(Ordering::Relaxed)
}

/// Update the live I/O rate. Called by the device-io thread.
pub fn set_io_rate(hz: u32) {
    LIVE_IO_RATE.store(hz, Ordering::Relaxed);
}

/// Per-device measured event rate (Hz). Populated by the device-io thread
/// from raw event counts per device. UI reads it to display each device's
/// real polling rate in the canvas header.
pub type DeviceRates = std::sync::Arc<std::sync::RwLock<std::collections::HashMap<String, u32>>>;

/// Build a fresh `DeviceRates` handle. Hand to both the device-io thread
/// (writes) and the UI (reads).
pub fn new_device_rates() -> DeviceRates {
    std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()))
}

// ── Per-pin scope tap ────────────────────────────────────────────────────────
//
// Bounded, time-windowed ring of raw samples per (device_id, pin_id). Used by
// the calibration window's oscilloscope so the trace density matches the
// device's actual polling Hz rather than UI repaint Hz. Populated on the I/O
// thread; read at UI repaint rate.

/// One taped sample: timestamp + scalar value (Vec2/Bool/etc. are
/// pre-projected to f32 by the writer to keep the ring compact).
pub type ScopeTapRing = std::collections::VecDeque<(Instant, f32)>;

/// Map of (device_id, pin_id) → ring of recent samples.
pub type ScopeTaps = Arc<RwLock<HashMap<(String, String), ScopeTapRing>>>;

/// Time window the I/O thread retains samples for (ms). Sized to cover
/// the longest scope window the UI offers (5 s) with a small overhang
/// so the scope can render the full window even if a frame is slow.
pub const SCOPE_TAP_RETAIN_MS: u64 = 5500;

/// Hard cap on per-pin ring length (defensive). Sized to comfortably hold
/// 5 s at ~4 kHz polling with headroom.
pub const SCOPE_TAP_MAX_LEN: usize = 32768;

/// Build a fresh `ScopeTaps` handle. The I/O thread writes; the UI reads.
pub fn new_scope_taps() -> ScopeTaps {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Pin names the I/O loop should tap. Kept narrow on purpose — calibration
/// only needs gyro + accel today.
pub const SCOPE_TAP_PINS: &[&str] = &[
    "gyro_x", "gyro_y", "gyro_z",
    "accel_x", "accel_y", "accel_z",
];
/// How many scope samples to buffer before the UI drains them.
const MAX_SCOPE_PENDING: usize = 8192;

// ── Shared state ──────────────────────────────────────────────────────────────

/// Latest outputs from the processing thread, read by the UI each frame.
#[derive(Default)]
pub struct ProcessingOutput {
    /// Latest computed output per (node_uid, output_pin). Excludes device.source.
    pub node_outputs: HashMap<(usize, usize), Option<Signal>>,
    /// Latest input signals per display/response_curve node for readout rendering.
    pub last_inputs: HashMap<usize, Vec<Option<Signal>>>,
    /// Latest output signals per twoway_response_curve node (blended engine output for UI).
    pub last_outputs: HashMap<usize, Vec<Option<Signal>>>,
    /// Accumulated scope samples not yet drained by the UI thread.
    pub scope_pending: Vec<(usize, Vec<Option<f32>>)>,
}

/// Separate lock for sink routing outputs — read by the I/O thread at 500 Hz,
/// written by the processing thread at 2 kHz. Kept apart from ProcessingOutput
/// so the I/O thread never contends on the UI/processing mutex.
pub type SinkBus = Arc<RwLock<HashMap<(String, String), Signal>>>;

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Spawns the processing thread and returns the shared state handles.
/// The caller keeps the `Arc` references; the thread holds clones.
///
/// `sample_rate` is read at the top of each wakeup so the user can retune
/// the processing rate live without restarting the thread.
pub fn spawn_processing_thread(
    graph: ArcGraph,
    device_signals: ArcSignals,
    output: Arc<Mutex<ProcessingOutput>>,
    sink_bus: SinkBus,
    sample_rate: Arc<AtomicU32>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Raise this thread above the UI/render threads. The engine ticks at
        // 2 kHz and feeds the I/O thread's sink bus; if a busy render frame
        // delays the tick, fresh output is late and the user feels input lag.
        // ABOVE_NORMAL (not TIME_CRITICAL): the loop runs near-continuously,
        // so TIME_CRITICAL could starve the UI — we want input ahead of
        // rendering, not the UI frozen. The I/O thread (TIME_CRITICAL) remains
        // the highest-priority leg as the hard real-time output path.
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
            };
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
        }

        let mut next_tick = Instant::now();
        let mut state: HashMap<usize, NodeState> = HashMap::new();
        // Persistent scratch reused across ticks (cleared in-place at the
        // top of every `eval_graph_tick` call). Avoids 5 HashMap reallocs
        // per tick — significant at 2 kHz with an empty graph.
        let mut tick_out: TickOutput = TickOutput::default();
        // Persistent scope-sample accumulator across the catchup loop.
        // Pre-allocated outside the hot loop so it grows once and is
        // reused thereafter.
        let mut scope_acc: Vec<(usize, Vec<Option<f32>>)> = Vec::new();

        loop {
            puffin::GlobalProfiler::lock().new_frame();
            puffin::profile_scope!("proc_thread_iter");
            let now = Instant::now();

            // Re-read sample rate each wakeup so live retunes apply immediately.
            // Clamp defensively in case settings.json holds a garbage value.
            let sr = sample_rate.load(Ordering::Relaxed).clamp(100, 16_000);
            LIVE_SAMPLE_RATE.store(sr, Ordering::Relaxed);
            let dt: f32 = 1.0 / sr as f32;
            let interval = Duration::from_nanos(1_000_000_000 / sr as u64);

            // How many ticks have elapsed since we last processed?
            let mut ticks = 0u32;
            while next_tick <= now {
                next_tick += interval;
                ticks += 1;
            }
            // Cap catchup to ~8 ms to avoid spiral-of-death on heavy load.
            let ticks = ticks.min(16);

            if ticks > 0 {
                // Refcount-bump reads — no cloning of graph or signal map.
                // The Arc<…> handles point at whatever the publishers most
                // recently stored. Stable across the catchup loop because
                // each `load()` returns a snapshot held by this scope.
                let graph_snap = {
                    puffin::profile_scope!("graph_load");
                    graph.load_full()
                };
                let dev_sigs = {
                    puffin::profile_scope!("dev_sigs_load");
                    device_signals.load_full()
                };

                scope_acc.clear();

                {
                    puffin::profile_scope!("eval_ticks");
                    for _ in 0..ticks {
                        eval_graph_tick(&graph_snap, &mut state, &dev_sigs, dt, &mut tick_out);
                        // Drain scope samples each tick — eval_graph_tick
                        // clears tick_out on entry, so we must move (not
                        // clone) the samples here before the next call.
                        scope_acc.append(&mut tick_out.scope_samples);
                    }
                }

                // tick_out now holds the LAST tick's outputs/inputs/sinks.
                {
                    puffin::profile_scope!("write_sink_bus");
                    *sink_bus.write().unwrap() = tick_out.sink_outputs.clone();
                }
                {
                    puffin::profile_scope!("write_proc_outputs");
                    let mut out = output.lock().unwrap();
                    for sample in scope_acc.drain(..) {
                        if out.scope_pending.len() < MAX_SCOPE_PENDING {
                            out.scope_pending.push(sample);
                        }
                    }
                    // Swap instead of clone: hand our just-filled maps
                    // to the UI and take back whatever was there (now
                    // an empty default since the UI drained it). Saves
                    // 3 HashMap clones per UI-frame at large patches.
                    // tick_out is cleared at the top of the next
                    // eval_graph_tick call so the swapped-in empties
                    // don't matter — but if the UI was slow this round
                    // (still holding stale data), we'd overwrite a
                    // non-empty map. Clear after-swap to guarantee.
                    std::mem::swap(&mut out.node_outputs,  &mut tick_out.outputs);
                    std::mem::swap(&mut out.last_inputs,   &mut tick_out.last_inputs);
                    std::mem::swap(&mut out.last_outputs,  &mut tick_out.last_outputs);
                    // After the swap, tick_out holds whatever the UI
                    // hadn't drained yet. Clear so the next eval starts
                    // from empty (eval_graph_tick also clears, but doing
                    // it here lets the allocations free sooner).
                    tick_out.outputs.clear();
                    tick_out.last_inputs.clear();
                    tick_out.last_outputs.clear();
                }
            }

            // Sleep until the next tick deadline, capped at 1 ms so we still
            // respond to a sample-rate change within ~1 ms. The old fixed
            // 200 µs spin was a major CPU sink: at 2 kHz target (500 µs
            // interval), waking every 200 µs to find 0 or 1 ticks pending
            // burned ~5k wake-ups/sec for almost no real work.
            //
            // Now: if we're already behind (next_tick <= now), don't
            // sleep at all — head straight back into the catchup loop.
            // If we're ahead, sleep the remaining gap so the wakeup
            // lands right when the next tick is due.
            let now2 = Instant::now();
            if next_tick > now2 {
                let gap = next_tick - now2;
                let sleep_for = gap.min(Duration::from_millis(1));
                thread::sleep(sleep_for);
            }
        }
    })
}
