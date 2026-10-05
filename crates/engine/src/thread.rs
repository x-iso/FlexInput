use std::collections::{HashMap, HashSet};
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

/// Recursively collect WASAPI loopback capture requests for every Audio Stream
/// Haptics node in the graph, descending into inline sub-patches. Each request is
/// keyed by the node's EFFECTIVE uid — `node_uid` at the top level, or
/// `namespaced_uid(outer, node_uid)` inside a sub-patch — so it matches the uid
/// `audio_stream_haptics_publish` uses to read the capture's latest params/spectrum.
#[cfg(windows)]
fn collect_loopback_reqs(
    nodes: &[crate::graph::NodeSnap],
    outer_uid: usize,
    nested: bool,
    out: &mut Vec<(usize, flexinput_devices::loopback_manager::CaptureRequest, f32, f32, f32)>,
) {
    for node in nodes {
        let uid = if nested {
            crate::eval::namespaced_uid(outer_uid, node.node_uid)
        } else {
            node.node_uid
        };
        if node.module_id == crate::eval::AUDIO_STREAM_HAPTICS_ID {
            if let Some(req) = crate::eval::loopback_request_from_params(&node.params) {
                let release_ms = node.params.get("asth_release")
                    .and_then(|v| v.as_f64()).unwrap_or(30.0) as f32;
                let volume = node.params.get("asth_volume")
                    .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                let crossover_hz = node.params.get("asth_crossover")
                    .and_then(|v| v.as_f64()).unwrap_or(250.0) as f32;
                let crossover_pos = crate::eval::crossover_hz_to_pos(crossover_hz);
                out.push((uid, req, release_ms, volume, crossover_pos));
            }
        }
        if let Some(ref sg) = node.inline_subgraph {
            collect_loopback_reqs(&sg.graph.nodes, uid, true, out);
        }
    }
}

/// Recursively collect network-node configs across the whole graph, descending
/// into inline sub-patches. Each config is keyed by the node's EFFECTIVE uid
/// (raw at top level, namespaced inside a sub-patch), matching what the eval
/// arms use to publish/read frames — so a network node nested in a sub-patch
/// still binds its socket. Unlike loopback, this is NOT windows-gated:
/// networking works on every platform.
fn collect_net_reqs(
    nodes: &[crate::graph::NodeSnap],
    outer_uid: usize,
    nested: bool,
    out: &mut Vec<(usize, flexinput_net::NetNodeConfig)>,
) {
    for node in nodes {
        let uid = if nested {
            crate::eval::namespaced_uid(outer_uid, node.node_uid)
        } else {
            node.node_uid
        };
        if let Some(cfg) = crate::eval::net_config_from_params(&node.module_id, &node.params) {
            out.push((uid, cfg));
        }
        if let Some(ref sg) = node.inline_subgraph {
            collect_net_reqs(&sg.graph.nodes, uid, true, out);
        }
    }
}

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

/// Separate lock for sink routing outputs — read by the I/O thread at the
/// polling rate (default 500 Hz), written by the processing thread at the
/// sample rate (default 2 kHz). Kept apart from ProcessingOutput
/// so the I/O thread never contends on the UI/processing mutex.
pub type SinkBus = Arc<RwLock<HashMap<(String, String), Signal>>>;

/// Sink pins that carry a one-shot DISPLACEMENT (pixels moved this tick) rather
/// than a level. Every other pin is a state the I/O thread may sample at its own
/// rate; these are not. The engine ticks at the sample rate and the I/O thread
/// reads the bus at the polling rate, so publishing only the latest tick's
/// displacement kept 1 tick in (sample rate / polling rate) — 1 in 4 at the
/// 2 kHz / 500 Hz defaults — and a JSM config aimed 4x slower than in JSM. They
/// are therefore banked across ticks here and drained by the reader instead.
pub fn is_displacement_pin(pin: &str) -> bool {
    matches!(pin, "mouse_move" | "mouse_move_x" | "mouse_move_y"
        | "scroll_move_x" | "scroll_move_y")
}

/// Bool sink pins delivered as a COUNT of off→on edges instead of a level: one
/// scroll notch per gate cycle. Sampled as a level, a held gate scrolled once per
/// I/O read (~500 notches/s at the defaults) and a pulse shorter than a read
/// could vanish. Counted on every engine tick and banked like a displacement, a
/// press is exactly one notch however long it is held. On the bus these pins
/// carry `Signal::Float(edges)`.
pub fn is_click_pin(pin: &str) -> bool {
    matches!(pin, "scroll_up" | "scroll_down" | "scroll_left" | "scroll_right")
}

/// Pins banked across ticks and drained by the reader rather than sampled.
fn is_banked_pin(pin: &str) -> bool {
    is_displacement_pin(pin) || is_click_pin(pin)
}

fn add_displacement(into: &mut HashMap<(String, String), Signal>, key: &(String, String), v: Signal) {
    match (into.get_mut(key), v) {
        (Some(Signal::Vec2(a)), Signal::Vec2(b)) => *a += b,
        (Some(Signal::Float(a)), Signal::Float(b)) => *a += b,
        // First sighting, or a type that changed under us: the new value stands.
        _ => { into.insert(key.clone(), v); }
    }
}

/// Add one tick's displacement pins and click-pin rising edges into `bank`.
/// `levels` is each click pin's level on the previous tick.
fn bank_displacement(
    bank: &mut HashMap<(String, String), Signal>,
    levels: &mut HashMap<(String, String), bool>,
    sinks: &HashMap<(String, String), Signal>,
) {
    for (k, &v) in sinks {
        if is_displacement_pin(&k.1) {
            add_displacement(bank, k, v);
        } else if is_click_pin(&k.1) {
            let on = matches!(v, Signal::Bool(true));
            let was = match levels.get_mut(k) {
                Some(l) => std::mem::replace(l, on),
                None => { levels.insert(k.clone(), on); false }
            };
            if on && !was {
                add_displacement(bank, k, Signal::Float(1.0));
            }
        }
    }
    // A pin that dropped out of the outputs (unwired) reads as released.
    levels.retain(|k, _| sinks.contains_key(k));
}

/// Replace the bus with the latest tick's `sinks`, except that each banked pin
/// becomes everything banked since the last publish plus whatever the reader
/// has not drained yet. Empties `bank`.
fn publish_sink_bus(
    bus: &mut HashMap<(String, String), Signal>,
    sinks: &HashMap<(String, String), Signal>,
    bank: &mut HashMap<(String, String), Signal>,
) {
    let undrained: Vec<((String, String), Signal)> = bus
        .iter()
        .filter(|(k, _)| is_banked_pin(&k.1))
        .map(|(k, &v)| (k.clone(), v))
        .collect();
    bus.clone_from(sinks);
    bus.retain(|k, _| !is_banked_pin(&k.1));
    for (k, v) in bank.drain() {
        add_displacement(bus, &k, v);
    }
    for (k, v) in undrained {
        add_displacement(bus, &k, v);
    }
}

/// Read the bus for the sinks: a copy of it, with the banked pins zeroed in the
/// bus afterwards so the same movement or notch is never delivered twice. The
/// I/O thread calls this once per iteration.
pub fn drain_sink_bus(bus: &SinkBus) -> HashMap<(String, String), Signal> {
    let mut bus = bus.write().unwrap();
    let out = bus.clone();
    for (k, v) in bus.iter_mut() {
        if is_banked_pin(&k.1) {
            *v = match *v {
                Signal::Vec2(_) => Signal::Vec2(Default::default()),
                Signal::Float(_) => Signal::Float(0.0),
                other => other,
            };
        }
    }
    out
}

/// UI→engine source-block channel. The UI writes the set of physical device
/// `(device_id, pin)` pairs to suppress from the game while the config overlay
/// is open; the processing thread unions it into `state[MACRO_CARRY_UID].source_block`
/// before every tick, so the existing tick-start apply zeroes them in `dev_sigs`
/// (the same path the Virtual Menu's source-block uses). Empty = nothing blocked.
///
/// This is deliberately separate from any node-published block: it lets the UI
/// suppress inputs it is consuming for on-screen navigation (config overlay,
/// M3) without a node in the graph, and the config-overlay nav reads the RAW
/// `proc_device_signals` snapshot, so blocking here never starves the nav.
pub type UiSourceBlock = Arc<RwLock<HashSet<(String, String)>>>;

/// Fresh, empty [`UiSourceBlock`] handle for the UI to share with the thread.
pub fn new_ui_source_block() -> UiSourceBlock {
    Arc::new(RwLock::new(HashSet::new()))
}

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
    ui_source_block: UiSourceBlock,
    // Same channel, second writer: the gamepad-shortcut watcher's owned
    // buttons (a chord's buttons must not also reach the game). Kept separate
    // so neither writer clobbers the other's set.
    shortcut_source_block: UiSourceBlock,
    // Bool pins the shortcut watcher is replaying to the game: a held-back
    // Home tap no shortcut used. Shown pressed, and let through the shortcut
    // block (not the UI's — an open config overlay still withholds them).
    shortcut_replay: UiSourceBlock,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Raise this thread above the UI/render threads. The engine ticks at
        // the sample rate (default 2 kHz) and feeds the I/O thread's sink bus;
        // if a busy render frame delays the tick, fresh output is late and
        // the user feels input lag.
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
        // Owns the per-node WASAPI loopback captures for Audio Stream Haptics
        // nodes. Reconciled once per wakeup against the live graph; eval reads the
        // published params. Windows-only (loopback is a WASAPI feature).
        #[cfg(windows)]
        let mut loopback_mgr = flexinput_devices::loopback_manager::LoopbackManager::new();
        #[cfg(windows)]
        let mut loopback_reqs: Vec<(usize, flexinput_devices::loopback_manager::CaptureRequest, f32, f32, f32)> = Vec::new();
        // Owns the per-node network sockets for Network Send/Receive nodes.
        // Reconciled once per wakeup against the live graph; eval moves frames
        // through flexinput_net's global slots. All platforms.
        let mut net_mgr = flexinput_net::NetManager::new();
        let mut net_reqs: Vec<(usize, flexinput_net::NetNodeConfig)> = Vec::new();
        // Persistent scratch reused across ticks (cleared in-place at the
        // top of every `eval_graph_tick` call). Avoids 5 HashMap reallocs
        // per tick — significant at 2 kHz+ tick rates with an empty graph.
        let mut tick_out: TickOutput = TickOutput::default();
        // Persistent scope-sample accumulator across the catchup loop.
        // Pre-allocated outside the hot loop so it grows once and is
        // reused thereafter.
        let mut scope_acc: Vec<(usize, Vec<Option<f32>>)> = Vec::new();
        // Displacement pins summed over the catch-up loop (see
        // `is_displacement_pin`); emptied into the sink bus once per wakeup.
        let mut disp_bank: HashMap<(String, String), Signal> = HashMap::new();
        // Previous-tick level of each click pin (see `is_click_pin`).
        let mut click_levels: HashMap<(String, String), bool> = HashMap::new();

        // High-resolution waiter for the sub-tick sleep below — precise without
        // raising the global timer resolution (see hr_timer). The device-I/O
        // thread no longer calls timeBeginPeriod, so a plain thread::sleep here
        // would round up to ~15.6 ms and collapse the tick rate.
        let waiter = crate::hr_timer::HrWaiter::new();

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
                // A replayed shortcut-leader tap: the button is physically up
                // by now, so press it on our copy. Only clones the map for the
                // ~0.1 s a replay lasts. Read once per wakeup — the block below
                // lets the same keys through.
                let replay: HashSet<(String, String)> = shortcut_replay
                    .read()
                    .map(|s| s.clone())
                    .unwrap_or_default();
                let dev_sigs = if replay.is_empty() {
                    dev_sigs
                } else {
                    let mut m = (*dev_sigs).clone();
                    for k in &replay {
                        m.insert(k.clone(), Signal::Bool(true));
                    }
                    Arc::new(m)
                };

                // Reconcile WASAPI loopback captures for Audio Stream Haptics
                // nodes once per wakeup (before the eval ticks read their params).
                #[cfg(windows)]
                {
                    puffin::profile_scope!("loopback_reconcile");
                    loopback_reqs.clear();
                    // Collect ASTH capture requests across the WHOLE graph, recursing
                    // into inline sub-patches. Each request is keyed by the node's
                    // EFFECTIVE uid (raw at top level, namespaced inside a sub-patch),
                    // matching what `audio_stream_haptics_publish` reads via
                    // `latest_params(uid)`. Without recursion, ASTH nested in a
                    // sub-patch never got a capture → it was silent there.
                    collect_loopback_reqs(&graph_snap.nodes, 0, false, &mut loopback_reqs);
                    loopback_mgr.reconcile(&loopback_reqs);
                }

                // Reconcile network sockets for Network Send/Receive nodes once
                // per wakeup (before eval reads/writes their frames). Config-diff
                // driven, so the UI's every-frame republish never flaps sockets.
                {
                    puffin::profile_scope!("net_reconcile");
                    net_reqs.clear();
                    collect_net_reqs(&graph_snap.nodes, 0, false, &mut net_reqs);
                    net_mgr.reconcile(&net_reqs);
                }

                scope_acc.clear();

                // Snapshot the UI source-block once per wakeup (cheap; usually
                // empty). Re-injected before EACH tick below, because the
                // tick-end pass clears `source_block` and repopulates it from
                // node-published `__src_block__:` keys — so a once-per-wakeup
                // injection would only cover the first tick of a catch-up burst,
                // and the LAST tick (whose sink outputs are published) could
                // leak the blocked input to the game.
                let mut ui_block: Vec<(String, String)> = ui_source_block
                    .read()
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default();
                if let Ok(s) = shortcut_source_block.read() {
                    ui_block.extend(s.iter().filter(|k| !replay.contains(*k)).cloned());
                }

                {
                    puffin::profile_scope!("eval_ticks");
                    for _ in 0..ticks {
                        if !ui_block.is_empty() {
                            let carry = state.entry(crate::eval::MACRO_CARRY_UID).or_default();
                            for k in &ui_block {
                                carry.source_block.insert(k.clone());
                            }
                        }
                        eval_graph_tick(&graph_snap, &mut state, &dev_sigs, dt, &mut tick_out);
                        // Drain scope samples each tick — eval_graph_tick
                        // clears tick_out on entry, so we must move (not
                        // clone) the samples here before the next call.
                        scope_acc.append(&mut tick_out.scope_samples);
                        bank_displacement(&mut disp_bank, &mut click_levels, &tick_out.sink_outputs);
                    }
                }

                // tick_out now holds the LAST tick's outputs/inputs/sinks; the
                // displacement pins carry every tick's share instead.
                {
                    puffin::profile_scope!("write_sink_bus");
                    publish_sink_bus(&mut sink_bus.write().unwrap(), &tick_out.sink_outputs, &mut disp_bank);
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
            // 200 µs spin was a major CPU sink: at the default 2 kHz target (500 µs
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
                waiter.wait(sleep_for);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;

    fn key(pin: &str) -> (String, String) {
        ("virtual.keymouse:0".to_string(), pin.to_string())
    }

    fn tick(disp: Vec2, lmb: bool) -> HashMap<(String, String), Signal> {
        HashMap::from([
            (key("mouse_move"), Signal::Vec2(disp)),
            (key("mouse_move_x"), Signal::Float(disp.x)),
            (key("mouse_left"), Signal::Bool(lmb)),
        ])
    }

    /// Run `engine_ticks` ticks of one displacement each, publishing every
    /// `ticks_per_wakeup` and draining every `ticks_per_read`; return what the
    /// reader was handed in total.
    fn deliver(engine_ticks: usize, ticks_per_wakeup: usize, ticks_per_read: usize, per_tick: Vec2) -> (Vec2, f32) {
        let bus: SinkBus = Arc::new(RwLock::new(HashMap::new()));
        let mut bank = HashMap::new();
        let mut levels = HashMap::new();
        let (mut got, mut got_x) = (Vec2::ZERO, 0.0);
        for t in 1..=engine_ticks {
            let last = tick(per_tick, true);
            bank_displacement(&mut bank, &mut levels, &last);
            if t % ticks_per_wakeup == 0 {
                publish_sink_bus(&mut bus.write().unwrap(), &last, &mut bank);
            }
            if t % ticks_per_read == 0 {
                let seen = drain_sink_bus(&bus);
                if let Some(Signal::Vec2(v)) = seen.get(&key("mouse_move")) { got += *v; }
                got_x += seen.get(&key("mouse_move_x")).map(|s| s.as_float()).unwrap_or(0.0);
                assert_eq!(seen.get(&key("mouse_left")), Some(&Signal::Bool(true)), "a level pin passes as it stands");
            }
        }
        (got, got_x)
    }

    /// The reported bug: the engine at 2 kHz, the I/O thread at 500 Hz. The
    /// reader must see all four ticks' movement, not the last one's.
    #[test]
    fn displacement_survives_a_slower_reader() {
        let (got, got_x) = deliver(400, 1, 4, Vec2::new(0.5, -0.25));
        assert!((got - Vec2::new(200.0, -100.0)).length() < 1e-3, "{got}");
        assert!((got_x - 200.0).abs() < 1e-3, "{got_x}");
    }

    /// A catch-up burst runs several ticks per wakeup and publishes once.
    #[test]
    fn displacement_survives_a_catch_up_burst() {
        let (got, _) = deliver(400, 8, 8, Vec2::new(1.0, 0.0));
        assert!((got.x - 400.0).abs() < 1e-3, "{got}");
    }

    /// A reader faster than the engine must not deliver one tick twice.
    #[test]
    fn displacement_is_not_delivered_twice_to_a_faster_reader() {
        let bus: SinkBus = Arc::new(RwLock::new(HashMap::new()));
        let mut bank = HashMap::new();
        let last = tick(Vec2::new(3.0, 0.0), false);
        bank_displacement(&mut bank, &mut HashMap::new(), &last);
        publish_sink_bus(&mut bus.write().unwrap(), &last, &mut bank);
        let first = drain_sink_bus(&bus);
        let second = drain_sink_bus(&bus);
        assert_eq!(first.get(&key("mouse_move")), Some(&Signal::Vec2(Vec2::new(3.0, 0.0))));
        assert_eq!(second.get(&key("mouse_move")), Some(&Signal::Vec2(Vec2::ZERO)));
    }

    #[test]
    fn only_the_move_pins_are_displacements() {
        for pin in ["mouse_move", "mouse_move_x", "mouse_move_y", "scroll_move_x", "scroll_move_y"] {
            assert!(is_displacement_pin(pin), "{pin}");
        }
        for pin in ["mouse", "mouse_x", "right_stick", "scroll_up", "mouse_left", "trackpad_scroll_y"] {
            assert!(!is_displacement_pin(pin), "{pin}");
        }
    }

    /// Drive `scroll_up` with `levels` (one per engine tick), publishing every
    /// `ticks_per_wakeup` and draining every `ticks_per_read`; return the notches
    /// the reader was handed.
    fn notches(levels: &[bool], ticks_per_wakeup: usize, ticks_per_read: usize) -> f32 {
        let bus: SinkBus = Arc::new(RwLock::new(HashMap::new()));
        let (mut bank, mut prev) = (HashMap::new(), HashMap::new());
        let mut got = 0.0;
        for (i, &on) in levels.iter().enumerate() {
            let t = i + 1;
            let last = HashMap::from([(key("scroll_up"), Signal::Bool(on))]);
            bank_displacement(&mut bank, &mut prev, &last);
            if t % ticks_per_wakeup == 0 {
                publish_sink_bus(&mut bus.write().unwrap(), &last, &mut bank);
            }
            if t % ticks_per_read == 0 {
                got += drain_sink_bus(&bus).get(&key("scroll_up")).map(|s| s.as_float()).unwrap_or(0.0);
            }
        }
        got
    }

    /// A held gate is one notch, not one per read.
    #[test]
    fn held_scroll_gate_is_one_notch() {
        assert_eq!(notches(&[true; 400], 1, 4), 1.0);
    }

    /// Each off→on cycle is one notch, even when the reader is slower than the
    /// cycles and a pulse lasts a single engine tick.
    #[test]
    fn every_scroll_gate_cycle_is_one_notch() {
        let levels: Vec<bool> = (0..400).map(|t| t % 2 == 0).collect();
        assert_eq!(notches(&levels, 4, 8), 200.0);
    }

    /// A gate that leaves the outputs (unwired) and comes back held is a new press.
    #[test]
    fn unwired_scroll_gate_reads_as_released() {
        let (mut bank, mut prev) = (HashMap::new(), HashMap::new());
        let held = HashMap::from([(key("scroll_up"), Signal::Bool(true))]);
        bank_displacement(&mut bank, &mut prev, &held);
        bank_displacement(&mut bank, &mut prev, &HashMap::new());
        bank_displacement(&mut bank, &mut prev, &held);
        assert_eq!(bank.get(&key("scroll_up")), Some(&Signal::Float(2.0)));
    }
}
