//! Per-device calibration window.
//!
//! Opened from a device.source node's "Calibrate" button. Shows one section per
//! applicable calibration routine for the device family: Gyro, Sticks, Triggers.
//!
//! Live signals come from the I/O thread's raw `device_signals` map. The window
//! displays *calibrated* values by re-applying the node's current params at
//! draw time — same arithmetic the engine uses — so during a Gyro session the
//! user sees the lines visibly close in on 0 as new offsets are written.
//!
//! Per-session UI state lives in `egui::Memory` keyed by NodeId.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use eframe::egui;
use egui::{Color32, Pos2, Sense, Stroke};
use egui_snarl::NodeId;
use flexinput_core::Signal;
use serde_json::Value;

use crate::canvas::Canvas;

// ── Layout / typography constants ────────────────────────────────────────────

/// Fraction of the window width allocated to the left text column. The rest
/// (minus a gap) becomes the right widget column.
const LEFT_FRAC: f32 = 0.40;
/// Minimum left-column width — clamps so instruction text never goes too
/// narrow on small windows.
const LEFT_MIN_W: f32 = 320.0;

/// Gap between the two side-by-side vectorscopes / triggers.
const SCOPE_GAP_FRAC: f32 = 0.03;

/// Gyro-scope aspect ratio: width / height. Mockup is ~2.6× wider than tall.
const SCOPE_ASPECT: f32 = 2.4;

/// Body / instruction font size. Mockup's instruction text is roughly 3× the
/// default `.small()` (which is ~10 px); 16 px lands close.
const INSTRUCT_SIZE: f32 = 15.0;
/// Orange used to highlight action verbs / key steps in instruction text.
const ORANGE: Color32 = Color32::from_rgb(220, 160, 40);

// Analog-trigger silhouette SVG (gray + black stroke). Used as the
// background glyph behind each trigger's live-fill bar.
const ATRIG_SVG: &[u8] = include_bytes!("../../../../app/assets/Atrig.svg");

// ── Capability detection ─────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub struct CalCaps {
    pub gyro: bool,
    pub sticks: bool,
    /// Which sticks the device actually has. A Joy-Con half reports ONE, and
    /// drawing both left an empty scope the user could never fill — it looked
    /// like a broken stick rather than a stick that does not exist.
    pub left_stick: bool,
    pub right_stick: bool,
    pub triggers: bool,
    /// Device reports absolute orientation (`orientation` pin) and so can have
    /// a baseline pose captured.
    pub orientation: bool,
}

impl CalCaps {
    pub fn any(self) -> bool { self.gyro || self.sticks || self.triggers }
}

/// Capabilities for a device, refined by the pins it actually declares.
///
/// The slug table below can only know about hardware someone has already added
/// to it, and every new controller silently lands on the conservative
/// sticks-only fallback — which is exactly how the Joy-Con 2 ended up with no
/// gyro section despite having the best IMU of any pad here. Where a pin
/// answers the question directly, the pin wins.
///
/// Triggers stay slug-driven: layouts declare `left_trigger`/`right_trigger`
/// for digital shoulders too, so their presence does NOT mean the trigger is
/// analog and there is nothing to calibrate.
pub fn caps_for_pins(dev_id: &str, pins: &[String]) -> CalCaps {
    let mut caps = caps_for(dev_id);
    if pins.is_empty() {
        return caps;
    }
    let has = |id: &str| pins.iter().any(|p| p == id);
    caps.gyro = has("gyro_x") || has("gyro_y") || has("gyro_z");
    caps.left_stick = has("left_stick") || has("left_stick_x");
    caps.right_stick = has("right_stick") || has("right_stick_x");
    caps.sticks = caps.left_stick || caps.right_stick;
    caps.orientation = has("orientation");
    caps
}

/// Output pin ids a `device.source` node declares, used to refine capabilities.
fn node_pin_ids(n: &crate::canvas::NodeData) -> Vec<String> {
    n.params
        .get("output_pin_ids")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn caps_for(dev_id: &str) -> CalCaps {
    // Accept both the native (`gilrs:`) and the SDL (`sdl:`) pad-id prefixes: a
    // DualSense routed through SDL gets the same calibration surface as a native
    // one (matches `device_source_caps`, which gates the Calibrate button). Only
    // handling `gilrs:` here is why the button appeared but the window stayed
    // empty for SDL pads.
    if let Some(slug) = crate::canvas::remapper_icons::phys_pad_slug(dev_id) {
        return match slug {
            // XInput: analog sticks + analog triggers, no IMU.
            "xinput"     => CalCaps { gyro: false, sticks: true,  left_stick: true, right_stick: true, triggers: true,  orientation: false },
            // DS4 / DualSense: sticks + IMU + analog triggers.
            "ds4"        => CalCaps { gyro: true,  sticks: true,  left_stick: true, right_stick: true, triggers: true,  orientation: false },
            "dualsense"  => CalCaps { gyro: true,  sticks: true,  left_stick: true, right_stick: true, triggers: true,  orientation: false },
            // Switch Pro: sticks + IMU, but triggers (ZL/ZR) are pure digital.
            "switch_pro" => CalCaps { gyro: true,  sticks: true,  left_stick: true, right_stick: true, triggers: false, orientation: false },
            // Joy-Con 2: one stick per half, full IMU, digital shoulders. The
            // firmware fuses orientation itself, so it also gets a baseline.
            "joycon2_l"  => CalCaps { gyro: true,  sticks: true,  left_stick: true, right_stick: false, triggers: false, orientation: true },
            "joycon2_r"  => CalCaps { gyro: true,  sticks: true,  left_stick: false, right_stick: true, triggers: false, orientation: true },
            // Anything else: conservative sticks-only.
            _            => CalCaps { gyro: false, sticks: true,  left_stick: true, right_stick: true, triggers: false, orientation: false },
        };
    }
    CalCaps { gyro: false, sticks: false, left_stick: false, right_stick: false, triggers: false, orientation: false }
}

/// Layout metrics derived from the current `ui.available_width()`. Used to
/// keep all section widgets aligned and scale with the user's window resize.
#[derive(Clone, Copy)]
struct Layout {
    left_w:    f32,
    right_w:   f32,
    scope_gap: f32,
    /// Single vectorscope / inner-widget cell width (half of right_w minus gap).
    cell_w:    f32,
    /// Vectorscope height (cell is square).
    cell_h:    f32,
    /// Gyro waterfall height — shorter than the vectorscopes per user spec.
    gyro_h:    f32,
    /// Trigger arc box — half-size of a vectorscope, centered under it.
    trig_w:    f32,
    trig_h:    f32,
}

impl Layout {
    fn compute(ui: &egui::Ui) -> Self {
        let avail = ui.available_width().max(700.0);
        let left_w = (avail * LEFT_FRAC).max(LEFT_MIN_W);
        // Subtract a small horizontal padding so the right column doesn't
        // collide with the window's right edge.
        let right_w = (avail - left_w - 12.0).max(360.0);
        let scope_gap = (right_w * SCOPE_GAP_FRAC).clamp(8.0, 24.0);
        let cell_w = ((right_w - scope_gap) * 0.5).max(120.0);
        let cell_h = cell_w; // vectorscope is square
        let gyro_h = (right_w / SCOPE_ASPECT).clamp(100.0, 180.0);
        // Trigger widget gains an outer range-clamp arc, so reserve more
        // box around the inner threshold arc + silhouette.
        let trig_w = cell_w * 0.72;
        let trig_h = trig_w;
        Self { left_w, right_w, scope_gap, cell_w, cell_h, gyro_h, trig_w, trig_h }
    }
}

// ── Live signal access ───────────────────────────────────────────────────────

type LiveSignals = HashMap<(String, String), Signal>;

fn read_float(live: &LiveSignals, dev: &str, pin: &str) -> f32 {
    match live.get(&(dev.into(), pin.into())) {
        Some(Signal::Float(v)) => *v,
        Some(Signal::Bool(b))  => if *b { 1.0 } else { 0.0 },
        _ => 0.0,
    }
}

fn read_vec2(live: &LiveSignals, dev: &str, x_pin: &str, y_pin: &str) -> (f32, f32) {
    (read_float(live, dev, x_pin), read_float(live, dev, y_pin))
}

fn read_param_f32(canvas: &Canvas, node: NodeId, key: &str, fallback: f32) -> f32 {
    canvas.snarl.get_node(node)
        .and_then(|n| n.params.get(key).and_then(|v| v.as_f64()))
        .map(|f| f as f32).unwrap_or(fallback)
}

fn read_param_arr<const N: usize>(canvas: &Canvas, node: NodeId, key: &str, fallback: [f32; N]) -> [f32; N] {
    let Some(arr) = canvas.snarl.get_node(node)
        .and_then(|n| n.params.get(key).and_then(|v| v.as_array().cloned()))
    else { return fallback; };
    if arr.len() < N { return fallback; }
    let mut out = fallback;
    for i in 0..N {
        if let Some(v) = arr[i].as_f64() { out[i] = v as f32; }
    }
    out
}

/// Read a 3-element bool array (`[x, y, z]` invert flags) from node params.
fn read_invert3(canvas: &Canvas, node: NodeId, key: &str) -> [bool; 3] {
    let Some(arr) = canvas.snarl.get_node(node)
        .and_then(|n| n.params.get(key).and_then(|v| v.as_array().cloned()))
    else { return [false; 3]; };
    let mut out = [false; 3];
    for i in 0..3.min(arr.len()) {
        if let Some(b) = arr[i].as_bool() { out[i] = b; }
    }
    out
}

// ── Per-session state ────────────────────────────────────────────────────────

/// Scope time window in milliseconds. Samples older than this are dropped
/// from the buffer and the view is mapped 0 ms at the top → WIN_MS at the
/// bottom regardless of UI repaint rate. Set to 1 s so a quick controller
/// motion (~0.5–1 s) fits in-frame with room to read its shape.
const SCOPE_WIN_MS: f32 = 1000.0;
const GYRO_SAMPLE_FRAMES: u32 = 250; // ~0.5 s of stable data at the default 500 Hz polling (duration scales with the polling-rate setting)

#[derive(Clone)]
struct ScopeBuffer {
    /// (age in ms relative to now-of-write — re-aged on render, frame channels).
    /// Stored as VecDeque<(Instant, [f32; 5])> so render can compute real age.
    samples: VecDeque<(Instant, [f32; 5])>,
    /// Last value pushed for each channel — used to fill in missed frames when
    /// the UI repaints faster than samples arrive (we just re-emit the prev
    /// reading so the lines are continuous).
    last: [f32; 5],
}
impl Default for ScopeBuffer {
    fn default() -> Self {
        Self { samples: VecDeque::new(), last: [0.0; 5] }
    }
}
impl ScopeBuffer {
    fn push(&mut self, frame: [f32; 5]) {
        self.last = frame;
        let now = Instant::now();
        // Drop samples older than ~1.5× the visible window so we keep some
        // pixels' worth of slack but the buffer never grows unbounded.
        let cutoff = std::time::Duration::from_millis((SCOPE_WIN_MS * 1.5) as u64);
        while let Some(&(t, _)) = self.samples.front() {
            if now.duration_since(t) > cutoff { self.samples.pop_front(); }
            else { break; }
        }
        self.samples.push_back((now, frame));
    }
}

#[derive(Clone, Default)]
struct GyroSession {
    sampling: bool,
    n: u32,
    /// Welford running mean per axis [gx, gy, gz_unused, ax, ay].
    mean:  [f64; 5],
    m2:    [f64; 5],
    scope: ScopeBuffer,
    /// When the last calibration (manual or auto) finished — drives the
    /// green completion glow on the start button.
    done_at: Option<Instant>,
}

/// Per-axis orientation-calibration sample buffer. Each Vec collects raw
/// gyro vectors observed during a single-axis rotation. Mean accel during
/// each pass is averaged across captures and used as a gravity reference
/// to refine the PCA-derived rotation.
#[derive(Clone, Default)]
struct OrientSession {
    /// Which axis is currently being sampled (0=Roll, 1=Pitch, 2=Yaw),
    /// or None if idle.
    sampling_axis: Option<usize>,
    /// Frame counter for the active sampling pass.
    sampling_n: u32,
    /// Per-body-axis raw gyro samples (3-vectors).
    samples: [Vec<[f32; 3]>; 3],
    /// Per-body-axis "captured" flag.
    captured: [bool; 3],
    /// Running mean accel vector recorded during each axis pass (chip
    /// frame). Averaged together to give a clean gravity reference.
    /// `accel_sum[axis] / accel_n[axis]` = mean accel for that pass.
    accel_sum: [[f64; 3]; 3],
    accel_n:   [u32; 3],
}

/// Frames to collect for one orientation-axis sampling pass. ~0.5 s at the
/// default 500 Hz polling (longer at lower settings) — fewer is plenty given
/// PCA only needs ~30 samples in different directions to recover an axis
/// confidently.
const ORIENT_SAMPLE_FRAMES: u32 = 250;

const STICK_BUCKETS: usize = 72; // 5° per bucket
const STICK_MIN_BUCKETS: usize = 64;
const STICK_CENTER_THRESHOLD: f32 = 0.05;
const STICK_CENTER_TARGET_N: u32 = 150;
const STICK_TRAIL_LEN: usize = 60;

#[derive(Clone)]
struct StickSession {
    sampling: bool,
    l_edge: [f32; 72],
    r_edge: [f32; 72],
    l_center_sum: [f64; 2],
    l_center_n: u32,
    r_center_sum: [f64; 2],
    r_center_n: u32,
}
impl Default for StickSession {
    fn default() -> Self {
        Self {
            sampling: false,
            l_edge: [0.0; 72],
            r_edge: [0.0; 72],
            l_center_sum: [0.0; 2], l_center_n: 0,
            r_center_sum: [0.0; 2], r_center_n: 0,
        }
    }
}

const TRIG_QUANTIZE: f32 = 1.0 / 255.0;     // ~8-bit step
const TRIG_JITTER_MARGIN_STEPS: f32 = 3.0;  // shrink max by ~3 quantization steps

#[derive(Clone, Default)]
struct TriggerSession {
    sampling: bool,
    l_min_obs: f32,
    l_max_obs: f32,
    r_min_obs: f32,
    r_max_obs: f32,
    /// Running mean of the top-1% values seen — used to debias the max from
    /// outlier spikes that happen on full-press impact noise.
    l_max_top_sum: f64,
    l_max_top_n:   u32,
    r_max_top_sum: f64,
    r_max_top_n:   u32,
    /// Symmetrically debias the min using the bottom-1% (release-rest noise).
    l_min_bot_sum: f64,
    l_min_bot_n:   u32,
    r_min_bot_sum: f64,
    r_min_bot_n:   u32,
    seen: bool,
}

#[derive(Clone)]
struct WindowState {
    /// L/R fading dot trail for the stick vectorscopes (unit-circle coords).
    l_trail: VecDeque<[f32; 2]>,
    r_trail: VecDeque<[f32; 2]>,
    gyro: GyroSession,
    stick: StickSession,
    trig: TriggerSession,
    orient: OrientSession,
    /// User-selected gyro-scope time window in ms (500 / 1000 / 5000).
    scope_win_ms: f32,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            l_trail: VecDeque::new(),
            r_trail: VecDeque::new(),
            gyro: GyroSession::default(),
            stick: StickSession::default(),
            trig: TriggerSession::default(),
            orient: OrientSession::default(),
            scope_win_ms: 1000.0,
        }
    }
}

fn state_id(node: NodeId) -> egui::Id { egui::Id::new(("cal_state", node)) }

// ── Public entry: render all open windows ────────────────────────────────────

pub type SpikeSettings = Arc<RwLock<HashMap<String, (bool, f32)>>>;

pub fn show_windows(
    ctx: &egui::Context,
    canvas: &mut Canvas,
    open: &mut HashSet<NodeId>,
    live: &LiveSignals,
    scope_taps: &flexinput_engine::ScopeTaps,
    spike_settings: &SpikeSettings,
) {
    let entries: Vec<(NodeId, String, String, Vec<String>)> = open.iter()
        .filter_map(|&nid| {
            let n = canvas.snarl.get_node(nid)?;
            if n.module_id != "device.source" { return None; }
            let dev_id = n.params.get("device_id").and_then(|v| v.as_str())?.to_string();
            Some((nid, n.display_name.clone(), dev_id, node_pin_ids(n)))
        })
        .collect();

    let mut to_close: Vec<NodeId> = Vec::new();
    for (node_id, display_name, dev_id, pin_ids) in entries {
        let caps = caps_for_pins(&dev_id, &pin_ids);
        if !caps.any() {
            to_close.push(node_id);
            continue;
        }
        let mut is_open = true;
        let title = format!("{} Calibration", display_name);
        // Default size capped to ~85% of the host window so the cal window
        // always fits inside the main app frame even on smaller monitors.
        let screen = ctx.content_rect();
        // Default to a compact size matching the min, so the window opens
        // as small as it can go and the user can grow it if needed.
        let min_w = (screen.width()  * 0.40).clamp(480.0, 700.0);
        let min_h = (screen.height() * 0.40).clamp(280.0, 460.0);
        let default_w = min_w;
        let default_h = min_h;
        // Opaque window frame — without an explicit fill the tab strip's
        // drop-shadow from underneath bleeds through the window content.
        let mut window_frame = egui::Frame::window(&ctx.style());
        window_frame.fill = ctx.style().visuals.window_fill;
        egui::Window::new(title)
            .id(egui::Id::new(("cal_window", node_id)))
            .open(&mut is_open)
            .resizable(true)
            .collapsible(true)
            .frame(window_frame)
            .default_width(default_w)
            .default_height(default_h)
            .min_width(min_w)
            .min_height(min_h)
            .show(ctx, |ui| {
                draw_window_body(ui, canvas, node_id, &dev_id, caps, live, scope_taps, spike_settings);
            });
        if !is_open {
            to_close.push(node_id);
            ctx.data_mut(|d| d.remove::<WindowState>(state_id(node_id)));
        }
    }
    for nid in to_close { open.remove(&nid); }
}

// ── Auto-calibration: flat + still watcher ───────────────────────────────────
//
// Runs every app frame (independent of the calibration window) for each
// device.source node that opted in via `gyro_auto_cal`. Keeps a rolling
// 5 s buffer of raw IMU samples; only once every gyro/accel axis holds
// still for the WHOLE window AND gravity unambiguously says the pad lies
// flat on a level surface do the buffer means become the new offsets and
// its variance the new noise floors (one atomic write) — then the watcher
// stays in cooldown until the controller is picked up again.

/// How long the pad must be flat + still before auto-calibration fires. The
/// stats come from this whole window and apply ATOMICALLY at its end — any
/// violation inside the window keeps poisoning the peak-to-peak checks until
/// it slides out, so a "half-done" calibration can never land.
const AUTOCAL_WINDOW_S: f32 = 5.0;
/// Max peak-to-peak span per gyro axis over the window to count as "still"
/// (normalized units: 0.006 ≈ 12 dps span at the ±2000 dps reference —
/// hand-held tremor is well above this, a table is well below).
const AUTOCAL_GYRO_P2P: f32 = 0.006;
/// Max peak-to-peak span per accel axis (0.008 ≈ 0.064 G span at ±8 G).
const AUTOCAL_ACCEL_P2P: f32 = 0.008;
/// "Flat" must be UNAMBIGUOUS: mean |ax| / |ay| under this (≈ 3.7° of surface
/// tilt at ±8 G) — a pad held in hands or propped on a stand never passes…
const AUTOCAL_FLAT_XY: f64 = 0.008;
/// …and mean |az| inside this tight band (±1 G ≈ 0.125 at ±8 G, ±8 %).
const AUTOCAL_FLAT_Z: (f32, f32) = (0.115, 0.135);

#[derive(Clone, Default)]
struct AutoCalState {
    /// Rolling raw-sample window [gx, gy, gz, ax, ay, az] at UI frame rate.
    buf: VecDeque<(Instant, [f32; 6])>,
    /// Set after firing; clears once stillness breaks (pad picked up).
    cooldown: bool,
}

fn auto_cal_id(node: NodeId) -> egui::Id { egui::Id::new(("auto_cal", node)) }

/// Completion-glow timestamp, separate from `WindowState` so the device-card
/// Calibrate buttons can glow green even when the window is closed (auto-cal).
fn cal_glow_id(node: NodeId) -> egui::Id { egui::Id::new(("cal_done_glow", node)) }

/// Border animation for a device card's Calibrate button: orange trail while
/// any calibration session is sampling on that node, green fading glow right
/// after one finishes (manual or auto). Call with the button's rect from any
/// place that renders a Calibrate button for a `device.source` node.
pub fn calibrate_button_activity(ui: &egui::Ui, node: NodeId, rect: egui::Rect) {
    let (sampling, done) = ui.ctx().data(|d| {
        let win = d.get_temp::<WindowState>(state_id(node));
        let sampling = win.as_ref()
            .map(|s| s.gyro.sampling || s.stick.sampling || s.trig.sampling
                || s.orient.sampling_axis.is_some())
            .unwrap_or(false);
        // Manual completions live in WindowState; auto-cal also stamps the
        // standalone key so the glow works with the window closed. Take the
        // most recent so neither can shadow a fresh one with a stale one.
        let done = match (d.get_temp::<Instant>(cal_glow_id(node)),
                          win.and_then(|s| s.gyro.done_at)) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        (sampling, done)
    });
    if sampling {
        button_border_trail(ui, rect);
    } else if let Some(done) = done {
        button_done_glow(ui, rect, done);
    }
}

pub fn auto_cal_tick(ctx: &egui::Context, canvas: &mut Canvas, live: &LiveSignals) {
    let candidates: Vec<(NodeId, String, bool, Vec<String>)> = canvas.snarl.node_ids()
        .filter(|(_, n)| n.module_id == "device.source"
            && n.params.get("gyro_auto_cal").and_then(|v| v.as_bool()).unwrap_or(false))
        .filter_map(|(id, n)| {
            let dev = n.params.get("device_id").and_then(|v| v.as_str())?.to_string();
            let skip_accel = n.params.get("skip_accel_cal")
                .and_then(|v| v.as_bool()).unwrap_or(false);
            Some((id, dev, skip_accel, node_pin_ids(n)))
        })
        .collect();

    for (node_id, dev_id, skip_accel, pin_ids) in candidates {
        if !caps_for_pins(&dev_id, &pin_ids).gyro { continue; }
        // Device disconnected → its pins vanish; read_float would return
        // steady zeros, which look perfectly "still". Require a live stream.
        if !live.contains_key(&(dev_id.clone(), "gyro_x".to_string())) { continue; }
        // A manual sampling session owns the offsets while it runs.
        let manual = ctx.data(|d| {
            d.get_temp::<WindowState>(state_id(node_id))
                .map(|s| s.gyro.sampling)
                .unwrap_or(false)
        });
        if manual { continue; }

        let raw = [
            read_float(live, &dev_id, "gyro_x"),
            read_float(live, &dev_id, "gyro_y"),
            read_float(live, &dev_id, "gyro_z"),
            read_float(live, &dev_id, "accel_x"),
            read_float(live, &dev_id, "accel_y"),
            read_float(live, &dev_id, "accel_z"),
        ];

        let fired = ctx.data_mut(|d| {
            let st = d.get_temp_mut_or_insert_with::<AutoCalState>(
                auto_cal_id(node_id), AutoCalState::default);
            let now = Instant::now();
            st.buf.push_back((now, raw));
            let keep = std::time::Duration::from_secs_f32(AUTOCAL_WINDOW_S * 1.25);
            while st.buf.front().map(|(t, _)| now.duration_since(*t) > keep).unwrap_or(false) {
                st.buf.pop_front();
            }
            let span = st.buf.front()
                .map(|(t, _)| now.duration_since(*t).as_secs_f32())
                .unwrap_or(0.0);
            if span < AUTOCAL_WINDOW_S || st.buf.len() < 30 { return None; }

            let mut mn = [f32::INFINITY; 6];
            let mut mx = [f32::NEG_INFINITY; 6];
            let mut mean = [0.0_f64; 6];
            for (_, s) in &st.buf {
                for i in 0..6 {
                    mn[i] = mn[i].min(s[i]);
                    mx[i] = mx[i].max(s[i]);
                    mean[i] += s[i] as f64;
                }
            }
            let n = st.buf.len() as f64;
            for m in &mut mean { *m /= n; }

            let still = (0..3).all(|i| mx[i] - mn[i] < AUTOCAL_GYRO_P2P)
                && (3..6).all(|i| mx[i] - mn[i] < AUTOCAL_ACCEL_P2P);
            if !still {
                // Movement re-arms the watcher for the next put-down.
                st.cooldown = false;
                return None;
            }
            if st.cooldown { return None; }
            let flat = mean[3].abs() < AUTOCAL_FLAT_XY
                && mean[4].abs() < AUTOCAL_FLAT_XY
                && (AUTOCAL_FLAT_Z.0..=AUTOCAL_FLAT_Z.1).contains(&(mean[5].abs() as f32));
            if !flat { return None; }

            let mut var = [0.0_f64; 6];
            for (_, s) in &st.buf {
                for i in 0..6 {
                    let dv = s[i] as f64 - mean[i];
                    var[i] += dv * dv;
                }
            }
            for v in &mut var { *v /= n; }
            st.cooldown = true;
            Some((mean, var))
        });
        let Some((mean, var)) = fired else { continue };

        // Same quantization snap as the manual path.
        const SNAP_EPS: f32 = 1.0 / 16384.0;
        let snap = |v: f64| -> f32 {
            let f = v as f32;
            if f.abs() < SNAP_EPS { 0.0 } else { f }
        };
        let g_floor = (var[0] + var[1] + var[2]).sqrt() as f32;
        let a_floor = ((var[3] + var[4]) * 1.5).sqrt() as f32;
        if let Some(nd) = canvas.snarl.get_node_mut(node_id) {
            nd.params.insert("gyro_offset".into(),
                serde_json::json!([snap(mean[0]), snap(mean[1]), snap(mean[2])]));
            if !skip_accel {
                nd.params.insert("accel_offset".into(),
                    serde_json::json!([snap(mean[3]), snap(mean[4]), 0.0_f32]));
            }
            nd.params.insert("gyro_calibrated".into(), Value::Bool(true));
            nd.params.insert("gyro_noise_floor".into(), serde_json::json!(g_floor));
            nd.params.insert("accel_noise_floor".into(), serde_json::json!(a_floor));
        }
        // Light the completion glow: on the device-card Calibrate buttons
        // always, and inside the calibration window if it's open.
        ctx.data_mut(|d| {
            d.insert_temp(cal_glow_id(node_id), Instant::now());
            if let Some(mut st) = d.get_temp::<WindowState>(state_id(node_id)) {
                st.gyro.done_at = Some(Instant::now());
                d.insert_temp(state_id(node_id), st);
            }
        });
    }
}

fn draw_window_body(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    node_id: NodeId,
    dev_id: &str,
    caps: CalCaps,
    live: &LiveSignals,
    scope_taps: &flexinput_engine::ScopeTaps,
    spike_settings: &SpikeSettings,
) {
    let lo = Layout::compute(ui);
    if caps.gyro {
        gyro_section(ui, canvas, node_id, dev_id, live, lo, scope_taps, spike_settings);
        ui.separator();
    }
    if caps.orientation {
        orientation_section(ui, canvas, node_id, dev_id, live);
        ui.separator();
    }
    if caps.sticks {
        sticks_section(ui, canvas, node_id, dev_id, live, lo, caps);
        ui.separator();
    }
    if caps.triggers {
        triggers_section(ui, canvas, node_id, dev_id, live, lo);
    }
    ui.ctx().request_repaint();
}

/// Frames averaged for the orientation baseline. ~1 s of a resting device at
/// the Joy-Con 2's ~67 Hz — long enough to average out jitter, short enough
/// that the user is not asked to hold still for an age.
const ORIENT_BASELINE_FRAMES: u32 = 64;

/// Baseline-pose capture for devices that report absolute orientation.
///
/// The published pose is world-referenced — on a Joy-Con 2 its yaw is magnetic
/// north — which is almost never what a patch wants. Capturing a reference
/// while the device sits in a known pose lets the engine report rotation
/// RELATIVE to that, so "as I set it down" reads as identity.
fn orientation_section(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    node_id: NodeId,
    dev_id: &str,
    live: &LiveSignals,
) {
    let done = canvas.snarl.get_node(node_id)
        .and_then(|n| n.params.get("orientation_baseline"))
        .and_then(|v| v.as_array())
        .is_some_and(|a| a.len() == 4);
    section_header(ui, "Orientation Baseline", done);

    instruct_line(ui, &[
        (false, "Clip BOTH halves into the grip / charger, lay it "),
        (true,  "FLAT"),
        (false, " on the table, then capture. Do not touch it while it samples."),
    ]);

    let live_q = read_vec4(live, dev_id, "orientation");

    let (sampling, n, sum) = ui.ctx().data_mut(|d| {
        let st = d.get_temp_mut_or_insert_with::<OrientBaseline>(
            orient_baseline_id(node_id), OrientBaseline::default);
        if st.sampling {
            if let Some(q) = live_q {
                // Align each sample's sign to the first before averaging: q and
                // -q are the SAME rotation, and a device that flips
                // representation mid-capture would otherwise average toward
                // zero and produce a garbage baseline.
                let s = if st.n == 0 {
                    1.0
                } else {
                    let dot = st.sum[0] * q[0] + st.sum[1] * q[1] + st.sum[2] * q[2] + st.sum[3] * q[3];
                    if dot < 0.0 { -1.0 } else { 1.0 }
                };
                for i in 0..4 { st.sum[i] += q[i] * s; }
                st.n += 1;
            }
            if st.n >= ORIENT_BASELINE_FRAMES {
                st.sampling = false;
            }
        }
        (st.sampling, st.n, st.sum)
    });

    if !sampling && n >= ORIENT_BASELINE_FRAMES {
        let len = (sum[0].powi(2) + sum[1].powi(2) + sum[2].powi(2) + sum[3].powi(2)).sqrt();
        if len > f32::EPSILON {
            let q: Vec<f64> = sum.iter().map(|v| (v / len) as f64).collect();
            if let Some(nd) = canvas.snarl.get_node_mut(node_id) {
                nd.params.insert("orientation_baseline".into(), serde_json::json!(q));
            }
        }
        ui.ctx().data_mut(|d| {
            d.insert_temp(orient_baseline_id(node_id), OrientBaseline::default());
        });
    }

    ui.horizontal(|ui| {
        let progress = if sampling { n as f32 / ORIENT_BASELINE_FRAMES as f32 } else { 0.0 };
        let label = if sampling { "Sampling…" } else { "Capture Baseline" };
        let enabled = live_q.is_some();
        let resp = ui.add_enabled_ui(enabled, |ui| {
            cal_button_with_progress(ui, label, progress)
        }).inner;
        if resp.clicked() && !sampling {
            ui.ctx().data_mut(|d| {
                d.insert_temp(orient_baseline_id(node_id), OrientBaseline {
                    sampling: true, n: 0, sum: [0.0; 4],
                });
            });
        }
        if !enabled {
            ui.label(egui::RichText::new("no orientation signal from this device")
                .size(INSTRUCT_SIZE).color(ORANGE));
        }
        if done && ui.button("Clear").clicked() {
            if let Some(nd) = canvas.snarl.get_node_mut(node_id) {
                nd.params.remove("orientation_baseline");
            }
        }
    });
}

#[derive(Clone, Copy, Default)]
struct OrientBaseline {
    sampling: bool,
    n: u32,
    sum: [f32; 4],
}

fn orient_baseline_id(node: NodeId) -> egui::Id { egui::Id::new(("orient_baseline", node)) }

/// Read a Vec4 pin, or `None` when the device is not publishing it.
fn read_vec4(live: &LiveSignals, dev: &str, pin: &str) -> Option<[f32; 4]> {
    match live.get(&(dev.to_string(), pin.to_string())) {
        Some(flexinput_core::Signal::Vec4(v)) => Some([v.x, v.y, v.z, v.w]),
        _ => None,
    }
}

fn section_header(ui: &mut egui::Ui, title: &str, done: bool) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).heading());
        if done {
            ui.label(egui::RichText::new("✓")
                .color(Color32::from_rgb(80, 200, 100)).heading());
        }
    });
    ui.add_space(2.0);
}

fn cal_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add_sized([100.0, 30.0], egui::Button::new(
        egui::RichText::new(label).size(15.0)
    ))
}

/// Like [`cal_button`] but with an orange progress fill animating left→right
/// on top of the button when `progress` ∈ (0.0, 1.0). Used during gyro
/// calibration so the user gets visual feedback without a separate
/// "Sampling… N/M" label causing layout jitter.
fn cal_button_with_progress(ui: &mut egui::Ui, label: &str, progress: f32) -> egui::Response {
    let resp = cal_button(ui, label);
    let p = progress.clamp(0.0, 1.0);
    if p > 0.0 {
        let rect = resp.rect;
        let fill_w = rect.width() * p;
        let fill_rect = egui::Rect::from_min_size(
            rect.min,
            egui::vec2(fill_w, rect.height()),
        );
        // Translucent orange so the underlying button glyphs stay visible
        // and the bar reads as a fill rather than a coverup.
        ui.painter().rect_filled(
            fill_rect,
            ui.style().visuals.widgets.active.corner_radius,
            ORANGE.gamma_multiply(0.35),
        );
    }
    resp
}

/// Generic variant — same progress overlay, but for arbitrary-sized buttons
/// (the axis-alignment Roll/Pitch/Yaw rotate buttons are taller than the
/// standard cal button). Renders the supplied button and overlays the bar.
fn add_button_with_progress(ui: &mut egui::Ui, button: egui::Button<'_>, progress: f32) -> egui::Response {
    let resp = ui.add(button);
    let p = progress.clamp(0.0, 1.0);
    if p > 0.0 {
        let rect = resp.rect;
        let fill_w = rect.width() * p;
        let fill_rect = egui::Rect::from_min_size(
            rect.min,
            egui::vec2(fill_w, rect.height()),
        );
        ui.painter().rect_filled(
            fill_rect,
            ui.style().visuals.widgets.active.corner_radius,
            ORANGE.gamma_multiply(0.35),
        );
    }
    resp
}

/// Orange trail chasing around a button's border — the "currently
/// calibrating" indicator. Head is brightest, tail fades out over ~30 % of
/// the perimeter; one lap ≈ 1.5 s.
fn button_border_trail(ui: &egui::Ui, rect: egui::Rect) {
    let t = ui.input(|i| i.time) as f32;
    let head = (t * 0.65).fract();
    let w = rect.width();
    let h = rect.height();
    let per = 2.0 * (w + h);
    // Walk the rect perimeter clockwise from the top-left corner, u ∈ 0..1.
    let point_at = |u: f32| -> Pos2 {
        let d = u.rem_euclid(1.0) * per;
        if d < w {
            egui::pos2(rect.left() + d, rect.top())
        } else if d < w + h {
            egui::pos2(rect.right(), rect.top() + (d - w))
        } else if d < 2.0 * w + h {
            egui::pos2(rect.right() - (d - w - h), rect.bottom())
        } else {
            egui::pos2(rect.left(), rect.bottom() - (d - 2.0 * w - h))
        }
    };
    const SEGS: usize = 26;
    const TRAIL: f32 = 0.30;
    let painter = ui.painter();
    for k in 0..SEGS {
        let f0 = k as f32 / SEGS as f32;
        let f1 = (k + 1) as f32 / SEGS as f32;
        let p0 = point_at(head - f0 * TRAIL);
        let p1 = point_at(head - f1 * TRAIL);
        // Segments are short enough that one spanning a corner just cuts it.
        painter.line_segment([p0, p1],
            Stroke::new(2.0, ORANGE.gamma_multiply(1.0 - f1)));
    }
    ui.ctx().request_repaint();
}

/// Green border glow fading out over ~2 s after a calibration finishes.
fn button_done_glow(ui: &egui::Ui, rect: egui::Rect, done_at: Instant) {
    const DUR: f32 = 2.0;
    let el = done_at.elapsed().as_secs_f32();
    if el >= DUR { return; }
    let a = (1.0 - el / DUR).clamp(0.0, 1.0);
    let g = Color32::from_rgb(80, 200, 100);
    let painter = ui.painter();
    painter.rect_stroke(rect.expand(1.0), 4.0,
        Stroke::new(2.0, g.gamma_multiply(a)),
        egui::epaint::StrokeKind::Outside);
    painter.rect_stroke(rect.expand(3.5), 6.0,
        Stroke::new(3.0, g.gamma_multiply(a * 0.35)),
        egui::epaint::StrokeKind::Outside);
    ui.ctx().request_repaint();
}

/// One paragraph of instruction text at the larger calibration-window size.
/// `segments` is an alternating sequence of (orange, normal) chunks rendered
/// inline. Pass `(true, "...")` for words you want highlighted, `(false, "...")`
/// for the rest.
fn instruct_line(ui: &mut egui::Ui, segments: &[(bool, &str)]) {
    let mut text = egui::text::LayoutJob::default();
    let weak = ui.style().visuals.weak_text_color();
    for (orange, chunk) in segments {
        let color = if *orange { ORANGE } else { weak };
        text.append(chunk, 0.0, egui::TextFormat {
            font_id: egui::FontId::proportional(INSTRUCT_SIZE),
            color,
            ..Default::default()
        });
    }
    ui.label(text);
}

/// Same as instruct_line, but with a hanging-indent step number rendered at
/// the start. Used for the numbered "1. Click Start." style lists.
fn instruct_step(ui: &mut egui::Ui, num: u32, segments: &[(bool, &str)]) {
    let mut joined: Vec<(bool, String)> = Vec::with_capacity(segments.len() + 1);
    joined.push((true, format!("{}. ", num)));
    for (o, s) in segments { joined.push((*o, (*s).to_string())); }
    let refs: Vec<(bool, &str)> = joined.iter().map(|(o, s)| (*o, s.as_str())).collect();
    instruct_line(ui, &refs);
}

// ── Gyro section ─────────────────────────────────────────────────────────────

fn gyro_section(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    node_id: NodeId,
    dev_id: &str,
    live: &LiveSignals,
    lo: Layout,
    scope_taps: &flexinput_engine::ScopeTaps,
    spike_settings: &SpikeSettings,
) {
    let done = canvas.snarl.get_node(node_id)
        .and_then(|n| n.params.get("gyro_calibrated").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let _ = done; // header check intentionally suppressed per new design
    section_header(ui, "Gyro Calibration", false);

    // Raw samples this frame.
    let raw_gx = read_float(live, dev_id, "gyro_x");
    let raw_gy = read_float(live, dev_id, "gyro_y");
    let raw_gz = read_float(live, dev_id, "gyro_z");
    let raw_ax = read_float(live, dev_id, "accel_x");
    let raw_ay = read_float(live, dev_id, "accel_y");

    // Current calibration applied at display time (same math as engine).
    let g_off = read_param_arr::<3>(canvas, node_id, "gyro_offset", [0.0; 3]);
    let a_off = read_param_arr::<3>(canvas, node_id, "accel_offset", [0.0; 3]);
    let disp = [
        raw_gx - g_off[0],
        raw_gy - g_off[1],
        raw_gz - g_off[2],
        raw_ax - a_off[0],
        raw_ay - a_off[1],
    ];

    // Step the running mean over RAW values; outliers reject 3σ from current mean.
    // While sampling, we push fresh offsets to the node every frame so the
    // displayed scope visibly converges on 0 — except gz (gravity-coupled
    // in some mounts) and az, which we never write.
    let (sampling_now, n_now, gyro_done_at) = ui.ctx().data_mut(|d| {
        let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(node_id), WindowState::default);
        st.gyro.scope.push(disp);
        let raw = [raw_gx, raw_gy, raw_gz, raw_ax, raw_ay];
        let mut bumped = false;
        if st.gyro.sampling {
            for i in 0..5 {
                let x = raw[i] as f64;
                let n_f = (st.gyro.n + 1) as f64;
                let mean = st.gyro.mean[i];
                let m2   = st.gyro.m2[i];
                let var  = if st.gyro.n > 16 { m2 / (st.gyro.n as f64) } else { 0.0 };
                let sd   = var.sqrt() as f32;
                if st.gyro.n > 32 && sd > 1e-6 && (raw[i] - mean as f32).abs() > 3.0 * sd {
                    continue; // outlier
                }
                let delta = x - mean;
                let new_mean = mean + delta / n_f;
                st.gyro.mean[i] = new_mean;
                st.gyro.m2[i]   = m2 + delta * (x - new_mean);
                if i == 0 { st.gyro.n += 1; bumped = true; }
            }
            // Quantization-aware: snap means closer to 0 if they fall within
            // half a quantization step of their integer LSB grid. For gyro
            // (±2000 dps → ±1.0 graph) the LSB ≈ 2000/32767/2000 ≈ 3.05e-5;
            // for accel (±8 G → ±1.0 graph) LSB ≈ 8/32767/8 ≈ 3.05e-5. We
            // use a slightly wider window so small jitter biases don't move
            // the offset farther than the true center.
            const SNAP_EPS: f32 = 1.0 / 16384.0;
            let mut writeback = [0.0_f32; 3];
            for i in 0..3 {
                let m = st.gyro.mean[i] as f32;
                writeback[i] = if m.abs() < SNAP_EPS { 0.0 } else { m };
            }
            let mut a_write = [0.0_f32; 3];
            for i in 0..2 { // ax, ay
                let m = st.gyro.mean[3 + i] as f32;
                a_write[i] = if m.abs() < SNAP_EPS { 0.0 } else { m };
            }
            // gz / az are gravity-coupled in some orientations; never write.
            if bumped {
                if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                    n.params.insert("gyro_offset".into(), serde_json::json!(writeback));
                    // Skip accelerometer writes if the user toggled that off.
                    let skip_accel = n.params.get("skip_accel_cal")
                        .and_then(|v| v.as_bool()).unwrap_or(false);
                    if !skip_accel {
                        n.params.insert("accel_offset".into(), serde_json::json!(a_write));
                    }
                    // Noise floors from the running Welford variance — written
                    // every frame once the estimate is meaningful, so the
                    // scope's deadzone brackets visibly converge into place
                    // while sampling runs. Gyro floor = RMS magnitude of the
                    // 3-axis noise (the engine gates on |g|); accel floor
                    // scales the 2 sampled axes up to a 3-axis distance.
                    if st.gyro.n > 16 {
                        let var = |i: usize| st.gyro.m2[i] / st.gyro.n as f64;
                        let g_floor = (var(0) + var(1) + var(2)).sqrt() as f32;
                        let a_floor = ((var(3) + var(4)) * 1.5).sqrt() as f32;
                        n.params.insert("gyro_noise_floor".into(), serde_json::json!(g_floor));
                        n.params.insert("accel_noise_floor".into(), serde_json::json!(a_floor));
                    }
                }
            }
            if st.gyro.n >= GYRO_SAMPLE_FRAMES {
                st.gyro.sampling = false;
                st.gyro.done_at = Some(Instant::now());
                if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                    n.params.insert("gyro_calibrated".into(), Value::Bool(true));
                }
            }
        }
        (st.gyro.sampling, st.gyro.n, st.gyro.done_at)
    });

    // Push the maximum repaint rate during sampling so the scope captures as
    // many of the I/O thread's polling-rate samples as the UI can deliver.
    if sampling_now { ui.ctx().request_repaint(); }

    // Hoist the skip-accel flag so both the inner UI and gyro_scope can see it.
    let skip_accel_initial = canvas.snarl.get_node(node_id)
        .and_then(|n| n.params.get("skip_accel_cal").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    // Read axis-alignment state once — used by both the Axis alignment
    // button row in the left column and the sample-collection block at
    // the end of this function.
    let (or_sampling_axis, or_n, or_captured) = ui.ctx().data(|d| {
        d.get_temp::<WindowState>(state_id(node_id))
            .map(|s| (s.orient.sampling_axis, s.orient.sampling_n, s.orient.captured))
            .unwrap_or((None, 0, [false; 3]))
    });
    let axis_names = ["Roll", "Pitch", "Yaw"];
    let axis_hints = ["tilt side-to-side", "rock front-to-back", "rotate like a wheel"];

    ui.horizontal(|ui| {
        // ── LEFT COLUMN ────────────────────────────────────────────────
        ui.vertical(|ui| {
            ui.set_width(lo.left_w);

            // Instructions paragraph.
            instruct_line(ui, &[(false, "Before beginning calibration")]);
            instruct_line(ui, &[(true, "put your gamepad flat on level surface.")]);
            ui.add_space(6.0);

            // start / reset / ✓ skip accelerometer — all on one line.
            // The start button gains an orange progress overlay during
            // sampling so there's no separate "Sampling… N/M" label
            // shifting the layout below.
            ui.horizontal(|ui| {
                let gyro_progress = if sampling_now {
                    (n_now as f32 / GYRO_SAMPLE_FRAMES as f32).clamp(0.0, 1.0)
                } else { 0.0 };
                let start_resp = cal_button_with_progress(
                    ui,
                    if sampling_now { "stop" } else { "start" },
                    gyro_progress,
                );
                if sampling_now {
                    button_border_trail(ui, start_resp.rect);
                } else if let Some(done) = gyro_done_at {
                    button_done_glow(ui, start_resp.rect, done);
                }
                if start_resp.clicked() {
                    ui.ctx().data_mut(|d| {
                        let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(node_id), WindowState::default);
                        if sampling_now {
                            st.gyro.sampling = false;
                        } else {
                            st.gyro = GyroSession { sampling: true, ..Default::default() };
                            if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                                n.params.insert("gyro_offset".into(), serde_json::json!([0.0_f32, 0.0, 0.0]));
                                n.params.insert("accel_offset".into(), serde_json::json!([0.0_f32, 0.0, 0.0]));
                                n.params.insert("gyro_calibrated".into(), Value::Bool(false));
                            }
                        }
                    });
                }
                if cal_button(ui, "reset").clicked() {
                    if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                        n.params.insert("gyro_offset".into(), serde_json::json!([0.0_f32, 0.0, 0.0]));
                        n.params.insert("accel_offset".into(), serde_json::json!([0.0_f32, 0.0, 0.0]));
                        n.params.insert("gyro_calibrated".into(), Value::Bool(false));
                    }
                }
                let mut skip_accel = skip_accel_initial;
                if ui.checkbox(&mut skip_accel,
                    egui::RichText::new("skip accelerometer").size(INSTRUCT_SIZE - 1.0))
                    .changed()
                {
                    if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                        n.params.insert("skip_accel_cal".into(), Value::Bool(skip_accel));
                        if skip_accel {
                            n.params.insert("accel_offset".into(), serde_json::json!([0.0_f32, 0.0, 0.0]));
                        }
                    }
                }
            });

            // Hands-free auto-calibration: a watcher on the app frame (runs
            // with this window closed too) recalibrates whenever the pad has
            // been lying flat and motionless for a couple of seconds.
            {
                let mut auto_on = canvas.snarl.get_node(node_id)
                    .and_then(|n| n.params.get("gyro_auto_cal").and_then(|v| v.as_bool()))
                    .unwrap_or(false);
                if ui.checkbox(&mut auto_on,
                    egui::RichText::new("auto-calibrate when still").size(INSTRUCT_SIZE - 1.0))
                    .on_hover_text("Recalibrates hands-free after the controller has been lying\n\
                                    flat on a level surface, fully motionless, for 5 seconds —\n\
                                    even while this window is closed. Any movement during the\n\
                                    wait restarts it; the write is atomic (never half-done).\n\
                                    Re-arms once the controller is picked up again.")
                    .changed()
                {
                    if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                        n.params.insert("gyro_auto_cal".into(), Value::Bool(auto_on));
                    }
                }
            }

            ui.add_space(8.0);

            // "Axis alignment" header + inline solve / reset buttons.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Axis alignment").size(INSTRUCT_SIZE).strong());
                let all = or_captured.iter().all(|c| *c);
                if ui.add_enabled(all && or_sampling_axis.is_none(),
                    egui::Button::new(egui::RichText::new("solve").size(INSTRUCT_SIZE - 1.0))).clicked()
                {
                    let (samples, accel_mean) = ui.ctx().data(|d| {
                        let st = d.get_temp::<WindowState>(state_id(node_id));
                        let samples = st.as_ref().map(|s| s.orient.samples.clone()).unwrap_or_default();
                        let accel_mean = st.as_ref().and_then(|s| {
                            let mut sum = [0.0_f64; 3];
                            let mut n   = 0_u64;
                            for ai in 0..3 {
                                if !s.orient.captured[ai] || s.orient.accel_n[ai] == 0 { continue; }
                                let inv = 1.0 / s.orient.accel_n[ai] as f64;
                                sum[0] += s.orient.accel_sum[ai][0] * inv;
                                sum[1] += s.orient.accel_sum[ai][1] * inv;
                                sum[2] += s.orient.accel_sum[ai][2] * inv;
                                n += 1;
                            }
                            if n == 0 { return None; }
                            let k = 1.0 / n as f64;
                            Some([(sum[0] * k) as f32, (sum[1] * k) as f32, (sum[2] * k) as f32])
                        });
                        (samples, accel_mean)
                    });
                    if let Some(m) = solve_orient_matrix(&samples, accel_mean) {
                        if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                            n.params.insert("gyro_orient_matrix".into(), serde_json::json!(m.to_vec()));
                        }
                    }
                }
                if ui.button(egui::RichText::new("reset").size(INSTRUCT_SIZE - 1.0)).clicked() {
                    if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                        n.params.remove("gyro_orient_matrix");
                    }
                    ui.ctx().data_mut(|d| {
                        let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(node_id), WindowState::default);
                        st.orient = OrientSession::default();
                    });
                }
            });

            // Roll / Pitch / Yaw axis-capture buttons — three across.
            // The active button gets an orange progress overlay; the
            // "N/M" sampling text is dropped because the fill IS the
            // progress indicator (and avoids button-label width jitter).
            ui.horizontal(|ui| {
                for ai in 0..3 {
                    let active = or_sampling_axis == Some(ai);
                    let core = if or_captured[ai] {
                        format!("{} ✓", axis_names[ai])
                    } else {
                        axis_names[ai].to_string()
                    };
                    let label = format!("{}\n{}", core, axis_hints[ai]);
                    let btn = egui::Button::new(egui::RichText::new(label).size(INSTRUCT_SIZE - 1.0));
                    let progress = if active {
                        (or_n as f32 / ORIENT_SAMPLE_FRAMES as f32).clamp(0.0, 1.0)
                    } else { 0.0 };
                    let enabled = or_sampling_axis.is_none() || active;
                    let resp = if enabled {
                        add_button_with_progress(ui, btn, progress)
                    } else {
                        ui.add_enabled(false, btn)
                    };
                    if resp.clicked() {
                        ui.ctx().data_mut(|d| {
                            let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(node_id), WindowState::default);
                            if active {
                                st.orient.sampling_axis = None;
                                st.orient.sampling_n = 0;
                            } else {
                                st.orient.sampling_axis = Some(ai);
                                st.orient.sampling_n = 0;
                                st.orient.samples[ai].clear();
                                st.orient.captured[ai] = false;
                            }
                        });
                    }
                }
            });

            ui.add_space(4.0);
            ui.label(egui::RichText::new(
                "if normal calibration method doesn't help\n\
                 or you have some IMU misalignment:\n\
                 do pure single-axis rotation per button, then solve")
                .size(INSTRUCT_SIZE - 2.0)
                .color(Color32::from_gray(140)));
        });

        // ── RIGHT COLUMN ───────────────────────────────────────────────
        ui.vertical(|ui| {
            ui.set_width(lo.right_w);

            // Oscilloscope.
            gyro_scope(ui, node_id, dev_id, lo, skip_accel_initial, canvas, scope_taps);

            // Outlier spike-filter row — directly beneath the scope so
            // dragging sensitivity gives instant visual feedback on the
            // trace above.
            let mut enabled = canvas.snarl.get_node(node_id)
                .and_then(|n| n.params.get("spike_suppress").and_then(|v| v.as_bool()))
                .unwrap_or(true);
            let mut sens = canvas.snarl.get_node(node_id)
                .and_then(|n| n.params.get("spike_sensitivity").and_then(|v| v.as_f64()))
                .unwrap_or(50.0) as f32;
            let mut filter_changed = false;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(egui::RichText::new("Outlier spike filter")
                    .size(INSTRUCT_SIZE).strong());
                if ui.checkbox(&mut enabled, "")
                    .on_hover_text("Suppress single-packet outlier spikes at the HID polling layer.\n\
                                    Spikes that get through still show on the scope above —\n\
                                    tune sensitivity until the trace is clean.")
                    .changed() { filter_changed = true; }
                ui.add_enabled_ui(enabled, |ui| {
                    ui.label(egui::RichText::new("sensitivity").weak());
                    if ui.add(egui::Slider::new(&mut sens, 0.0..=100.0)
                        .suffix(" %").show_value(true))
                        .on_hover_text("0 % = filter off (raw samples pass through).\n\
                                        100 % = aggressive (catches near-noise excursions).")
                        .changed() { filter_changed = true; }
                });
            });
            if filter_changed {
                if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                    n.params.insert("spike_suppress".into(), Value::Bool(enabled));
                    n.params.insert("spike_sensitivity".into(),
                        serde_json::Number::from_f64(sens as f64)
                            .map(Value::Number).unwrap_or(Value::Null));
                }
            }
            if let Ok(mut map) = spike_settings.write() {
                map.insert(dev_id.to_string(), (enabled, sens));
            }

            // Noise-floor deadzone row: zero out readings inside the measured
            // noise band. The floor itself comes from calibration (manual or
            // auto); the width factor scales how far the brackets sit around
            // it. Gyro gates on the 3-axis magnitude at center; accel gets a
            // dynamic band that follows gravity as the device rotates.
            {
                let (mut dz_on, mut dz_w, g_floor, mut dz_view) = canvas.snarl.get_node(node_id)
                    .map(|n| (
                        n.params.get("gyro_dz_enabled").and_then(|v| v.as_bool()).unwrap_or(false),
                        n.params.get("gyro_dz_width").and_then(|v| v.as_f64()).unwrap_or(2.0) as f32,
                        n.params.get("gyro_noise_floor").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                        n.params.get("dz_view_after").and_then(|v| v.as_bool()).unwrap_or(false),
                    ))
                    .unwrap_or((false, 2.0, 0.0, false));
                let mut dz_changed = false;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(egui::RichText::new("Noise-floor deadzone")
                        .size(INSTRUCT_SIZE).strong());
                    if ui.checkbox(&mut dz_on, "")
                        .on_hover_text("Zero gyro readings whose 3-axis magnitude stays inside the\n\
                                        measured noise band (kills orientation drift and lean bias\n\
                                        at rest), and hold the accel steady inside a band that\n\
                                        follows gravity as the device rotates. The border eases in\n\
                                        exponentially — no snap when crossing it.")
                        .changed() { dz_changed = true; }
                    ui.add_enabled_ui(dz_on, |ui| {
                        ui.label(egui::RichText::new("width ×").weak());
                        if ui.add(egui::DragValue::new(&mut dz_w)
                            .speed(0.05).range(0.05..=10.0))
                            .on_hover_text("Bracket width as a multiple of the measured noise floor.\n\
                                            2.0 clears ~99 % of noise; raise it if drift persists,\n\
                                            lower it if slow motions get eaten.")
                            .changed() { dz_changed = true; }
                        if g_floor > 0.0 {
                            ui.label(egui::RichText::new(format!("±{:.4}", g_floor * dz_w))
                                .size(INSTRUCT_SIZE - 1.0).weak());
                        } else {
                            ui.label(egui::RichText::new("calibrate first")
                                .size(INSTRUCT_SIZE - 1.0)
                                .color(Color32::from_gray(120)));
                        }
                        if ui.checkbox(&mut dz_view,
                            egui::RichText::new("view filtered").size(INSTRUCT_SIZE - 2.0))
                            .on_hover_text("Show the scope traces AFTER the deadzone (what the\n\
                                            engine outputs) instead of the raw readings.")
                            .changed() { dz_changed = true; }
                    });
                });
                if dz_changed {
                    if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                        n.params.insert("gyro_dz_enabled".into(), Value::Bool(dz_on));
                        n.params.insert("gyro_dz_width".into(),
                            serde_json::Number::from_f64(dz_w as f64)
                                .map(Value::Number).unwrap_or(Value::Null));
                        n.params.insert("dz_view_after".into(), Value::Bool(dz_view));
                    }
                }
            }

            // Invert axis — single row: Gyro: X Y Z   Accel: X Y Z.
            let g_inv = read_invert3(canvas, node_id, "gyro_invert");
            let a_inv = read_invert3(canvas, node_id, "accel_invert");
            let mut g_new = g_inv;
            let mut a_new = a_inv;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Invert axis").size(INSTRUCT_SIZE).strong());
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Gyro:").size(INSTRUCT_SIZE).weak());
                ui.checkbox(&mut g_new[0], egui::RichText::new("X").size(INSTRUCT_SIZE));
                ui.checkbox(&mut g_new[1], egui::RichText::new("Y").size(INSTRUCT_SIZE));
                ui.checkbox(&mut g_new[2], egui::RichText::new("Z").size(INSTRUCT_SIZE));
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Accel:").size(INSTRUCT_SIZE).weak());
                ui.checkbox(&mut a_new[0], egui::RichText::new("X").size(INSTRUCT_SIZE));
                ui.checkbox(&mut a_new[1], egui::RichText::new("Y").size(INSTRUCT_SIZE));
                ui.checkbox(&mut a_new[2], egui::RichText::new("Z").size(INSTRUCT_SIZE));
            });
            if g_new != g_inv {
                if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                    n.params.insert("gyro_invert".into(), serde_json::json!(g_new));
                }
            }
            if a_new != a_inv {
                if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                    n.params.insert("accel_invert".into(), serde_json::json!(a_new));
                }
            }
        });
    });
    // Collect samples for the active axis (UI-frame rate is fine — PCA
    // only needs a handful of well-separated vectors). Also accumulate
    // a running mean accel vector so the solver can use gravity as an
    // absolute reference and refine PCA's wobbly axis estimate.
    if let Some(ai) = or_sampling_axis {
        let gx = read_float(live, dev_id, "gyro_x");
        let gy = read_float(live, dev_id, "gyro_y");
        let gz = read_float(live, dev_id, "gyro_z");
        let ax = read_float(live, dev_id, "accel_x");
        let ay = read_float(live, dev_id, "accel_y");
        let az = read_float(live, dev_id, "accel_z");
        let done = ui.ctx().data_mut(|d| {
            let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(node_id), WindowState::default);
            let mag = (gx * gx + gy * gy + gz * gz).sqrt();
            if mag > 0.01 {
                st.orient.samples[ai].push([gx, gy, gz]);
            }
            // Accumulate accel UNCONDITIONALLY — even at rest it reads
            // gravity, which is exactly the signal we want for the
            // gravity-alignment refinement.
            st.orient.accel_sum[ai][0] += ax as f64;
            st.orient.accel_sum[ai][1] += ay as f64;
            st.orient.accel_sum[ai][2] += az as f64;
            st.orient.accel_n[ai]      += 1;
            st.orient.sampling_n += 1;
            if st.orient.sampling_n >= ORIENT_SAMPLE_FRAMES {
                let enough = st.orient.samples[ai].len() >= 16;
                st.orient.captured[ai] = enough;
                st.orient.sampling_axis = None;
                st.orient.sampling_n = 0;
                true
            } else { false }
        });
        if !done { ui.ctx().request_repaint(); }
    }
}

fn gyro_scope(
    ui: &mut egui::Ui,
    _node_id: NodeId,
    dev_id: &str,
    lo: Layout,
    skip_accel: bool,
    canvas: &Canvas,
    scope_taps: &flexinput_engine::ScopeTaps,
) {
    // Width = right column extent; height = shortened gyro height per spec.
    let (rect, _) = ui.allocate_exact_size(egui::vec2(lo.right_w, lo.gyro_h), Sense::hover());

    // Read the per-window scope-window setting (default 1000 ms).
    let scope_win_ms = ui.ctx().data(|d| {
        d.get_temp::<WindowState>(state_id(_node_id))
            .map(|s| s.scope_win_ms)
            .unwrap_or(1000.0)
    });

    // Bottom strip reserved for legend + clickable time-window indicator.
    const LEGEND_STRIP_H: f32 = 22.0;
    let scope_rect = egui::Rect::from_min_max(
        rect.min,
        egui::pos2(rect.max.x, rect.max.y - LEGEND_STRIP_H),
    );
    let legend_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x, scope_rect.max.y),
        rect.max,
    );

    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, Color32::from_gray(15));
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_gray(50)),
        egui::epaint::StrokeKind::Inside);

    // Read raw poll-rate samples directly from the I/O thread's scope-tap rings.
    // Apply the same calibration math the engine uses (offset → matrix → sign)
    // so the trace visibly converges toward 0 as offsets are written during
    // sampling, AND the orientation-matrix solve takes visible effect on the
    // scope right away.
    let g_off = read_param_arr::<3>(canvas, _node_id, "gyro_offset", [0.0; 3]);
    let a_off = read_param_arr::<3>(canvas, _node_id, "accel_offset", [0.0; 3]);
    let g_inv = read_invert3(canvas, _node_id, "gyro_invert");
    let a_inv = read_invert3(canvas, _node_id, "accel_invert");
    let g_sign = [
        if g_inv[0] { -1.0_f32 } else { 1.0 },
        if g_inv[1] { -1.0_f32 } else { 1.0 },
        if g_inv[2] { -1.0_f32 } else { 1.0 },
    ];
    let a_sign = [
        if a_inv[0] { -1.0_f32 } else { 1.0 },
        if a_inv[1] { -1.0_f32 } else { 1.0 },
        if a_inv[2] { -1.0_f32 } else { 1.0 },
    ];

    // Read the 3×3 orientation matrix (row-major). Identity = no-op.
    let mut orient_m: [f32; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let mut orient_active = false;
    if let Some(arr) = canvas.snarl.get_node(_node_id)
        .and_then(|n| n.params.get("gyro_orient_matrix"))
        .and_then(|v| v.as_array())
    {
        if arr.len() == 9 {
            for i in 0..9 {
                if let Some(v) = arr[i].as_f64() { orient_m[i] = v as f32; }
            }
            for i in 0..9 {
                let id = if i == 0 || i == 4 || i == 8 { 1.0 } else { 0.0 };
                if (orient_m[i] - id).abs() > 0.01 { orient_active = true; break; }
            }
        }
    }
    let apply_m = |v: [f32; 3]| -> [f32; 3] {
        [
            orient_m[0] * v[0] + orient_m[1] * v[1] + orient_m[2] * v[2],
            orient_m[3] * v[0] + orient_m[4] * v[1] + orient_m[5] * v[2],
            orient_m[6] * v[0] + orient_m[7] * v[1] + orient_m[8] * v[2],
        ]
    };

    // The gyro/accel samples need to be matrix-transformed as a TRIPLE,
    // not per-axis. So we pull all 3 gyro pin rings, time-align by index,
    // run them through the matrix, then split back into per-channel lists.
    let pin_for_ch = ["gyro_x", "gyro_y", "gyro_z", "accel_x", "accel_y"];
    let off_for_ch = [g_off[0], g_off[1], g_off[2], a_off[0], a_off[1]];
    let sgn_for_ch = [g_sign[0], g_sign[1], g_sign[2], a_sign[0], a_sign[1]];
    let mut samples_ch: [Vec<(Instant, f32)>; 5] = Default::default();
    {
        let taps = scope_taps.read().unwrap();
        if orient_active {
            // Pull gyro_{x,y,z} as parallel rings; matrix-transform per
            // index. Ring lengths should match because they're written
            // in lockstep from the same I/O loop iteration.
            let gx_ring = taps.get(&(dev_id.to_string(), "gyro_x".into())).cloned().unwrap_or_default();
            let gy_ring = taps.get(&(dev_id.to_string(), "gyro_y".into())).cloned().unwrap_or_default();
            let gz_ring = taps.get(&(dev_id.to_string(), "gyro_z".into())).cloned().unwrap_or_default();
            let n = gx_ring.len().min(gy_ring.len()).min(gz_ring.len());
            for i in 0..n {
                let (t, gx) = gx_ring[i];
                let (_, gy) = gy_ring[i];
                let (_, gz) = gz_ring[i];
                let v = [gx - g_off[0], gy - g_off[1], gz - g_off[2]];
                let v = apply_m(v);
                samples_ch[0].push((t, v[0] * g_sign[0]));
                samples_ch[1].push((t, v[1] * g_sign[1]));
                samples_ch[2].push((t, v[2] * g_sign[2]));
            }
            // Accel: same dance but only X/Y are shown.
            let ax_ring = taps.get(&(dev_id.to_string(), "accel_x".into())).cloned().unwrap_or_default();
            let ay_ring = taps.get(&(dev_id.to_string(), "accel_y".into())).cloned().unwrap_or_default();
            let az_ring = taps.get(&(dev_id.to_string(), "accel_z".into())).cloned().unwrap_or_default();
            let n = ax_ring.len().min(ay_ring.len()).min(az_ring.len());
            for i in 0..n {
                let (t, ax) = ax_ring[i];
                let (_, ay) = ay_ring[i];
                let (_, az) = az_ring[i];
                let v = [ax - a_off[0], ay - a_off[1], az - a_off[2]];
                let v = apply_m(v);
                samples_ch[3].push((t, v[0] * a_sign[0]));
                samples_ch[4].push((t, v[1] * a_sign[1]));
            }
        } else {
            for ch in 0..5 {
                if let Some(ring) = taps.get(&(dev_id.to_string(), pin_for_ch[ch].to_string())) {
                    let off = off_for_ch[ch];
                    let sgn = sgn_for_ch[ch];
                    samples_ch[ch] = ring.iter()
                        .map(|&(t, v)| (t, (v - off) * sgn))
                        .collect();
                }
            }
        }
    }

    // "View filtered": replay the engine's noise-floor deadzone over the
    // rings so the scope shows what actually leaves the device stage.
    // Gyro = the same magnitude gate + exponential border ease; accel = the
    // same dynamic baseline, simulated chronologically over the visible
    // window (2D — az isn't a scope channel — so it's a close approximation
    // of the engine's 3D distance).
    {
        let node = canvas.snarl.get_node(_node_id);
        let dz_on = node.and_then(|n| n.params.get("gyro_dz_enabled").and_then(|v| v.as_bool())).unwrap_or(false);
        let view_after = node.and_then(|n| n.params.get("dz_view_after").and_then(|v| v.as_bool())).unwrap_or(false);
        if dz_on && view_after {
            let dz_w    = node.and_then(|n| n.params.get("gyro_dz_width").and_then(|v| v.as_f64())).unwrap_or(2.0) as f32;
            let g_dz = node.and_then(|n| n.params.get("gyro_noise_floor").and_then(|v| v.as_f64())).unwrap_or(0.0) as f32 * dz_w;
            let a_dz = node.and_then(|n| n.params.get("accel_noise_floor").and_then(|v| v.as_f64())).unwrap_or(0.0) as f32 * dz_w;
            if g_dz > 0.0 {
                let n = samples_ch[0].len().min(samples_ch[1].len()).min(samples_ch[2].len());
                for i in 0..n {
                    let (gx, gy, gz) = (samples_ch[0][i].1, samples_ch[1][i].1, samples_ch[2][i].1);
                    let s = flexinput_engine::eval::gyro_dz_ease(
                        (gx * gx + gy * gy + gz * gz).sqrt(), g_dz);
                    samples_ch[0][i].1 = gx * s;
                    samples_ch[1][i].1 = gy * s;
                    samples_ch[2][i].1 = gz * s;
                }
            }
            if a_dz > 0.0 {
                let n = samples_ch[3].len().min(samples_ch[4].len());
                let mut base: Option<[f32; 2]> = None;
                for i in 0..n {
                    let v = [samples_ch[3][i].1, samples_ch[4][i].1];
                    let b = base.get_or_insert(v);
                    let d = ((v[0] - b[0]).powi(2) + (v[1] - b[1]).powi(2)).sqrt();
                    let s = flexinput_engine::eval::gyro_dz_ease(d, a_dz);
                    if s > 0.0 {
                        b[0] += (v[0] - b[0]) * s;
                        b[1] += (v[1] - b[1]) * s;
                    }
                    samples_ch[3][i].1 = b[0];
                    samples_ch[4][i].1 = b[1];
                }
            }
        }
    }
    // Repaint as fast as possible so new tap samples flow in promptly.
    ui.ctx().request_repaint();

    // Vertical waterfall: newest sample at top (age=0), time flows down.
    // y_position is computed from sample age in ms vs. scope_win_ms so the
    // visual horizon is real-time regardless of UI repaint rate.
    let center_x = scope_rect.center().x;
    let half_w = (scope_rect.width() * 0.5) - 8.0;

    // Gray crosshair guidelines: vertical zero line + two side rails at the
    // dynamic peak (1.0 magnitude) and a horizontal midline at half-window.
    let guide = Color32::from_gray(55);
    painter.line_segment(
        [egui::pos2(center_x, scope_rect.top() + 4.0), egui::pos2(center_x, scope_rect.bottom() - 4.0)],
        Stroke::new(1.0, guide),
    );
    painter.line_segment(
        [egui::pos2(center_x - half_w * 0.5, scope_rect.top() + 4.0),
         egui::pos2(center_x - half_w * 0.5, scope_rect.bottom() - 4.0)],
        Stroke::new(1.0, Color32::from_gray(38)),
    );
    painter.line_segment(
        [egui::pos2(center_x + half_w * 0.5, scope_rect.top() + 4.0),
         egui::pos2(center_x + half_w * 0.5, scope_rect.bottom() - 4.0)],
        Stroke::new(1.0, Color32::from_gray(38)),
    );
    let mid_y = scope_rect.top() + scope_rect.height() * 0.5;
    painter.line_segment(
        [egui::pos2(scope_rect.left() + 8.0, mid_y), egui::pos2(scope_rect.right() - 8.0, mid_y)],
        Stroke::new(1.0, Color32::from_gray(38)),
    );
    // Active channel set. When the user skips accelerometer calibration the
    // accel rows are hidden entirely so the remaining gyro channels can
    // dominate the peak-scaling and zoom in.
    let active: &[usize] = if skip_accel { &[0, 1, 2] } else { &[0, 1, 2, 3, 4] };

    let peak = active.iter()
        .flat_map(|&ch| samples_ch[ch].iter().map(|(_, v)| v.abs()))
        .fold(0.0_f32, f32::max);
    // Auto-zoom with 50% padding margin so the visible peak never touches
    // the rails. peak=0.001 → scale fills ±0.0015; floor at ±0.005 so an
    // idle stick still shows some background noise rather than collapsing.
    let display_peak = (peak * 1.5).max(0.005);
    let scale = half_w / display_peak;

    let colors = [
        Color32::from_rgb(230, 70, 70),    // Roll  (gx)
        Color32::from_rgb(110, 220, 110),  // Pitch (gy)
        Color32::from_rgb(120, 170, 255),  // Yaw   (gz)
        Color32::from_rgb(235, 220, 70),   // Accel X (ax)
        Color32::from_rgb(120, 220, 220),  // Accel Y (ay)
    ];
    let labels = ["Roll", "Pitch", "Yaw", "Accel X", "Accel Y"];

    let now = Instant::now();
    let age_to_y = |age_ms: f32| -> f32 {
        let t = (age_ms / scope_win_ms).clamp(0.0, 1.0);
        scope_rect.top() + t * scope_rect.height()
    };
    // Helper: scale a color toward black by `mul` (0..1), preserving alpha.
    // Floor alpha at 25% so the oldest trace (bottom of the waterfall) is
    // still clearly visible rather than dissolving into the panel.
    let scale_color = |c: Color32, mul: f32| -> Color32 {
        let m = mul.clamp(0.0, 1.0);
        Color32::from_rgba_unmultiplied(
            (c.r() as f32 * m.max(0.40)) as u8,
            (c.g() as f32 * m.max(0.40)) as u8,
            (c.b() as f32 * m.max(0.40)) as u8,
            ((c.a() as f32) * m.max(0.25)) as u8,
        )
    };
    let mut total_pts = 0usize;
    for &ch in active {
        let ring = &samples_ch[ch];
        if ring.is_empty() { continue; }
        // Build the polyline newest→oldest with per-segment age so we can
        // fade and lay a soft glow under each segment.
        let mut pts: Vec<(Pos2, f32)> = Vec::with_capacity(ring.len());
        for (t, v) in ring.iter().rev() {
            let age_ms = now.duration_since(*t).as_secs_f32() * 1000.0;
            if age_ms > scope_win_ms { break; }
            let y = age_to_y(age_ms);
            let x = center_x + (v * scale).clamp(-half_w, half_w);
            pts.push((egui::pos2(x, y), age_ms));
        }
        total_pts += pts.len();
        // Glow underlay (wider, dim) + crisp top stroke, both fading with age.
        // Sub-linear fade so the trail stays readable across the full window.
        for i in 1..pts.len() {
            let (pp, ap) = pts[i - 1];
            let (p,  a ) = pts[i];
            let age = 0.5 * (ap + a);
            let fade = 1.0 - (age / scope_win_ms).clamp(0.0, 1.0);
            // sqrt → faster early decay, longer mid/late visibility.
            let alpha = fade.sqrt();
            let glow = scale_color(colors[ch], alpha * 0.55);
            let core = scale_color(colors[ch], alpha);
            painter.line_segment([pp, p], Stroke::new(3.5, glow));
            painter.line_segment([pp, p], Stroke::new(1.4, core));
        }
        // Edge dot at the newest sample (top of waterfall).
        if let Some(&(head, _)) = pts.first() {
            let halo = Color32::from_rgba_unmultiplied(
                colors[ch].r(), colors[ch].g(), colors[ch].b(), 110);
            painter.circle_filled(head, 5.0, halo);
            painter.circle_filled(head, 2.6, colors[ch]);
        }
    }

    // ── Noise-floor deadzone brackets ────────────────────────────────────
    //
    // Vertical borders at ±(floor × width) around the zero line, in the same
    // auto-zoom scale as the traces so they track the zoom. Orange while
    // sampling — converging into place as the floor estimate settles — then
    // green once the deadzone is armed. (Gyro only: the accel band follows
    // gravity dynamically, so it has no fixed place on the scope.)
    {
        let node = canvas.snarl.get_node(_node_id);
        let dz_on = node.and_then(|n| n.params.get("gyro_dz_enabled").and_then(|v| v.as_bool())).unwrap_or(false);
        let dz_w  = node.and_then(|n| n.params.get("gyro_dz_width").and_then(|v| v.as_f64())).unwrap_or(2.0) as f32;
        let g_floor = node.and_then(|n| n.params.get("gyro_noise_floor").and_then(|v| v.as_f64())).unwrap_or(0.0) as f32;
        let sampling = ui.ctx().data(|d| {
            d.get_temp::<WindowState>(state_id(_node_id))
                .map(|s| s.gyro.sampling)
                .unwrap_or(false)
        });
        if g_floor > 0.0 && (dz_on || sampling) {
            let dx = (g_floor * dz_w * scale).min(half_w);
            let col = if sampling { ORANGE } else { Color32::from_rgb(90, 190, 120) }
                .gamma_multiply(0.85);
            let top = scope_rect.top() + 4.0;
            let bot = scope_rect.bottom() - 4.0;
            for side in [-1.0_f32, 1.0] {
                let x = center_x + side * dx;
                painter.line_segment([egui::pos2(x, top), egui::pos2(x, bot)],
                    Stroke::new(1.3, col));
                // Bracket feet point inward, toward the zero line.
                let foot = -side * 5.0;
                painter.line_segment([egui::pos2(x, top), egui::pos2(x + foot, top)],
                    Stroke::new(1.3, col));
                painter.line_segment([egui::pos2(x, bot), egui::pos2(x + foot, bot)],
                    Stroke::new(1.3, col));
            }
        }
    }

    // ── Bottom legend strip ──────────────────────────────────────────────
    //
    // Channel chips inline on the left, peak readout in the middle, and a
    // clickable time-window indicator on the right that cycles 500 → 1000
    // → 5000 ms.
    let legend_font  = egui::FontId::proportional(13.0);
    let legend_y     = legend_rect.center().y;

    // Channel chips: small filled circle + label, packed left-to-right.
    let mut chip_x = legend_rect.left() + 8.0;
    for &i in active {
        let dot_c = egui::pos2(chip_x + 5.0, legend_y);
        painter.circle_filled(dot_c, 4.0, colors[i]);
        let text_pos = egui::pos2(dot_c.x + 8.0, legend_y);
        let galley = painter.layout_no_wrap(
            labels[i].to_string(),
            legend_font.clone(),
            colors[i],
        );
        painter.galley(
            egui::pos2(text_pos.x, text_pos.y - galley.size().y * 0.5),
            galley.clone(),
            colors[i],
        );
        chip_x = text_pos.x + galley.size().x + 12.0;
    }

    // Peak / point-count readout. Anchored just above the scope's bottom-
    // right corner so it never collides with the channel chips on the
    // legend strip — important on the smallest window size where the
    // chips already span most of the legend width.
    painter.text(
        egui::pos2(scope_rect.right() - 10.0, scope_rect.bottom() - 6.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("peak ±{:.3}  ·  {} pts", peak, total_pts),
        legend_font.clone(),
        Color32::from_gray(170),
    );

    // Clickable time-window indicator on the right.
    let win_text   = format!("{} ms", scope_win_ms as i32);
    let win_galley = painter.layout_no_wrap(
        win_text,
        legend_font.clone(),
        Color32::from_gray(220),
    );
    let win_size = win_galley.size();
    let pad_x = 8.0;
    let pad_y = 3.0;
    let win_rect = egui::Rect::from_min_size(
        egui::pos2(
            legend_rect.right() - 8.0 - win_size.x - 2.0 * pad_x,
            legend_y - win_size.y * 0.5 - pad_y,
        ),
        egui::vec2(win_size.x + 2.0 * pad_x, win_size.y + 2.0 * pad_y),
    );
    let win_resp = ui.interact(
        win_rect,
        ui.id().with(("scope_win_btn", _node_id)),
        Sense::click(),
    );
    let bg = if win_resp.hovered() {
        Color32::from_gray(45)
    } else {
        Color32::from_gray(28)
    };
    let painter = ui.painter();
    painter.rect_filled(win_rect, 3.0, bg);
    painter.rect_stroke(
        win_rect, 3.0,
        Stroke::new(1.0, Color32::from_gray(70)),
        egui::epaint::StrokeKind::Inside,
    );
    let text_color = if win_resp.hovered() { Color32::WHITE } else { Color32::from_gray(220) };
    painter.galley(
        egui::pos2(win_rect.center().x - win_size.x * 0.5,
                   win_rect.center().y - win_size.y * 0.5),
        win_galley,
        text_color,
    );
    if win_resp.clicked() {
        let next = match scope_win_ms as i32 {
            x if x <= 500 => 1000.0,
            x if x <= 1000 => 5000.0,
            _ => 500.0,
        };
        ui.ctx().data_mut(|d| {
            let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(_node_id), WindowState::default);
            st.scope_win_ms = next;
        });
    }
}

// ── Sticks section ───────────────────────────────────────────────────────────

fn sticks_section(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    node_id: NodeId,
    dev_id: &str,
    live: &LiveSignals,
    lo: Layout,
    caps: CalCaps,
) {
    let done = canvas.snarl.get_node(node_id)
        .and_then(|n| n.params.get("sticks_calibrated").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    section_header(ui, "Stick Calibration", done);

    let (raw_lx, raw_ly) = read_vec2(live, dev_id, "left_stick_x", "left_stick_y");
    let (raw_rx, raw_ry) = read_vec2(live, dev_id, "right_stick_x", "right_stick_y");
    // Display = calibrated (offset + per-quadrant scale).
    let lc = read_param_arr::<2>(canvas, node_id, "lstick_center", [0.0; 2]);
    let rc = read_param_arr::<2>(canvas, node_id, "rstick_center", [0.0; 2]);
    let ls = read_param_arr::<RADIAL_N>(canvas, node_id, "lstick_radial", [1.0; RADIAL_N]);
    let rs = read_param_arr::<RADIAL_N>(canvas, node_id, "rstick_radial", [1.0; RADIAL_N]);
    let (lx, ly) = apply_stick_view(raw_lx, raw_ly, lc, ls);
    let (rx, ry) = apply_stick_view(raw_rx, raw_ry, rc, rs);
    // Deadzone applied to the displayed dot only (same math the engine uses
    // on the live signal). Lets the user see exactly where their selected
    // deadzone collapses the dot to center.
    let deadzone = canvas.snarl.get_node(node_id)
        .and_then(|n| n.params.get("deadzone").and_then(|v| v.as_f64()))
        .map(|v| v as f32).unwrap_or(0.1).clamp(0.0, 0.5);
    let apply_dz = |x: f32, y: f32, dz: f32| -> (f32, f32) {
        if dz <= 0.0 { return (x, y); }
        let len = (x * x + y * y).sqrt();
        if len < dz { (0.0, 0.0) }
        else {
            let s = (len - dz) / (1.0 - dz).max(f32::EPSILON) / len;
            (x * s, y * s)
        }
    };
    let (lx, ly) = apply_dz(lx, ly, deadzone);
    let (rx, ry) = apply_dz(rx, ry, deadzone);

    // Step session.
    let (sampling, l_filled, r_filled, l_cn, r_cn, commit) = ui.ctx().data_mut(|d| {
        let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(node_id), WindowState::default);
        // Push to fading trails (in calibrated coordinates).
        push_trail(&mut st.l_trail, [lx, ly]);
        push_trail(&mut st.r_trail, [rx, ry]);
        if st.stick.sampling {
            // Edge tracking uses RAW values so we can compute corrective
            // scales relative to the raw signal space.
            push_edge_sample(&mut st.stick.l_edge, raw_lx, raw_ly);
            push_edge_sample(&mut st.stick.r_edge, raw_rx, raw_ry);
            if (raw_lx * raw_lx + raw_ly * raw_ly).sqrt() < STICK_CENTER_THRESHOLD {
                st.stick.l_center_sum[0] += raw_lx as f64;
                st.stick.l_center_sum[1] += raw_ly as f64;
                st.stick.l_center_n += 1;
            }
            if (raw_rx * raw_rx + raw_ry * raw_ry).sqrt() < STICK_CENTER_THRESHOLD {
                st.stick.r_center_sum[0] += raw_rx as f64;
                st.stick.r_center_sum[1] += raw_ry as f64;
                st.stick.r_center_n += 1;
            }
        }
        let l_filled = filled_buckets(&st.stick.l_edge);
        let r_filled = filled_buckets(&st.stick.r_edge);
        // Absent sticks must not hold the commit hostage. On a one-stick pad
        // the missing side never fills a bucket and never accumulates a rest
        // sample, so requiring both meant calibration could NEVER complete —
        // the button just spun forever with no indication why.
        let l_ok = !caps.left_stick
            || (l_filled >= STICK_MIN_BUCKETS && st.stick.l_center_n >= STICK_CENTER_TARGET_N);
        let r_ok = !caps.right_stick
            || (r_filled >= STICK_MIN_BUCKETS && st.stick.r_center_n >= STICK_CENTER_TARGET_N);
        let commit = st.stick.sampling && l_ok && r_ok;
        (st.stick.sampling, l_filled, r_filled, st.stick.l_center_n, st.stick.r_center_n, commit)
    });

    if commit {
        ui.ctx().data_mut(|d| {
            let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(node_id), WindowState::default);
            st.stick.sampling = false;
            let lc_new = [
                (st.stick.l_center_sum[0] / st.stick.l_center_n as f64) as f32,
                (st.stick.l_center_sum[1] / st.stick.l_center_n as f64) as f32,
            ];
            let rc_new = [
                (st.stick.r_center_sum[0] / st.stick.r_center_n as f64) as f32,
                (st.stick.r_center_sum[1] / st.stick.r_center_n as f64) as f32,
            ];
            let ls_new = derive_radial_profile(&st.stick.l_edge, lc_new);
            let rs_new = derive_radial_profile(&st.stick.r_edge, rc_new);
            if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                n.params.insert("lstick_center".into(), serde_json::json!(lc_new));
                n.params.insert("rstick_center".into(), serde_json::json!(rc_new));
                n.params.insert("lstick_radial".into(), serde_json::json!(ls_new.to_vec()));
                n.params.insert("rstick_radial".into(), serde_json::json!(rs_new.to_vec()));
                // Clear obsolete legacy keys so they don't shadow new profile.
                n.params.remove("lstick_scale");
                n.params.remove("rstick_scale");
                n.params.insert("sticks_calibrated".into(), Value::Bool(true));
            }
        });
    }

    if sampling { ui.ctx().request_repaint(); }

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(lo.left_w);
            instruct_step(ui, 1, &[(false, "Click "), (true, "Start"), (false, ".")]);
            instruct_step(ui, 2, &[
                (true,  "Rotate"),
                (false, " both sticks "),
                (true,  "slowly"),
                (false, ", tracing the edge fully several times."),
            ]);
            instruct_step(ui, 3, &[
                (false, "Let both sticks fully "),
                (true,  "return to rest"),
                (false, " for ~½ second."),
            ]);
            ui.add_space(6.0);
            instruct_line(ui, &[(false,
                "Calibration commits once the edge is mostly traced and a stable rest \
                 position is observed. The inner ring magnifies the centermost 10% of \
                 deflection so drift is easy to spot.")]);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if cal_button(ui, if sampling { "Stop" } else { "Start" }).clicked() {
                    ui.ctx().data_mut(|d| {
                        let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(node_id), WindowState::default);
                        if sampling {
                            st.stick.sampling = false;
                        } else {
                            st.stick = StickSession { sampling: true, ..Default::default() };
                        }
                    });
                }
                if cal_button(ui, "Reset").clicked() {
                    if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                        n.params.insert("lstick_center".into(), serde_json::json!([0.0_f32, 0.0]));
                        n.params.insert("rstick_center".into(), serde_json::json!([0.0_f32, 0.0]));
                        let ones: Vec<f32> = vec![1.0; RADIAL_N];
                        n.params.insert("lstick_radial".into(), serde_json::json!(ones));
                        n.params.insert("rstick_radial".into(), serde_json::json!(ones));
                        n.params.remove("lstick_scale");
                        n.params.remove("rstick_scale");
                        n.params.insert("sticks_calibrated".into(), Value::Bool(false));
                    }
                }
            });
            if sampling {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(format!(
                    "Edge L {}/{} R {}/{}\nRest L {}/{}  R {}/{}",
                    l_filled, STICK_MIN_BUCKETS, r_filled, STICK_MIN_BUCKETS,
                    l_cn, STICK_CENTER_TARGET_N, r_cn, STICK_CENTER_TARGET_N,
                )).size(INSTRUCT_SIZE).color(ORANGE));
            }
        });
        ui.vertical(|ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = lo.scope_gap;
                let l_edge_snap = ui.ctx().data(|d| d.get_temp::<WindowState>(state_id(node_id))
                    .map(|s| s.stick.l_edge).unwrap_or([0.0; STICK_BUCKETS]));
                let r_edge_snap = ui.ctx().data(|d| d.get_temp::<WindowState>(state_id(node_id))
                    .map(|s| s.stick.r_edge).unwrap_or([0.0; STICK_BUCKETS]));
                let l_trail = ui.ctx().data(|d| d.get_temp::<WindowState>(state_id(node_id))
                    .map(|s| s.l_trail.clone()).unwrap_or_default());
                let r_trail = ui.ctx().data(|d| d.get_temp::<WindowState>(state_id(node_id))
                    .map(|s| s.r_trail.clone()).unwrap_or_default());
                // Only draw scopes for sticks the device actually has. A
                // Joy-Con half has one, and the missing scope sat permanently
                // centred — indistinguishable from a dead stick, and the
                // "rotate the stick fully" step could never complete for it.
                if caps.left_stick {
                    stick_scope(ui, lx, ly, &l_trail, &l_edge_snap, lc, ls, lo);
                }
                if caps.right_stick {
                    stick_scope(ui, rx, ry, &r_trail, &r_edge_snap, rc, rs, lo);
                }
            });
            // Deadzone slider — directly linked to the device.source's
            // `deadzone` param (same one the header slider edits). Lets
            // the user tune deadzone down until the displayed dot stops
            // drifting at rest.
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Deadzone")
                    .size(INSTRUCT_SIZE).color(ORANGE));
                let mut dz = deadzone;
                let resp = ui.add(egui::Slider::new(&mut dz, 0.0_f32..=0.5)
                    .show_value(false)
                    .clamping(egui::SliderClamping::Always));
                ui.add(egui::DragValue::new(&mut dz)
                    .speed(0.005)
                    .range(0.0_f32..=0.5)
                    .fixed_decimals(3));
                if (dz - deadzone).abs() > f32::EPSILON || resp.changed() {
                    if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                        n.params.insert("deadzone".into(),
                            Value::from(dz.clamp(0.0, 0.5) as f64));
                    }
                }
            });
        });
    });
}

/// Number of buckets in the per-stick radial correction profile, mirrored
/// from the engine constant so the UI writes the right-sized array.
/// Same as STICK_BUCKETS so the edge trace maps 1:1 to the engine profile.
const RADIAL_N: usize = STICK_BUCKETS;

fn apply_stick_view(x: f32, y: f32, center: [f32; 2], radial: [f32; RADIAL_N]) -> (f32, f32) {
    let cx = x - center[0];
    let cy = y - center[1];
    let r = (cx * cx + cy * cy).sqrt();
    if r < 1e-5 { return (cx, cy); }
    let theta = cy.atan2(cx);
    let norm = (theta / std::f32::consts::TAU + 1.0) % 1.0;
    let fpos = norm * RADIAL_N as f32;
    let i0 = fpos.floor() as usize % RADIAL_N;
    let i1 = (i0 + 1) % RADIAL_N;
    let t  = fpos - fpos.floor();
    let s  = radial[i0] * (1.0 - t) + radial[i1] * t;
    (cx * s, cy * s)
}

fn push_trail(trail: &mut VecDeque<[f32; 2]>, pt: [f32; 2]) {
    if trail.len() >= STICK_TRAIL_LEN { trail.pop_front(); }
    trail.push_back(pt);
}

fn angle_bucket(x: f32, y: f32) -> Option<usize> {
    let r = (x * x + y * y).sqrt();
    if r < 0.5 { return None; }
    let theta = y.atan2(x);
    let norm = (theta / std::f32::consts::TAU + 1.0) % 1.0;
    Some(((norm * STICK_BUCKETS as f32) as usize).min(STICK_BUCKETS - 1))
}

fn push_edge_sample(edges: &mut [f32; STICK_BUCKETS], x: f32, y: f32) {
    if let Some(b) = angle_bucket(x, y) {
        let r = (x * x + y * y).sqrt();
        if r > edges[b] { edges[b] = r; }
    }
}

fn filled_buckets(edges: &[f32; STICK_BUCKETS]) -> usize {
    edges.iter().filter(|&&r| r > 0.6).count()
}

/// Reduce the 72-bucket edge-radius profile (raw stick coords) to a
/// 16-bucket centered-radius profile, then invert each entry so multiplying
/// the centered (x, y) by it brings every angle's edge to ~1.0.
///
/// Each output bucket averages the centered radii of the input buckets that
/// fall within its angular span; empty buckets fall back to a smoothed copy
/// of their neighbours so the profile stays continuous.
fn derive_radial_profile(edges: &[f32; STICK_BUCKETS], center: [f32; 2]) -> [f32; RADIAL_N] {
    // Step 1: rebin each observed (angle, radius) into the centered-angle
    // bucket the engine will look up at runtime. Same bucket count as the
    // input so there's no resolution loss.
    let mut radii: [f32; RADIAL_N] = [0.0; RADIAL_N];
    let mut have:  [bool;  RADIAL_N] = [false; RADIAL_N];
    for (i, &r) in edges.iter().enumerate() {
        if r <= 0.0 { continue; }
        let theta_raw = (i as f32 + 0.5) / STICK_BUCKETS as f32 * std::f32::consts::TAU;
        let cx = r * theta_raw.cos() - center[0];
        let cy = r * theta_raw.sin() - center[1];
        let rc = (cx * cx + cy * cy).sqrt();
        if rc <= 0.0 { continue; }
        let theta_c = cy.atan2(cx);
        let norm = (theta_c / std::f32::consts::TAU + 1.0) % 1.0;
        let bin = ((norm * RADIAL_N as f32) as usize).min(RADIAL_N - 1);
        // Keep the LARGEST radius observed in this bucket so we project the
        // outermost edge to the unit circle. Bulges drive the profile down
        // (scale < 1) so the dot can never overshoot, and dips drive it up
        // so they reach the unit circle.
        if rc > radii[bin] { radii[bin] = rc; have[bin] = true; }
    }
    // Step 2: fill empty buckets via nearest-known interpolation around the ring.
    let mut filled: [f32; RADIAL_N] = radii;
    if have.iter().any(|h| *h) {
        for i in 0..RADIAL_N {
            if have[i] { continue; }
            let mut left_d  = 0_usize;
            let mut right_d = 0_usize;
            while !have[(i + RADIAL_N - left_d)  % RADIAL_N] && left_d  < RADIAL_N { left_d  += 1; }
            while !have[(i + right_d) % RADIAL_N] && right_d < RADIAL_N { right_d += 1; }
            let l = radii[(i + RADIAL_N - left_d) % RADIAL_N];
            let r = radii[(i + right_d) % RADIAL_N];
            let total = (left_d + right_d).max(1) as f32;
            filled[i] = l * (right_d as f32 / total) + r * (left_d as f32 / total);
        }
    }
    // Step 3: smooth lightly with a 3-tap circular kernel so single-bucket
    // spikes don't make the corrected output discontinuous, but the overall
    // shape is preserved.
    let mut smooth: [f32; RADIAL_N] = [0.0; RADIAL_N];
    for i in 0..RADIAL_N {
        let a = filled[(i + RADIAL_N - 1) % RADIAL_N];
        let b = filled[i];
        let c = filled[(i + 1) % RADIAL_N];
        smooth[i] = 0.25 * a + 0.5 * b + 0.25 * c;
    }
    // Step 4: invert and clamp.
    let mut scale = [1.0_f32; RADIAL_N];
    for i in 0..RADIAL_N {
        if smooth[i] > 0.1 { scale[i] = (1.0 / smooth[i]).clamp(0.5, 2.5); }
    }
    scale
}

// ── Orientation-matrix solver ─────────────────────────────────────────────────
//
// Workflow:
//   1. User performs three single-axis rotations (Pitch, Yaw, Roll) and we
//      collect the raw gyro 3-vectors for each.
//   2. `principal_axis()` runs PCA via power-iteration on each sample cloud
//      to recover the dominant chip-frame axis. Sign is picked so the axis
//      points the same way as the mean motion vector (otherwise PCA returns
//      either +v or -v arbitrarily).
//   3. The three recovered axes form the columns of M⁻¹ (chip-frame
//      directions that correspond to body Pitch/Yaw/Roll). We invert to get
//      M (chip → body).
//   4. M is not perfectly orthonormal because the user's rotations weren't
//      pure single-axis motions. `orthonormalize_mat3()` runs modified
//      Gram-Schmidt to snap M to the closest rotation matrix, eliminating
//      shear/scale distortion.

/// Run 3D PCA on a sample cloud via power iteration on the covariance
/// matrix. Returns the unit eigenvector with the largest eigenvalue —
/// i.e. the direction of greatest variance, which corresponds to the
/// physical rotation axis the user actually rotated around.
fn principal_axis(samples: &[[f32; 3]]) -> Option<[f32; 3]> {
    if samples.len() < 16 { return None; }
    // Subtract mean (we're after axis-of-variance, not direction-of-mean).
    let n = samples.len() as f32;
    let mut mean = [0.0_f32; 3];
    for s in samples { for i in 0..3 { mean[i] += s[i]; } }
    for i in 0..3 { mean[i] /= n; }
    // 3×3 covariance, row-major.
    let mut c = [0.0_f32; 9];
    for s in samples {
        let dx = s[0] - mean[0];
        let dy = s[1] - mean[1];
        let dz = s[2] - mean[2];
        c[0] += dx * dx; c[1] += dx * dy; c[2] += dx * dz;
        c[3] += dy * dx; c[4] += dy * dy; c[5] += dy * dz;
        c[6] += dz * dx; c[7] += dz * dy; c[8] += dz * dz;
    }
    for v in c.iter_mut() { *v /= n; }
    // Power iteration: 32 multiplies are plenty for a 3×3 spread case.
    let mut v = [1.0_f32, 1.0, 1.0];
    for _ in 0..48 {
        let nv = [
            c[0] * v[0] + c[1] * v[1] + c[2] * v[2],
            c[3] * v[0] + c[4] * v[1] + c[5] * v[2],
            c[6] * v[0] + c[7] * v[1] + c[8] * v[2],
        ];
        let m = (nv[0] * nv[0] + nv[1] * nv[1] + nv[2] * nv[2]).sqrt();
        if m < 1e-9 { return None; }
        v = [nv[0] / m, nv[1] / m, nv[2] / m];
    }
    // Sign convention: pick the orientation matching the overall mean
    // direction so independent solves don't flip signs randomly.
    let dot = mean[0] * v[0] + mean[1] * v[1] + mean[2] * v[2];
    if dot < 0.0 { v = [-v[0], -v[1], -v[2]]; }
    Some(v)
}

/// Modified Gram-Schmidt orthonormalisation of a row-major 3×3 matrix
/// whose ROWS are the three near-orthogonal body axes recovered from
/// PCA. Returns the cleaned matrix (rows orthonormal, determinant +1).
fn orthonormalize_mat3(m: [f32; 9]) -> [f32; 9] {
    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 { a[0]*b[0]+a[1]*b[1]+a[2]*b[2] }
    fn sub(a: [f32; 3], b: [f32; 3], k: f32) -> [f32; 3] { [a[0]-k*b[0], a[1]-k*b[1], a[2]-k*b[2]] }
    fn norm(a: [f32; 3]) -> [f32; 3] {
        let n = (a[0]*a[0]+a[1]*a[1]+a[2]*a[2]).sqrt().max(1e-9);
        [a[0]/n, a[1]/n, a[2]/n]
    }
    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0]]
    }
    let r0 = norm([m[0], m[1], m[2]]);
    let r1 = sub([m[3], m[4], m[5]], r0, dot([m[3], m[4], m[5]], r0));
    let r1 = norm(r1);
    // Force r2 = r0 × r1 so the matrix has determinant +1.
    let r2 = cross(r0, r1);
    [
        r0[0], r0[1], r0[2],
        r1[0], r1[1], r1[2],
        r2[0], r2[1], r2[2],
    ]
}

/// Solve a chip-frame → body-frame rotation matrix from three sample
/// clouds + a mean accel (gravity) reference. Returns None if any cloud
/// is too small or degenerate.
///
/// Two-stage solve:
///   1. PCA on the three gyro clouds recovers the chip-frame direction
///      of each body axis. This is the same as the gyro-only solver, but
///      we get a rough answer that may be off by a few degrees due to
///      hand wobble.
///   2. If we have a usable gravity reference, refine by computing the
///      pre-rotation `R_g` that maps the gyro-derived "down" vector to
///      the canonical body-down (`-Y`). Apply `R_g` to every PCA axis.
///      Gravity gives an absolute reference frame the gyro motions
///      can't, so this reduces tilt error substantially.
///
/// Sample-index convention matches the pin order used elsewhere
/// (Roll=gyro_x, Pitch=gyro_y, Yaw=gyro_z):
///   samples[0] = Roll  cloud → body +X
///   samples[1] = Pitch cloud → body +Y
///   samples[2] = Yaw   cloud → body +Z
fn solve_orient_matrix(
    samples: &[Vec<[f32; 3]>; 3],
    accel_mean: Option<[f32; 3]>,
) -> Option<[f32; 9]> {
    let mut a_roll  = principal_axis(&samples[0])?;
    let mut a_pitch = principal_axis(&samples[1])?;
    let mut a_yaw   = principal_axis(&samples[2])?;

    // Stage 2: gravity refinement.
    //
    // Body convention (matches crates/devices/src/gyro.rs and
    // engine 3DOF math): gravity is +Z when the controller is held
    // face-up. The chip-frame mean accel IS the body-frame +Z axis
    // expressed in chip coordinates — exactly what we want for the
    // Yaw row of M (Yaw is rotation about +Z).
    //
    // Strategy:
    //   - Override the noisy PCA Yaw axis with the chip-frame gravity
    //     direction (absolute, hand-wobble-immune).
    //   - Sign-align PCA Roll and Pitch against the gyro-mean direction
    //     so they at least face a consistent way.
    //   - Project PCA Roll perpendicular to the new Yaw via Gram-Schmidt
    //     so the basis is orthogonal even if hand motion wasn't.
    //   - Pitch = Yaw × Roll, giving a right-handed orthonormal basis.
    if let Some(g) = accel_mean {
        let g_mag = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
        if g_mag > 0.1 {
            let g_chip = [g[0] / g_mag, g[1] / g_mag, g[2] / g_mag];

            // Yaw row of M maps chip-frame g_chip → body +Z. For an
            // orthonormal M, the Yaw ROW equals g_chip itself (since
            // M · g_chip = [0, 0, 1] ⇒ row2 · g_chip = 1 ⇒ row2 = g_chip).
            a_yaw = g_chip;

            // Sign-align PCA Roll/Pitch using the mean of their sample
            // clouds — this is more reliable than relying on PCA's
            // arbitrary sign for back-and-forth motion.
            let mean_of = |s: &[[f32; 3]]| -> [f32; 3] {
                if s.is_empty() { return [0.0; 3]; }
                let n = s.len() as f32;
                let mut m = [0.0_f32; 3];
                for v in s { m[0] += v[0]; m[1] += v[1]; m[2] += v[2]; }
                [m[0]/n, m[1]/n, m[2]/n]
            };
            // Only Roll needs sign-aligning: Pitch is rebuilt below as
            // Yaw × Roll, so its sign is fully determined by the other two.
            let m_roll  = mean_of(&samples[0]);
            let dot3 = |a: [f32;3], b: [f32;3]| a[0]*b[0]+a[1]*b[1]+a[2]*b[2];
            if dot3(a_roll,  m_roll)  < 0.0 { a_roll  = [-a_roll[0],  -a_roll[1],  -a_roll[2]];  }

            // Gram-Schmidt: make Roll perpendicular to Yaw.
            let d = dot3(a_roll, a_yaw);
            a_roll = [a_roll[0] - d * a_yaw[0], a_roll[1] - d * a_yaw[1], a_roll[2] - d * a_yaw[2]];
            let rn = (a_roll[0]*a_roll[0] + a_roll[1]*a_roll[1] + a_roll[2]*a_roll[2]).sqrt().max(1e-9);
            a_roll = [a_roll[0]/rn, a_roll[1]/rn, a_roll[2]/rn];

            // Pitch = Yaw × Roll, ensuring right-handed orthonormal basis.
            // (For body axes Roll=+X, Pitch=+Y, Yaw=+Z, we want
            //  Pitch = Yaw × Roll = +Z × +X = +Y. ✓)
            a_pitch = [
                a_yaw[1] * a_roll[2] - a_yaw[2] * a_roll[1],
                a_yaw[2] * a_roll[0] - a_yaw[0] * a_roll[2],
                a_yaw[0] * a_roll[1] - a_yaw[1] * a_roll[0],
            ];
        }
    }

    // Build M with chip directions as ROWS: M·chip = body.
    let m_chip_to_body = [
        a_roll[0],  a_roll[1],  a_roll[2],
        a_pitch[0], a_pitch[1], a_pitch[2],
        a_yaw[0],   a_yaw[1],   a_yaw[2],
    ];
    Some(orthonormalize_mat3(m_chip_to_body))
}

fn stick_scope(
    ui: &mut egui::Ui,
    x: f32, y: f32,
    trail: &VecDeque<[f32; 2]>,
    edges: &[f32; STICK_BUCKETS],
    center: [f32; 2],
    scale: [f32; RADIAL_N],
    lo: Layout,
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(lo.cell_w, lo.cell_h), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, Color32::from_gray(15));
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_gray(50)),
        egui::epaint::StrokeKind::Inside);

    let center_p = rect.center();
    let r_outer = (rect.width().min(rect.height()) * 0.5) - 8.0;

    // Outer unit circle.
    painter.circle_stroke(center_p, r_outer, Stroke::new(1.0, Color32::from_gray(80)));
    // Crosshair through the center (no extra grid lines beyond this).
    painter.line_segment(
        [egui::pos2(rect.left() + 8.0, center_p.y), egui::pos2(rect.right() - 8.0, center_p.y)],
        Stroke::new(1.0, Color32::from_gray(45)),
    );
    painter.line_segment(
        [egui::pos2(center_p.x, rect.top() + 8.0), egui::pos2(center_p.x, rect.bottom() - 8.0)],
        Stroke::new(1.0, Color32::from_gray(45)),
    );
    // Inner magnified region indicator (40% of outer = 10% deflection display).
    let r_inner_visible = r_outer * 0.4;
    painter.circle_stroke(center_p, r_inner_visible, Stroke::new(1.0, Color32::from_rgb(70, 110, 130)));

    // Edge-trace polyline (already-traced circularity).
    {
        let mut prev: Option<Pos2> = None;
        let mut first: Option<Pos2> = None;
        for (i, &r) in edges.iter().enumerate() {
            if r <= 0.0 { prev = None; continue; }
            let theta = (i as f32 + 0.5) / STICK_BUCKETS as f32 * std::f32::consts::TAU;
            let rx_raw = r * theta.cos();
            let ry_raw = r * theta.sin();
            let (vx, vy) = apply_stick_view(rx_raw, ry_raw, center, scale);
            let p = egui::pos2(
                center_p.x + vx.clamp(-1.05, 1.05) * r_outer,
                center_p.y - vy.clamp(-1.05, 1.05) * r_outer,
            );
            if first.is_none() { first = Some(p); }
            if let Some(pp) = prev {
                painter.line_segment([pp, p], Stroke::new(1.6, Color32::from_rgb(80, 200, 100)));
            }
            prev = Some(p);
        }
        // Close the loop if the trace is sufficiently complete.
        if let (Some(a), Some(b)) = (prev, first) {
            if filled_buckets(edges) >= (STICK_BUCKETS * 9 / 10) {
                painter.line_segment([a, b], Stroke::new(1.6, Color32::from_rgb(80, 200, 100)));
            }
        }
    }

    // Fading trail rendered as a small number of polyline chunks with
    // ramped alpha instead of one circle per sample. Matches the
    // vectorscope module's fading-line look and drops the painter shape
    // count from O(trail) to O(FADE_CHUNKS) per chunk.
    //
    // The display applies a 4× magnification inside the inner deadzone
    // ring (`r_xy < 0.10`). A point that crosses that boundary visually
    // jumps, so we split a chunk whenever the inner/outer zone flips —
    // otherwise the polyline would draw a misleading line connecting
    // the magnified inner point to the un-magnified outer point.
    const FADE_CHUNKS: usize = 10;
    let n = trail.len();
    if n >= 2 {
        // Pre-project and tag each point with its zone (inner/outer).
        let pts: Vec<(egui::Pos2, bool)> = trail.iter().map(|p| {
            let r_xy = (p[0] * p[0] + p[1] * p[1]).sqrt();
            let inner = r_xy < 0.10;
            let (dx, dy) = if inner { (p[0] * 4.0, -p[1] * 4.0) } else { (p[0], -p[1]) };
            (
                egui::pos2(
                    center_p.x + dx.clamp(-1.0, 1.0) * r_outer,
                    center_p.y + dy.clamp(-1.0, 1.0) * r_outer,
                ),
                inner,
            )
        }).collect();

        let per_chunk = (n / FADE_CHUNKS).max(1);
        for c in 0..FADE_CHUNKS {
            let lo_i = c * per_chunk;
            let hi_i = ((c + 1) * per_chunk + 1).min(pts.len()); // share boundary
            if hi_i <= lo_i + 1 { continue; }
            let age = c as f32 / (FADE_CHUNKS - 1).max(1) as f32;
            let alpha = (40.0 + age * 160.0) as u8;
            let stroke_color = Color32::from_rgba_unmultiplied(220, 80, 80, alpha);
            // Emit sub-polylines within the chunk, splitting whenever the
            // zone flips so we never draw a line across the inner/outer
            // magnification boundary.
            let mut run: Vec<egui::Pos2> = Vec::new();
            let mut run_zone: Option<bool> = None;
            for &(p, inner) in &pts[lo_i..hi_i] {
                match run_zone {
                    Some(z) if z == inner => run.push(p),
                    _ => {
                        if run.len() >= 2 {
                            painter.line(run.clone(), Stroke::new(1.25, stroke_color));
                        }
                        run.clear();
                        run.push(p);
                        run_zone = Some(inner);
                    }
                }
            }
            if run.len() >= 2 {
                painter.line(run, Stroke::new(1.25, stroke_color));
            }
        }
    }

    // Current dot (large in magnified region, small outside).
    let r_xy = (x * x + y * y).sqrt();
    let (dx, dy, dr) = if r_xy < 0.10 { (x * 4.0, -y * 4.0, 6.0) } else { (x, -y, 4.0) };
    let pt = egui::pos2(
        center_p.x + dx.clamp(-1.0, 1.0) * r_outer,
        center_p.y + dy.clamp(-1.0, 1.0) * r_outer,
    );
    painter.circle_filled(pt, dr, Color32::from_rgb(220, 80, 80));
}

// ── Triggers section ─────────────────────────────────────────────────────────

const TRIG_THRESHOLD_DEFAULT: f32 = 0.5;

fn triggers_section(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    node_id: NodeId,
    dev_id: &str,
    live: &LiveSignals,
    lo: Layout,
) {
    let done = canvas.snarl.get_node(node_id)
        .and_then(|n| n.params.get("triggers_calibrated").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    section_header(ui, "Analog Trigger Calibration", done);

    let raw_l = read_float(live, dev_id, "left_trigger");
    let raw_r = read_float(live, dev_id, "right_trigger");

    // Displayed bar = engine-equivalent normalisation.
    let lt_min  = read_param_f32(canvas, node_id, "ltrig_min", 0.0);
    let lt_max  = read_param_f32(canvas, node_id, "ltrig_max", 1.0);
    let rt_min  = read_param_f32(canvas, node_id, "rtrig_min", 0.0);
    let rt_max  = read_param_f32(canvas, node_id, "rtrig_max", 1.0);
    let lt_thr  = read_param_f32(canvas, node_id, "ltrig_digital_threshold", TRIG_THRESHOLD_DEFAULT);
    let rt_thr  = read_param_f32(canvas, node_id, "rtrig_digital_threshold", TRIG_THRESHOLD_DEFAULT);
    let l_norm  = normalise_trig(raw_l, lt_min, lt_max);
    let r_norm  = normalise_trig(raw_r, rt_min, rt_max);

    // Step session: track min/max + tally top/bottom 1% for jitter-aware bounds.
    let sampling = ui.ctx().data_mut(|d| {
        let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(node_id), WindowState::default);
        if st.trig.sampling {
            if !st.trig.seen {
                st.trig.l_min_obs = raw_l; st.trig.l_max_obs = raw_l;
                st.trig.r_min_obs = raw_r; st.trig.r_max_obs = raw_r;
                st.trig.seen = true;
            } else {
                if raw_l < st.trig.l_min_obs { st.trig.l_min_obs = raw_l; }
                if raw_l > st.trig.l_max_obs { st.trig.l_max_obs = raw_l; }
                if raw_r < st.trig.r_min_obs { st.trig.r_min_obs = raw_r; }
                if raw_r > st.trig.r_max_obs { st.trig.r_max_obs = raw_r; }
                // Top-1% tally: any sample within 1% of the current max contributes.
                let l_top = st.trig.l_max_obs - 0.01;
                let r_top = st.trig.r_max_obs - 0.01;
                if raw_l >= l_top { st.trig.l_max_top_sum += raw_l as f64; st.trig.l_max_top_n += 1; }
                if raw_r >= r_top { st.trig.r_max_top_sum += raw_r as f64; st.trig.r_max_top_n += 1; }
                let l_bot = st.trig.l_min_obs + 0.01;
                let r_bot = st.trig.r_min_obs + 0.01;
                if raw_l <= l_bot { st.trig.l_min_bot_sum += raw_l as f64; st.trig.l_min_bot_n += 1; }
                if raw_r <= r_bot { st.trig.r_min_bot_sum += raw_r as f64; st.trig.r_min_bot_n += 1; }
            }
        }
        st.trig.sampling
    });

    if sampling { ui.ctx().request_repaint(); }

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(lo.left_w);
            instruct_line(ui, &[
                (true,  "Depress"),
                (false, " and then "),
                (true,  "fully press"),
                (false, " on each trigger "),
                (true,  "twice"),
                (false, " to establish full working range."),
            ]);
            ui.add_space(8.0);
            instruct_line(ui, &[(false,
                "Drag the yellow pin to set the Digital output threshold for each trigger.")]);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if cal_button(ui, if sampling { "Stop & save" } else { "Start" }).clicked() {
                    if sampling {
                        let snap = ui.ctx().data_mut(|d| {
                            let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(node_id), WindowState::default);
                            st.trig.sampling = false;
                            st.trig.clone()
                        });
                        if snap.seen {
                            let (l_min_c, l_max_c) = debias_range(
                                snap.l_min_obs, snap.l_max_obs,
                                snap.l_min_bot_sum, snap.l_min_bot_n,
                                snap.l_max_top_sum, snap.l_max_top_n,
                            );
                            let (r_min_c, r_max_c) = debias_range(
                                snap.r_min_obs, snap.r_max_obs,
                                snap.r_min_bot_sum, snap.r_min_bot_n,
                                snap.r_max_top_sum, snap.r_max_top_n,
                            );
                            if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                                n.params.insert("ltrig_min".into(), serde_json::json!(l_min_c));
                                n.params.insert("ltrig_max".into(), serde_json::json!(l_max_c));
                                n.params.insert("rtrig_min".into(), serde_json::json!(r_min_c));
                                n.params.insert("rtrig_max".into(), serde_json::json!(r_max_c));
                                n.params.insert("triggers_calibrated".into(), Value::Bool(true));
                            }
                        }
                    } else {
                        ui.ctx().data_mut(|d| {
                            let st = d.get_temp_mut_or_insert_with::<WindowState>(state_id(node_id), WindowState::default);
                            st.trig = TriggerSession { sampling: true, ..Default::default() };
                        });
                    }
                }
                if cal_button(ui, "Reset").clicked() {
                    if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                        n.params.insert("ltrig_min".into(), serde_json::json!(0.0_f32));
                        n.params.insert("ltrig_max".into(), serde_json::json!(1.0_f32));
                        n.params.insert("rtrig_min".into(), serde_json::json!(0.0_f32));
                        n.params.insert("rtrig_max".into(), serde_json::json!(1.0_f32));
                        n.params.insert("triggers_calibrated".into(), Value::Bool(false));
                    }
                }
            });
            if sampling {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(format!(
                    "L [{:.3}..{:.3}]   R [{:.3}..{:.3}]",
                    ui.ctx().data(|d| d.get_temp::<WindowState>(state_id(node_id)).map(|s| s.trig.l_min_obs).unwrap_or(0.0)),
                    ui.ctx().data(|d| d.get_temp::<WindowState>(state_id(node_id)).map(|s| s.trig.l_max_obs).unwrap_or(0.0)),
                    ui.ctx().data(|d| d.get_temp::<WindowState>(state_id(node_id)).map(|s| s.trig.r_min_obs).unwrap_or(0.0)),
                    ui.ctx().data(|d| d.get_temp::<WindowState>(state_id(node_id)).map(|s| s.trig.r_max_obs).unwrap_or(0.0)),
                )).size(INSTRUCT_SIZE).color(ORANGE));
            }
        });
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            // Each trigger widget is half the width of a vectorscope and
            // sits centered on the matching vectorscope's vertical axis.
            // Layout per side: [pad-left] [widget] [pad-right] so the cells
            // remain `lo.cell_w` wide and the two widgets line up under the
            // vectorscopes above.
            let pad = ((lo.cell_w - lo.trig_w) * 0.5).max(0.0);
            ui.add_space(pad);
            let new_l = trigger_widget(ui, "L", l_norm, lt_thr, lt_min, lt_max, lo);
            ui.add_space(pad + lo.scope_gap + pad);
            let new_r = trigger_widget(ui, "R", r_norm, rt_thr, rt_min, rt_max, lo);
            if let Some(n) = canvas.snarl.get_node_mut(node_id) {
                if let Some(t) = new_l.threshold { n.params.insert("ltrig_digital_threshold".into(), serde_json::json!(t)); }
                if let Some(v) = new_l.range_min { n.params.insert("ltrig_min".into(), serde_json::json!(v)); }
                if let Some(v) = new_l.range_max { n.params.insert("ltrig_max".into(), serde_json::json!(v)); }
                if let Some(t) = new_r.threshold { n.params.insert("rtrig_digital_threshold".into(), serde_json::json!(t)); }
                if let Some(v) = new_r.range_min { n.params.insert("rtrig_min".into(), serde_json::json!(v)); }
                if let Some(v) = new_r.range_max { n.params.insert("rtrig_max".into(), serde_json::json!(v)); }
            }
        });
    });
}

fn normalise_trig(v: f32, mn: f32, mx: f32) -> f32 {
    if (mx - mn).abs() < 1e-4 { v.clamp(0.0, 1.0) }
    else { ((v - mn) / (mx - mn)).clamp(0.0, 1.0) }
}

/// Jitter-aware range debias + quantization.
///
/// - max → snap to mean of top-1% samples, then subtract a small margin
///   (TRIG_JITTER_MARGIN_STEPS × LSB) so post-press impact noise can't
///   cause the normalised output to chatter just below 1.0.
/// - min → analogously: mean of bottom-1% + small margin.
/// - both → quantize to the device's LSB grid.
fn debias_range(
    raw_min: f32, raw_max: f32,
    bot_sum: f64, bot_n: u32,
    top_sum: f64, top_n: u32,
) -> (f32, f32) {
    let mut mn = if bot_n > 0 { (bot_sum / bot_n as f64) as f32 } else { raw_min };
    let mut mx = if top_n > 0 { (top_sum / top_n as f64) as f32 } else { raw_max };
    let margin = TRIG_QUANTIZE * TRIG_JITTER_MARGIN_STEPS;
    mn += margin;
    mx -= margin;
    if mx <= mn { mx = mn + TRIG_QUANTIZE; }
    let mn_q = (mn / TRIG_QUANTIZE).round() * TRIG_QUANTIZE;
    let mx_q = (mx / TRIG_QUANTIZE).round() * TRIG_QUANTIZE;
    (mn_q.clamp(0.0, 1.0), mx_q.clamp(0.0, 1.0))
}

/// Updates returned by `trigger_widget` for the current frame. Any field
/// may be `Some` if the user dragged/clicked that pin.
#[derive(Default)]
struct TriggerWidgetEdit {
    threshold: Option<f32>,
    range_min: Option<f32>,
    range_max: Option<f32>,
}

/// Trigger arc with live fill + draggable threshold pin (inner ring) +
/// draggable range min/max pins (outer ring) + Atrig SVG silhouette.
///
/// Geometry: two concentric half-circles on the right side, cup opening
/// leftward.
///   * Inner radius `r_in`   — threshold pin (yellow).
///   * Outer radius `r_out`  — range pins (green min, red max).
/// Clicks within ±r_hit pixels of each ring are dispatched to that ring.
fn trigger_widget(
    ui: &mut egui::Ui,
    label: &str,
    norm: f32,
    threshold: f32,
    range_min: f32,
    range_max: f32,
    lo: Layout,
) -> TriggerWidgetEdit {
    let size = egui::vec2(lo.trig_w, lo.trig_h);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, Color32::from_gray(15));
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_gray(50)),
        egui::epaint::StrokeKind::Inside);

    let cx = rect.center().x - rect.width() * 0.06;
    let cy = rect.center().y;
    // Two concentric arcs: outer for range-clamp pins, inner for threshold.
    let r_out = (rect.width().min(rect.height()) * 0.5) - 10.0;
    let r_in  = r_out * 0.74;
    let start = -std::f32::consts::FRAC_PI_2;
    let end   =  std::f32::consts::FRAC_PI_2;

    // Trigger silhouette — sized to fit inside the inner arc.
    if let Some(tex) = load_atrig_texture(ui, (r_in * 2.0) as u32) {
        let img_h = r_in * 1.30;
        let img_w = img_h * (69.0 / 82.0);
        let img_rect = egui::Rect::from_center_size(
            egui::pos2(cx, cy + r_in * 0.05),
            egui::vec2(img_w, img_h),
        );
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let mut mesh = egui::epaint::Mesh::with_texture(tex.id());
        mesh.add_rect_with_uv(img_rect, uv, Color32::WHITE);
        painter.add(egui::Shape::mesh(mesh));
    }

    // L/R glyph.
    let glyph_size = (r_in * 0.78).clamp(22.0, 62.0);
    painter.text(
        egui::pos2(cx, cy + r_in * 0.05),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(glyph_size),
        Color32::from_gray(80),
    );

    // Inner background arc.
    arc_segment(painter, egui::pos2(cx, cy), r_in, start, end, 5.0, Color32::from_gray(50));
    // Inner live fill (post-calibration) — blue.
    let fill_end = start + (end - start) * norm.clamp(0.0, 1.0);
    if fill_end > start {
        arc_segment(painter, egui::pos2(cx, cy), r_in, start, fill_end, 5.0,
            Color32::from_rgb(100, 180, 240));
    }

    // Outer background arc — dim, full sweep.
    arc_segment(painter, egui::pos2(cx, cy), r_out, start, end, 3.5,
        Color32::from_gray(45));
    // Outer active range — orange band between min and max pins.
    let rmin_t = range_min.clamp(0.0, 1.0);
    let rmax_t = range_max.clamp(rmin_t, 1.0);
    let a_min = start + (end - start) * rmin_t;
    let a_max = start + (end - start) * rmax_t;
    if a_max > a_min {
        arc_segment(painter, egui::pos2(cx, cy), r_out, a_min, a_max, 3.5,
            Color32::from_rgb(220, 160, 40));
    }

    // Threshold pin on inner ring (yellow).
    let thr_t = threshold.clamp(0.0, 1.0);
    let thr_a = start + (end - start) * thr_t;
    let thr_pin = polar(egui::pos2(cx, cy), r_in, thr_a);
    painter.circle_filled(thr_pin, 5.0, Color32::from_rgb(240, 220, 60));
    painter.circle_stroke(thr_pin, 5.0, Stroke::new(1.0, Color32::from_gray(30)));

    // Range pins on outer ring.
    let min_pin = polar(egui::pos2(cx, cy), r_out, a_min);
    let max_pin = polar(egui::pos2(cx, cy), r_out, a_max);
    painter.circle_filled(min_pin, 5.0, Color32::from_rgb(80, 200, 110));
    painter.circle_stroke(min_pin, 5.0, Stroke::new(1.0, Color32::from_gray(30)));
    painter.circle_filled(max_pin, 5.0, Color32::from_rgb(230, 90, 90));
    painter.circle_stroke(max_pin, 5.0, Stroke::new(1.0, Color32::from_gray(30)));

    let mut out = TriggerWidgetEdit::default();

    // Pointer routing: figure out which ring (inner/outer) the user is
    // interacting with. We pick whichever ring's radius is CLOSER to the
    // pointer's current distance from the arc center. Clicks at very
    // different radii (silhouette area or far outside) snap to whichever
    // is nearest, which feels natural.
    if resp.dragged() || resp.clicked() {
        // Sticky-target: once a drag starts on a ring, keep editing that
        // ring even if the pointer wanders into the other ring's
        // capture zone mid-drag. Stash the target on drag start.
        let target_key = egui::Id::new(("trig_drag_target", label, resp.id));
        let target_now = if resp.drag_started() || resp.clicked() {
            let pos = resp.interact_pointer_pos().unwrap_or(egui::pos2(cx, cy));
            let d = ((pos.x - cx).powi(2) + (pos.y - cy).powi(2)).sqrt();
            // Decide based on which radius is closer.
            let to_in = (d - r_in).abs();
            let to_out = (d - r_out).abs();
            // For the outer ring, decide which pin (min vs max) is closer
            // in angle so a single click can grab the right handle.
            let target: u8 = if to_in <= to_out {
                0 // threshold
            } else {
                let dx = pos.x - cx;
                let dy = pos.y - cy;
                let a = if dx < 0.0 {
                    if dy < 0.0 { start } else { end }
                } else {
                    dy.atan2(dx).clamp(start, end)
                };
                if (a - a_min).abs() <= (a - a_max).abs() { 1 } else { 2 }
            };
            ui.ctx().data_mut(|d| d.insert_temp(target_key, target));
            target
        } else {
            ui.ctx().data(|d| d.get_temp::<u8>(target_key)).unwrap_or(0)
        };
        if let Some(pos) = resp.interact_pointer_pos() {
            let dx = pos.x - cx;
            let dy = pos.y - cy;
            let a = if dx < 0.0 {
                if dy < 0.0 { start } else { end }
            } else {
                dy.atan2(dx).clamp(start, end)
            };
            let span = end - start;
            let t = ((a - start) / span).clamp(0.0, 1.0);
            match target_now {
                0 => out.threshold = Some(t),
                1 => out.range_min = Some(t.min(rmax_t - 0.01).max(0.0)),
                2 => out.range_max = Some(t.max(rmin_t + 0.01).min(1.0)),
                _ => {}
            }
        }
    }
    out
}

fn polar(c: Pos2, r: f32, a: f32) -> Pos2 {
    egui::pos2(c.x + r * a.cos(), c.y + r * a.sin())
}

fn arc_segment(painter: &egui::Painter, center: Pos2, radius: f32, a0: f32, a1: f32, stroke_w: f32, color: Color32) {
    const N: usize = 72;
    let span = a1 - a0;
    let mut prev = polar(center, radius, a0);
    for i in 1..=N {
        let a = a0 + span * (i as f32 / N as f32);
        let p = polar(center, radius, a);
        painter.line_segment([prev, p], Stroke::new(stroke_w, color));
        prev = p;
    }
}

fn load_atrig_texture(ui: &egui::Ui, h_px: u32) -> Option<egui::TextureHandle> {
    let key = egui::Id::new(("cal_atrig_svg", h_px));
    if let Some(h) = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return Some(h);
    }
    let text = std::str::from_utf8(ATRIG_SVG).ok()?;
    let img = crate::canvas::viewer::rasterize_svg_recolored(
        text, h_px, h_px, "override", Color32::TRANSPARENT,
    )?;
    let handle = ui.ctx().load_texture(
        format!("cal_atrig_{}", h_px),
        img,
        egui::TextureOptions::LINEAR,
    );
    ui.ctx().data_mut(|d| d.insert_temp(key, handle.clone()));
    Some(handle)
}
