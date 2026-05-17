use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use flexinput_core::Signal;

use crate::eval::{eval_graph_tick, TickOutput};
use crate::graph::ProcessingGraph;
use crate::state::NodeState;

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
    graph: Arc<RwLock<ProcessingGraph>>,
    device_signals: Arc<RwLock<HashMap<(String, String), Signal>>>,
    output: Arc<Mutex<ProcessingOutput>>,
    sink_bus: SinkBus,
    sample_rate: Arc<AtomicU32>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut next_tick = Instant::now();
        let mut state: HashMap<usize, NodeState> = HashMap::new();

        loop {
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
                let graph_snap = graph.read().unwrap().clone();
                let dev_sigs   = device_signals.read().unwrap().clone();

                // Evaluate all catchup ticks first, accumulating scope samples
                // and keeping only the last tick's outputs.  This reduces
                // proc_outputs lock acquisitions from O(ticks) to O(1) per wakeup.
                let mut scope_acc: Vec<(usize, Vec<Option<f32>>)> = Vec::new();
                let mut last_out: Option<TickOutput> = None;
                for _ in 0..ticks {
                    let tick_out = eval_graph_tick(&graph_snap, &mut state, &dev_sigs, dt);
                    scope_acc.extend(tick_out.scope_samples.iter().cloned());
                    last_out = Some(tick_out);
                }

                if let Some(tick_out) = last_out {
                    // Write sink outputs on the fast path (separate lock, no UI contention).
                    *sink_bus.write().unwrap() = tick_out.sink_outputs;

                    // Write display outputs once per wakeup (not once per tick).
                    let mut out = output.lock().unwrap();
                    for sample in scope_acc {
                        if out.scope_pending.len() < MAX_SCOPE_PENDING {
                            out.scope_pending.push(sample);
                        }
                    }
                    out.node_outputs  = tick_out.outputs;
                    out.last_inputs   = tick_out.last_inputs;
                    out.last_outputs  = tick_out.last_outputs;
                }
            }

            // Short sleep to yield the CPU; catches up on the next wakeup.
            thread::sleep(Duration::from_micros(200));
        }
    })
}
