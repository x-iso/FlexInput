use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use flexinput_core::{BusMap, Signal, SigMap};

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

/// Engine passes that ran at least one tick, and the ticks they ran, since
/// start. The engine publishes its outputs once per pass, so passes/s is how
/// often the sink pins can change; ticks/s below the sample rate means the
/// engine is dropping catch-up ticks. The UI differences these for a live rate.
static ENGINE_PASSES: AtomicU64 = AtomicU64::new(0);
static ENGINE_TICKS: AtomicU64 = AtomicU64::new(0);

/// `(passes, ticks)` run since start. See [`ENGINE_PASSES`].
pub fn engine_counters() -> (u64, u64) {
    (ENGINE_PASSES.load(Ordering::Relaxed), ENGINE_TICKS.load(Ordering::Relaxed))
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

/// Which ticks are due at a wake-up. Ticks are spaced by the sample rate and the
/// wake-ups by the polling rate, on separate clocks. At equal rates, ticks falling
/// due right where the polls land would make jitter give one poll no tick and the
/// next two: an output a whole period late, every so often. So a tick counts from
/// half an interval before it's due, and the schedule drifts onto the wake-ups,
/// which puts that edge as far from them as it can be. Only the phase moves: the
/// pull is too gentle to hold a rate more than about 1% off the wake-ups' (ticks
/// carry a fixed dt, so that would be time running fast), and it only acts while
/// every wake-up gets ticks, since faster wake-ups always find the next tick ahead.
#[derive(Default)]
struct TickClock {
    next: Option<Instant>,
    interval: Duration,
    ticked_last: bool,
}

impl TickClock {
    fn due(&mut self, now: Instant, interval: Duration) -> u32 {
        // A new rate starts its schedule here.
        let mut next = match self.next {
            Some(next) if self.interval == interval => next,
            _ => now,
        };
        self.interval = interval;
        let mut ticks = 0;
        while next <= now + interval / 2 {
            next += interval;
            ticks += 1;
        }
        if ticks > 0 && self.ticked_last {
            // How far this wake-up is from the last tick it counted, either way.
            let tick = next - interval;
            next = if now >= tick { next + (now - tick) / 64 } else { next - (tick - now) / 64 };
        }
        self.ticked_last = ticks > 0;
        self.next = Some(next);
        ticks
    }
}

/// What the engine thread's passes spent their time on, refreshed once a second
/// for Settings → Performance: name, average ms per pass, worst ms.
pub static ENGINE_TIMINGS: Mutex<Vec<(String, f32, f32)>> = Mutex::new(Vec::new());

const ENGINE_TIMING_NAMES: [&str; 4] =
    ["running ticks", "writing the sink bus", "waiting for the output lock", "waiting for input (idle)"];

#[derive(Default)]
struct EngineTimingAcc {
    sum: [Duration; 4],
    worst: [Duration; 4],
    ticks: u64,
    dropped: u64,
    passes: u32,
    since: Option<Instant>,
}

impl EngineTimingAcc {
    fn add(&mut self, row: usize, d: Duration) {
        self.sum[row] += d;
        self.worst[row] = self.worst[row].max(d);
    }

    fn counts(&mut self, ticks: u32, dropped: u32) {
        self.ticks += ticks as u64;
        self.dropped += dropped as u64;
    }

    fn end_pass(&mut self, now: Instant) {
        self.passes += 1;
        let since = *self.since.get_or_insert(now);
        if now - since < Duration::from_secs(1) {
            return;
        }
        let n = self.passes.max(1) as f32;
        let mut rows: Vec<(String, f32, f32)> = ENGINE_TIMING_NAMES.iter().enumerate()
            .map(|(i, name)| (name.to_string(), self.sum[i].as_secs_f32() * 1e3 / n, self.worst[i].as_secs_f32() * 1e3))
            .collect();
        rows.push(("ticks per pass".to_string(), self.ticks as f32 / n, 0.0));
        rows.push(("ticks dropped, per second".to_string(), self.dropped as f32, 0.0));
        if let Ok(mut t) = ENGINE_TIMINGS.lock() {
            *t = rows;
        }
        *self = Self { since: Some(now), ..Self::default() };
    }
}

/// Milliseconds since the first call, for [`note_scope_taps_read`].
fn taps_clock_ms() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

/// Last time the UI read the taps, +1 so that 0 means never.
static SCOPE_TAPS_READ_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long the I/O loop keeps tapping after the last read.
const SCOPE_TAPS_LINGER_MS: u64 = 2000;

/// Call when reading the taps: the I/O loop only fills them while it's told so.
pub fn note_scope_taps_read() {
    SCOPE_TAPS_READ_AT.store(taps_clock_ms() + 1, Ordering::Relaxed);
}

/// Whether anything has read the taps lately. A scope that just opened starts
/// empty and fills within a frame.
pub fn scope_taps_wanted() -> bool {
    let at = SCOPE_TAPS_READ_AT.load(Ordering::Relaxed);
    at != 0 && taps_clock_ms() + 1 - at < SCOPE_TAPS_LINGER_MS
}

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
    /// Calibrate Gyro presses (device ids, one per press) not yet drained by
    /// the UI thread, which runs the calibration.
    pub gyro_cal_requests: Vec<String>,
}

/// How many undrained Calibrate Gyro requests to keep — presses are rare; this
/// only bounds a UI that stopped draining.
const MAX_GYRO_CAL_PENDING: usize = 64;

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

fn add_displacement<S: std::hash::BuildHasher>(into: &mut HashMap<(String, String), Signal, S>, key: (&str, &str), v: Signal) {
    use flexinput_core::PinMapExt;
    match (into.get_pin_mut(key.0, key.1), v) {
        (Some(Signal::Vec2(a)), Signal::Vec2(b)) => *a += b,
        (Some(Signal::Float(a)), Signal::Float(b)) => *a += b,
        // First sighting, or a type that changed under us: the new value stands.
        _ => { into.insert((key.0.to_string(), key.1.to_string()), v); }
    }
}

/// Add one tick's displacement pins and click-pin rising edges into `bank`.
/// `levels` is each click pin's level on the previous tick.
fn bank_displacement(
    bank: &mut SigMap,
    levels: &mut HashMap<(String, String), bool>,
    sinks: &BusMap,
) {
    use flexinput_core::PinMapExt;
    for (k, &v) in sinks.iter() {
        if is_displacement_pin(k.1) {
            add_displacement(bank, k, v);
        } else if is_click_pin(k.1) {
            let on = matches!(v, Signal::Bool(true));
            let was = match levels.get_pin_mut(k.0, k.1) {
                Some(l) => std::mem::replace(l, on),
                None => { levels.insert((k.0.to_string(), k.1.to_string()), on); false }
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
///
/// Updated in place rather than rebuilt: this runs every engine wakeup (up to
/// the sample rate), and a rebuild cloned two key Strings per sink pin each time.
fn publish_sink_bus(
    bus: &mut HashMap<(String, String), Signal>,
    sinks: &BusMap,
    bank: &mut SigMap,
) {
    use flexinput_core::PinMapExt;
    // Level pins that left the outputs go; banked pins keep what the reader
    // has not drained yet.
    bus.retain(|k, _| is_banked_pin(&k.1) || sinks.contains_key(k));
    for (k, &v) in sinks.iter() {
        if !is_banked_pin(k.1) {
            bus.set_pin(k.0, k.1, v);
        }
    }
    for (k, v) in bank.drain() {
        add_displacement(bus, (&k.0, &k.1), v);
    }
}

/// Read the bus for the sinks: a copy of it, with the banked pins zeroed in the
/// bus afterwards so the same movement or notch is never delivered twice.
pub fn drain_sink_bus(bus: &SinkBus) -> HashMap<(String, String), Signal> {
    let mut out = HashMap::new();
    drain_sink_bus_into(bus, &mut out);
    out
}

/// [`drain_sink_bus`] into `out`, which is made an exact copy of the bus in
/// place. The I/O thread calls this once per iteration (the polling rate) with
/// the same map, so the copy reuses last iteration's keys instead of cloning
/// two Strings per sink pin every time.
pub fn drain_sink_bus_into(bus: &SinkBus, out: &mut HashMap<(String, String), Signal>) {
    use flexinput_core::PinMapExt;
    let mut bus = bus.write().unwrap();
    out.retain(|k, _| bus.contains_key(k));
    for (k, v) in bus.iter_mut() {
        out.set_pin(&k.0, &k.1, *v);
        if is_banked_pin(&k.1) {
            *v = match *v {
                Signal::Vec2(_) => Signal::Vec2(Default::default()),
                Signal::Float(_) => Signal::Float(0.0),
                other => other,
            };
        }
    }
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

// ── Engine ↔ device-I/O hand-off ────────────────────────────────────────────
//
// The engine used to tick on its own timer at the sample rate (2 kHz by
// default) while inputs only change when the I/O thread polls (500 Hz by
// default): three of every four wakeups ran against an input that hadn't
// moved, and each one was a context switch that, on a machine a game keeps
// busy, preempts a game thread. And the I/O thread read the sink bus as the
// engine had left it after the PREVIOUS poll's input, so outputs lagged one
// polling period behind.
//
// Now the I/O thread publishes a frame and kicks the engine; the engine runs
// every tick due by then (same sample-rate math, same dt) against that frame
// and marks it consumed; the I/O thread waits for that — bounded — before it
// reads the bus, so the outputs it sends come from the input it just polled.
// If the engine can't finish in time (a slow machine), the I/O thread goes on
// with the previous outputs, which is exactly the old behavior. If the kicks
// stop (I/O thread stalled), the engine falls back to ticking on a timer.

/// Synchronizes the engine with the device-I/O thread; see the comment above.
/// One per process, shared by [`spawn_processing_thread`] and the I/O loop.
pub struct EngineSync {
    /// Input frames the I/O thread has published.
    published: std::sync::atomic::AtomicU64,
    /// The newest published frame the engine has run its due ticks against.
    consumed: std::sync::atomic::AtomicU64,
    #[cfg(windows)]
    kick: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(windows)]
    done: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(not(windows))]
    kicked: (Mutex<bool>, std::sync::Condvar),
    #[cfg(not(windows))]
    done: (Mutex<()>, std::sync::Condvar),
}

// SAFETY: the two HANDLEs are kernel event objects, safe to signal and wait on
// from any thread; they are only closed in Drop.
#[cfg(windows)]
unsafe impl Send for EngineSync {}
#[cfg(windows)]
unsafe impl Sync for EngineSync {}

impl EngineSync {
    pub fn new() -> Arc<Self> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::CreateEventW;
            // SAFETY: FFI. Auto-reset, initially unsignaled, unnamed.
            let (kick, done) = unsafe {
                (
                    CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()),
                    CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()),
                )
            };
            Arc::new(Self { published: 0.into(), consumed: 0.into(), kick, done })
        }
        #[cfg(not(windows))]
        {
            Arc::new(Self {
                published: 0.into(),
                consumed: 0.into(),
                kicked: (Mutex::new(false), std::sync::Condvar::new()),
                done: (Mutex::new(()), std::sync::Condvar::new()),
            })
        }
    }

    /// I/O thread: a fresh input frame is published — wake the engine. Call it
    /// AFTER storing the signals. Returns the frame's number for
    /// [`Self::wait_consumed`].
    pub fn input_published(&self) -> u64 {
        let n = self.published.fetch_add(1, Ordering::AcqRel) + 1;
        #[cfg(windows)]
        // SAFETY: FFI on our own event.
        unsafe {
            windows_sys::Win32::System::Threading::SetEvent(self.kick);
        }
        #[cfg(not(windows))]
        {
            *self.kicked.0.lock().unwrap() = true;
            self.kicked.1.notify_one();
        }
        n
    }

    /// I/O thread: wait until the engine has run its due ticks against frame
    /// `n`, for at most `budget`. True if it has.
    pub fn wait_consumed(&self, n: u64, budget: Duration, waiter: &crate::hr_timer::HrWaiter) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            if self.consumed.load(Ordering::Acquire) >= n {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            #[cfg(windows)]
            waiter.wait_event(self.done, deadline - now);
            #[cfg(not(windows))]
            {
                let _ = waiter;
                let guard = self.done.0.lock().unwrap();
                let _ = self.done.1.wait_timeout(guard, deadline - now);
            }
        }
    }

    /// Engine: the newest frame published so far. Read BEFORE loading the
    /// signals, so the signals loaded are at least that new.
    fn frame(&self) -> u64 {
        self.published.load(Ordering::Acquire)
    }

    /// Engine: the ticks due so far have run against frame `n`.
    fn mark_consumed(&self, n: u64) {
        self.consumed.fetch_max(n, Ordering::AcqRel);
        #[cfg(windows)]
        // SAFETY: FFI on our own event.
        unsafe {
            windows_sys::Win32::System::Threading::SetEvent(self.done);
        }
        #[cfg(not(windows))]
        {
            let _g = self.done.0.lock().unwrap();
            self.done.1.notify_all();
        }
    }

    /// Engine: sleep until the next kick, or `fallback` if none comes.
    fn wait_kick(&self, fallback: Duration, waiter: &crate::hr_timer::HrWaiter) {
        #[cfg(windows)]
        waiter.wait_event(self.kick, fallback);
        #[cfg(not(windows))]
        {
            let _ = waiter;
            let mut kicked = self.kicked.0.lock().unwrap();
            if !*kicked {
                kicked = self.kicked.1.wait_timeout(kicked, fallback).unwrap().0;
            }
            *kicked = false;
        }
    }
}

#[cfg(windows)]
impl Drop for EngineSync {
    fn drop(&mut self) {
        // SAFETY: FFI; created in `new`, not used after drop.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.kick);
            windows_sys::Win32::Foundation::CloseHandle(self.done);
        }
    }
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
    // Hand-off with the device-I/O thread: the engine runs when it publishes
    // an input frame (see `EngineSync`).
    sync: Arc<EngineSync>,
) -> thread::JoinHandle<()> {
    // Named, so it can be told apart in a per-thread CPU list.
    thread::Builder::new().name("engine".into()).spawn(move || {
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
        crate::raise_input_thread("engine");

        let mut clock = TickClock::default();
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
        // Calibrate Gyro presses over the catch-up loop, like `scope_acc`.
        let mut gyro_cal_acc: Vec<String> = Vec::new();
        // Displacement pins summed over the catch-up loop (see
        // `is_displacement_pin`); emptied into the sink bus once per wakeup.
        let mut disp_bank: SigMap = SigMap::default();
        // Previous-tick level of each click pin (see `is_click_pin`).
        let mut click_levels: HashMap<(String, String), bool> = HashMap::new();

        // High-resolution waiter for the wait for the next input frame below —
        // precise without raising the global timer resolution (see hr_timer).
        // The device-I/O thread no longer calls timeBeginPeriod, so a plain
        // timed wait would round up to ~15.6 ms when the kicks stop.
        let waiter = crate::hr_timer::HrWaiter::new();
        let mut timings = EngineTimingAcc::default();

        loop {
            puffin::GlobalProfiler::lock().new_frame();
            puffin::profile_scope!("proc_thread_iter");
            // The input frame these ticks run against — read before the
            // signals are loaded below, so they're at least this new.
            let frame = sync.frame();
            let now = Instant::now();

            // Re-read sample rate each wakeup so live retunes apply immediately.
            // Clamp defensively in case settings.json holds a garbage value.
            let sr = sample_rate.load(Ordering::Relaxed).clamp(100, 16_000);
            LIVE_SAMPLE_RATE.store(sr, Ordering::Relaxed);
            let dt: f32 = 1.0 / sr as f32;
            let interval = Duration::from_nanos(1_000_000_000 / sr as u64);

            // How many ticks have elapsed since we last processed?
            let ticks = clock.due(now, interval);
            // Cap catchup to avoid spiral-of-death on heavy load: 10 ms worth,
            // at least 16. A wakeup normally runs one polling period's ticks
            // (4 at the defaults); the slowest polling step (125 Hz, 8 ms)
            // must still fit at any sample rate.
            let due_ticks = ticks;
            let ticks = ticks.min((sr / 100).max(16));
            timings.counts(ticks, due_ticks - ticks);

            if ticks > 0 {
                ENGINE_PASSES.fetch_add(1, Ordering::Relaxed);
                ENGINE_TICKS.fetch_add(ticks as u64, Ordering::Relaxed);
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
                    let eval_t = Instant::now();
                    for t in 0..ticks {
                        tick_out.skip_display = t + 1 != ticks;
                        tick_out.same_input = t != 0;
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
                        gyro_cal_acc.append(&mut tick_out.gyro_cal_requests);
                        bank_displacement(&mut disp_bank, &mut click_levels, &tick_out.sink_outputs);
                    }
                    timings.add(0, eval_t.elapsed());
                }

                // tick_out now holds the LAST tick's outputs/inputs/sinks; the
                // displacement pins carry every tick's share instead.
                {
                    puffin::profile_scope!("write_sink_bus");
                    let sink_t = Instant::now();
                    publish_sink_bus(&mut sink_bus.write().unwrap(), &tick_out.sink_outputs, &mut disp_bank);
                    timings.add(1, sink_t.elapsed());
                }
                {
                    puffin::profile_scope!("write_proc_outputs");
                    let lock_t = Instant::now();
                    let mut out = output.lock().unwrap();
                    timings.add(2, lock_t.elapsed());
                    for sample in scope_acc.drain(..) {
                        if out.scope_pending.len() < MAX_SCOPE_PENDING {
                            out.scope_pending.push(sample);
                        }
                    }
                    for dev in gyro_cal_acc.drain(..) {
                        if out.gyro_cal_requests.len() < MAX_GYRO_CAL_PENDING {
                            out.gyro_cal_requests.push(dev);
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

            // The due ticks have run against `frame`: let the I/O thread send
            // their outputs (see `EngineSync`).
            sync.mark_consumed(frame);

            // Sleep until the I/O thread publishes the next input frame. If it
            // goes quiet (stalled, or not started yet), fall back to waking on
            // our own every ~2.5 polling periods (2 ms before the first poll
            // rate is known) and catching up the ticks due by then.
            let io_hz = current_io_rate();
            let fallback = if io_hz > 0 {
                Duration::from_secs_f64((2.5 / io_hz as f64).clamp(0.002, 0.025))
            } else {
                Duration::from_millis(2)
            };
            let wait_t = Instant::now();
            sync.wait_kick(fallback, &waiter);
            timings.add(3, wait_t.elapsed());
            timings.end_pass(Instant::now());
        }
    }).expect("failed to spawn engine thread")
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;

    /// Wake-ups `period` apart with alternating jitter; the ticks each one gets.
    fn wake_ticks(clock: &mut TickClock, start: Instant, period: Duration, interval: Duration, n: u32) -> Vec<u32> {
        (0..n).map(|i| {
            let jitter = Duration::from_micros(if i % 2 == 0 { 300 } else { 0 });
            clock.due(start + period * i + jitter, interval)
        }).collect()
    }

    #[test]
    fn equal_rates_give_exactly_one_tick_per_wake_up_despite_jitter() {
        let t = Duration::from_millis(8);
        let start = Instant::now();
        let mut clock = TickClock::default();
        // Any phase: a schedule started half a period off settles too.
        clock.due(start, t);
        let later = start + Duration::from_millis(4);
        let got = wake_ticks(&mut clock, later, t, t, 400);
        assert!(got[50..].iter().all(|&n| n == 1), "{:?}", &got[50..]);
    }

    #[test]
    fn twice_the_rate_gives_exactly_two_ticks_per_wake_up() {
        let start = Instant::now();
        let mut clock = TickClock::default();
        clock.due(start, Duration::from_millis(4));
        let later = start + Duration::from_millis(3);
        let got = wake_ticks(&mut clock, later, Duration::from_millis(8), Duration::from_millis(4), 400);
        assert!(got[50..].iter().all(|&n| n == 2), "{:?}", &got[50..]);
    }

    #[test]
    fn the_clock_keeps_time_whatever_the_rates() {
        let start = Instant::now();
        for (period_us, interval_us) in [(8000u64, 8000u64), (8000, 500), (2000, 500), (6993, 2000), (1000, 8000)] {
            let mut clock = TickClock::default();
            let n = 8_000_000 / period_us as u32 / 2;
            let ticks: u32 = (0..n).map(|i| {
                clock.due(start + Duration::from_micros(period_us) * i, Duration::from_micros(interval_us))
            }).sum();
            // n wake-ups span n - 1 periods, and the first counts one tick.
            let want = ((n as u64 - 1) * period_us / interval_us) as i64 + 1;
            assert!((ticks as i64 - want).abs() <= 2, "{period_us}/{interval_us}: {ticks} vs {want}");
        }
    }

    #[test]
    fn a_stall_is_caught_up_and_a_new_rate_starts_fresh() {
        let t = Duration::from_millis(2);
        let start = Instant::now();
        let mut clock = TickClock::default();
        assert_eq!(clock.due(start, t), 1);
        assert_eq!(clock.due(start + Duration::from_millis(100), t), 50);
        assert_eq!(clock.due(start + Duration::from_millis(100), Duration::from_millis(8)), 1);
    }

    fn key(pin: &str) -> (String, String) {
        ("virtual.keymouse:0".to_string(), pin.to_string())
    }

    fn tick(disp: Vec2, lmb: bool) -> BusMap {
        BusMap::from_iter([
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
        let mut bank = SigMap::default();
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
        let mut bank = SigMap::default();
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
        let (mut bank, mut prev) = (SigMap::default(), HashMap::new());
        let mut got = 0.0;
        for (i, &on) in levels.iter().enumerate() {
            let t = i + 1;
            let last = BusMap::from_iter([(key("scroll_up"), Signal::Bool(on))]);
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
        let (mut bank, mut prev) = (SigMap::default(), HashMap::new());
        let held = BusMap::from_iter([(key("scroll_up"), Signal::Bool(true))]);
        bank_displacement(&mut bank, &mut prev, &held);
        bank_displacement(&mut bank, &mut prev, &BusMap::default());
        bank_displacement(&mut bank, &mut prev, &held);
        assert_eq!(bank.get(&key("scroll_up")), Some(&Signal::Float(2.0)));
    }

    // ── Engine ↔ I/O hand-off ────────────────────────────────────────────────

    /// The I/O side's wait returns as soon as the engine has run the frame,
    /// not at the end of its budget.
    #[test]
    fn hand_off_returns_when_the_engine_consumes_the_frame() {
        let sync = EngineSync::new();
        let engine = {
            let sync = Arc::clone(&sync);
            std::thread::spawn(move || {
                let waiter = crate::hr_timer::HrWaiter::new();
                sync.wait_kick(Duration::from_secs(5), &waiter);
                std::thread::sleep(Duration::from_millis(2));
                sync.mark_consumed(sync.frame());
            })
        };
        let waiter = crate::hr_timer::HrWaiter::new();
        let t = Instant::now();
        let n = sync.input_published();
        assert!(sync.wait_consumed(n, Duration::from_secs(2), &waiter));
        assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
        engine.join().unwrap();
    }

    /// With no engine, the wait gives up at its budget — precisely, not at the
    /// next ~15.6 ms system tick.
    #[test]
    fn hand_off_times_out_at_its_budget() {
        let sync = EngineSync::new();
        let waiter = crate::hr_timer::HrWaiter::new();
        let n = sync.input_published();
        let t = Instant::now();
        assert!(!sync.wait_consumed(n, Duration::from_millis(3), &waiter));
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(3), "{took:?}");
        assert!(took < Duration::from_millis(12), "{took:?}");
    }

    /// An engine pass over an OLDER frame (its done-signal still pending)
    /// doesn't satisfy a wait for a newer one.
    #[test]
    fn hand_off_ignores_an_older_frames_signal() {
        let sync = EngineSync::new();
        let waiter = crate::hr_timer::HrWaiter::new();
        let old = sync.input_published();
        sync.mark_consumed(old);
        let new = sync.input_published();
        assert!(!sync.wait_consumed(new, Duration::from_millis(3), &waiter));
        sync.mark_consumed(new);
        assert!(sync.wait_consumed(new, Duration::from_millis(3), &waiter));
    }

    /// The real processing thread consumes every published frame, including
    /// one published after a quiet spell (it was waiting on its fallback).
    #[test]
    fn processing_thread_consumes_each_published_frame() {
        let sync = EngineSync::new();
        let output = Arc::new(Mutex::new(ProcessingOutput::default()));
        spawn_processing_thread(
            new_arc_graph(),
            new_arc_signals(),
            Arc::clone(&output),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(AtomicU32::new(2000)),
            new_ui_source_block(),
            new_ui_source_block(),
            new_ui_source_block(),
            Arc::clone(&sync),
        );
        let waiter = crate::hr_timer::HrWaiter::new();
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(2));
            let n = sync.input_published();
            assert!(sync.wait_consumed(n, Duration::from_millis(500), &waiter), "frame {n}");
        }
        std::thread::sleep(Duration::from_millis(30));
        let n = sync.input_published();
        assert!(sync.wait_consumed(n, Duration::from_millis(500), &waiter), "after a quiet spell");
    }
}
