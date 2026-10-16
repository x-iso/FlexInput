//! Calibrate Gyro — the over-game prompt a mapping card's Calibrate Gyro
//! output (`flexinput_core::imu::GYRO_CAL_PIN`) summons.
//!
//! The engine passes each press up as the pads feeding the card
//! (`ProcessingOutput::gyro_cal_requests`); [`request`] starts a session for
//! them and [`show`] runs it every frame:
//!
//! 1. **Lay it down** — the prompt asks for the pad to be laid flat.
//! 2. **Calibrating** — once a pad has held still for [`STEADY_S`], its gyro
//!    is measured for [`CAL_S`] behind a filling ring. Any movement starts
//!    that pad over. The result becomes the pad's `gyro_offset` (and its
//!    spread the noise floor) on every `device.source` node for it, in every
//!    tab; the accelerometer is left alone.
//! 3. **Spike check** — the pad stays down until [`SPIKE_S`] of stillness
//!    are recorded, and `spike_tune` finds the least spike-filter setting that
//!    removes every spike in it (narrowest window, then lowest sensitivity),
//!    written the same way and pushed to the device at once. No spikes, no
//!    change.
//! 4. **Done** — a check and the spike result, then the prompt fades out.
//!
//! A pad's samples come from its spike filter's capture (`spike::capture_*`):
//! every sample, BEFORE the filter, so spikes the current setting removes are
//! still seen. A controller whose backend has no spike filter falls back to
//! its live pins, one sample per frame, and skips the spike check. Stillness
//! and the offset are both measured robustly, so a spiky pad still settles
//! and its spikes don't bias the offset.
//!
//! Pressing again while it waits cancels; it gives up after [`TIMEOUT_S`].
//! A Joy-Con 2's own orientation is corrected inside its decoder, which
//! learns its resting bias whenever the pad lies still — the time the pad
//! spends on the table here is enough for that too.
//!
//! Same transparent click-through viewport recipe as the menu overlay
//! (transparent + skip-taskbar triggers the vendored NOREDIRECTIONBITMAP
//! patch; passthrough latched once per window).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, FontId, Pos2, Stroke};
use egui_snarl::Snarl;
use flexinput_core::Signal;
use flexinput_devices::gyro::GYRO_REF_DPS;
use flexinput_devices::spike;
use flexinput_devices::spike_tune::{self, Tuning};
use serde_json::Value;

use crate::app::PatchTab;
use crate::canvas::NodeData;
use crate::panels::calibration::{SpikeSettings, AUTOCAL_ACCEL_P2P, AUTOCAL_GYRO_P2P};

type LiveSignals = HashMap<(String, String), Signal>;
/// Raw `[gx, gy, gz, ax, ay, az]`.
pub(crate) type Sample = [f32; 6];
/// A spike-filter tuning running on a worker thread: `Some` once it is done
/// (`None` inside when the recording was too short to judge).
pub(crate) type TuneSlot = Arc<Mutex<Option<Option<Tuning>>>>;

const TITLE: &str = "FlexInput Calibration Overlay";
const PAD_SVG: &[u8] = include_bytes!("../../../app/assets/gamepad_flat_front.svg");

/// The trailing stretch a pad must have held still over before sampling
/// starts — long enough for the knock of setting it down to pass. Still =
/// every axis's 5th–95th percentile spread inside the auto-calibration's
/// peak-to-peak spans, so a spike or two can't keep a pad from settling.
pub(crate) const STEADY_S: f64 = 0.5;
/// How long a still pad's gyro is measured. Also enough stillness for a
/// Joy-Con 2's decoder to learn its own bias (0.5 s windows, ~2 s constant).
const CAL_S: f64 = 3.0;
/// The still stretch the spike check needs, counted from the same start as
/// the gyro's (so it adds `SPIKE_S − CAL_S`).
const SPIKE_S: f64 = 7.0;
/// Fewest samples a calibration may be measured from.
const MIN_SAMPLES: usize = 10;
/// A sample further than this many robust σ from the median (and at least
/// [`OUTLIER_MIN`]) is a spike, left out of the offset.
const OUTLIER_SIGMA: f32 = 6.0;
const OUTLIER_MIN: f32 = 6.0e-5;
/// A resting rate beyond this on any axis isn't bias: a stream that froze
/// mid-motion reads perfectly still. Same plausibility limit as the Joy-Con
/// 2 decoder's own learner.
const MAX_BIAS_DPS: f32 = 10.0;
/// No new sample for this long: the pad stopped streaming.
const STALL_S: f64 = 0.3;
/// How long a pad's spike filter gets to deliver its first captured sample
/// before the pad falls back to its live pins.
const CAPTURE_GRACE_S: f64 = 0.8;
/// Gives up when the pads haven't all finished by then.
const TIMEOUT_S: f64 = 45.0;
/// A second press this soon after the first joins the session rather than
/// cancelling it — two cards on one button can fire a tick apart.
const JOIN_S: f64 = 0.3;
/// How long "moved" shows after a pad is picked up mid-calibration.
const MOVED_NOTE_S: f64 = 1.5;
const FADE_S: f64 = 0.4;
/// What the scope shows, and how often its spike marks are refreshed.
const SCOPE_S: f64 = 2.0;
const MARK_EVERY_S: f64 = 0.15;

const ORANGE: Color32 = Color32::from_rgb(220, 160, 40);
const GREEN: Color32 = Color32::from_rgb(80, 200, 100);
const CYAN: Color32 = Color32::from_rgb(90, 190, 235);
const SPIKE_RED: Color32 = Color32::from_rgb(255, 80, 80);
/// The Calibration window's scope colours: roll, pitch, yaw.
const AXIS_COLORS: [Color32; 3] = [
    Color32::from_rgb(230, 70, 70),
    Color32::from_rgb(110, 220, 110),
    Color32::from_rgb(120, 170, 255),
];

/// The quantization snap the Calibration window applies to its offsets.
const SNAP_EPS: f32 = 1.0 / 16384.0;

/// A finished gyro calibration, in normalized units.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GyroCal {
    offset: [f32; 3],
    /// RMS magnitude of the three axes' noise (the engine gates on |g|).
    floor: f32,
}

/// Where a pad's samples come from.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Source {
    /// Waiting for its spike filter's capture to deliver.
    Pending,
    /// What its spike filter is fed — every sample, spikes included.
    Capture,
    /// No filter to capture from: the live pins, one sample per frame.
    Signals,
}

#[derive(Clone, Debug)]
enum SpikeCheck {
    /// Waits for the still stretch to reach [`SPIKE_S`].
    Pending,
    /// No spike filter on this controller.
    Unavailable,
    /// Tuning on a worker thread.
    Analysing(TuneSlot),
    Done(Option<Tuning>),
}

/// One pad's calibration.
#[derive(Clone)]
struct PadCal {
    dev: String,
    source: Source,
    armed_at: Option<f64>,
    /// Capture read cursor (samples recorded so far).
    cursor: u64,
    last_sample_at: f64,
    /// Samples over the last [`SCOPE_S`]: stillness and the scope.
    recent: VecDeque<(f64, Sample)>,
    /// When the current still stretch began, and its samples.
    since: Option<f64>,
    stretch: Vec<Sample>,
    /// Last time a still stretch was broken by movement.
    moved_at: Option<f64>,
    gyro: Option<GyroCal>,
    gyro_applied: bool,
    spikes: SpikeCheck,
    spikes_applied: bool,
    /// Times of the spikes in `recent`, for the scope.
    marks: Vec<f64>,
    marks_at: f64,
}

impl PadCal {
    fn new(dev: String) -> Self {
        Self {
            dev,
            source: Source::Pending,
            armed_at: None,
            cursor: 0,
            last_sample_at: f64::NEG_INFINITY,
            recent: VecDeque::new(),
            since: None,
            stretch: Vec::new(),
            moved_at: None,
            gyro: None,
            gyro_applied: false,
            spikes: SpikeCheck::Pending,
            spikes_applied: false,
            marks: Vec::new(),
            marks_at: f64::NEG_INFINITY,
        }
    }

    /// Gyro measured and spike check settled.
    fn finished(&self) -> bool {
        self.gyro.is_some() && matches!(self.spikes, SpikeCheck::Unavailable | SpikeCheck::Done(_))
    }

    /// Still needs the pad lying still.
    fn sampling(&self) -> bool {
        self.gyro.is_none() || matches!(self.spikes, SpikeCheck::Pending)
    }

    /// Feed this frame's samples (`(time, raw)`, oldest first).
    fn step(&mut self, now: f64, batch: &[(f64, Sample)]) {
        if !batch.is_empty() {
            self.last_sample_at = now;
        }
        self.recent.extend(batch.iter().copied());
        while self.recent.front().is_some_and(|(t, _)| now - t > SCOPE_S) {
            self.recent.pop_front();
        }
        if let SpikeCheck::Analysing(slot) = &self.spikes {
            let done = slot.lock().ok().and_then(|mut r| r.take());
            if let Some(result) = done {
                self.spikes = SpikeCheck::Done(result);
            }
        }
        if !self.sampling() {
            return;
        }
        if now - self.last_sample_at > STALL_S || !self.still(now) {
            self.restart(now);
            return;
        }
        let since = *self.since.get_or_insert(now);
        self.stretch.extend(batch.iter().map(|(_, s)| *s));
        let held = now - since;
        if self.gyro.is_none() && held >= CAL_S && self.stretch.len() >= MIN_SAMPLES {
            match gyro_cal(&self.stretch) {
                Some(cal) => self.gyro = Some(cal),
                None => {
                    self.restart(now);
                    self.moved_at = None;
                    return;
                }
            }
        }
        if self.gyro.is_some() && matches!(self.spikes, SpikeCheck::Pending) {
            if self.source != Source::Capture {
                self.spikes = SpikeCheck::Unavailable;
            } else if held >= SPIKE_S {
                self.spikes = analyse(self.stretch.clone());
            }
        }
    }

    /// The last [`STEADY_S`] are covered (give or take a frame) and
    /// [`steady`].
    fn still(&self, now: f64) -> bool {
        let win: Vec<Sample> = self.recent.iter()
            .filter(|(t, _)| now - t <= STEADY_S)
            .map(|(_, s)| *s)
            .collect();
        let covered = self.recent.iter().find(|(t, _)| now - t <= STEADY_S)
            .is_some_and(|(t0, _)| now - t0 >= STEADY_S * 0.6);
        covered && steady(&win)
    }

    fn restart(&mut self, now: f64) {
        if self.since.is_some() {
            self.moved_at = Some(now);
        }
        self.since = None;
        self.stretch.clear();
    }

    /// 0..1 through this pad's stillness: the gyro's share first, then the
    /// spike check's.
    fn progress(&self, now: f64) -> f32 {
        if self.finished() || matches!(self.spikes, SpikeCheck::Analysing(_)) {
            return 1.0;
        }
        let total = if self.source == Source::Signals { CAL_S } else { SPIKE_S };
        let held = self.since.map_or(0.0, |s| now - s);
        let p = (held / total).clamp(0.0, 1.0) as f32;
        if self.gyro.is_some() { p.max(gyro_share(self.source)) } else { p }
    }

    /// Refresh the scope's spike marks while the pad is still.
    fn refresh_marks(&mut self, now: f64) {
        if self.since.is_none() || self.source != Source::Capture {
            self.marks.clear();
            return;
        }
        if now - self.marks_at < MARK_EVERY_S || self.recent.len() < 2 * spike_tune::BASE_R + 1 {
            return;
        }
        self.marks_at = now;
        let samples: Vec<Sample> = self.recent.iter().map(|(_, s)| *s).collect();
        let det = spike_tune::detect(&samples);
        // The newest few have no baseline to their right yet.
        let settled = samples.len() - spike_tune::BASE_R;
        self.marks = det.flags[..settled].iter().zip(&self.recent)
            .filter(|(f, _)| **f)
            .map(|(_, (t, _))| *t)
            .collect();
    }
}

/// A pad lying still over `win`: every axis's 5th–95th percentile spread
/// inside the auto-calibration's peak-to-peak spans — so a spike or two can't
/// fail it, and hand tremor always does. Needs four samples at the least.
pub(crate) fn steady(win: &[Sample]) -> bool {
    if win.len() < 4 {
        return false;
    }
    let mut v: Vec<f32> = Vec::with_capacity(win.len());
    (0..6).all(|a| {
        v.clear();
        v.extend(win.iter().map(|s| s[a]));
        v.sort_unstable_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        let n = v.len() - 1;
        let spread = v[n * 95 / 100] - v[n * 5 / 100];
        spread < if a < 3 { AUTOCAL_GYRO_P2P } else { AUTOCAL_ACCEL_P2P }
    })
}

/// The share of the ring the gyro takes.
fn gyro_share(source: Source) -> f32 {
    if source == Source::Signals { 1.0 } else { (CAL_S / SPIKE_S) as f32 }
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 0 { 0.0 } else if n % 2 == 1 { v[n / 2] } else { 0.5 * (v[n / 2 - 1] + v[n / 2]) }
}

/// The resting gyro of a still stretch: per axis, the mean of what lies
/// within [`OUTLIER_SIGMA`] robust σ of the median — a spike can't move it.
/// `None` when that isn't plausibly bias.
fn gyro_cal(stretch: &[Sample]) -> Option<GyroCal> {
    let limit = MAX_BIAS_DPS / GYRO_REF_DPS;
    let mut offset = [0.0f32; 3];
    let mut var = 0.0f64;
    for a in 0..3 {
        let mut v: Vec<f32> = stretch.iter().map(|s| s[a]).collect();
        let med = median(&mut v);
        let mut dev: Vec<f32> = v.iter().map(|x| (x - med).abs()).collect();
        let keep = (OUTLIER_SIGMA * 1.4826 * median(&mut dev)).max(OUTLIER_MIN);
        let inl: Vec<f64> = v.iter().filter(|x| (*x - med).abs() <= keep).map(|x| *x as f64).collect();
        if inl.is_empty() {
            return None;
        }
        let n = inl.len() as f64;
        let mean = inl.iter().sum::<f64>() / n;
        var += inl.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        if (mean as f32).abs() > limit {
            return None;
        }
        offset[a] = if (mean as f32).abs() < SNAP_EPS { 0.0 } else { mean as f32 };
    }
    Some(GyroCal { offset, floor: var.sqrt() as f32 })
}

/// Tune the spike filter from `samples` (a pad lying still) on a worker
/// thread — a full search replays the filter dozens of times. `None` if the
/// thread couldn't start.
pub(crate) fn spawn_tune(samples: Vec<Sample>) -> Option<TuneSlot> {
    let slot: TuneSlot = Arc::new(Mutex::new(None));
    let out = Arc::clone(&slot);
    std::thread::Builder::new()
        .name("spike-tune".into())
        .spawn(move || {
            let result = spike_tune::tune(&samples);
            if let Ok(mut o) = out.lock() {
                *o = Some(result);
            }
        })
        .ok()?;
    Some(slot)
}

fn analyse(stretch: Vec<Sample>) -> SpikeCheck {
    spawn_tune(stretch).map_or(SpikeCheck::Done(None), SpikeCheck::Analysing)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Outcome {
    Done,
    Cancelled,
    TimedOut,
    NoGyro,
}

#[derive(Clone, Default)]
struct Session {
    pads: Vec<PadCal>,
    started: f64,
    end: Option<(f64, Outcome)>,
}

impl Session {
    /// How long the outcome stays up before fading — long enough to read
    /// a spike result.
    fn hold_s(&self, o: Outcome) -> f64 {
        let spike_news = self.pads.iter().any(|p| spike_summary(p).is_some());
        match o {
            Outcome::Done if spike_news => 3.5,
            Outcome::Done | Outcome::Cancelled => 1.5,
            Outcome::TimedOut | Outcome::NoGyro => 3.0,
        }
    }
}

fn session_id() -> egui::Id { egui::Id::new("fxi_gyro_cal_session") }
fn passthrough_id() -> egui::Id { egui::Id::new("fxi_gyro_cal_passthrough_applied") }

/// A Calibrate Gyro press for `devs` (the pads feeding the card). Starts a
/// session for those that stream a gyro; while one is running, a press
/// cancels it.
pub(crate) fn request(ctx: &egui::Context, live: &LiveSignals, mut devs: Vec<String>) {
    if devs.is_empty() {
        return;
    }
    devs.sort();
    devs.dedup();
    let now = ctx.input(|i| i.time);
    let has_gyro = |d: &String| live.contains_key(&(d.clone(), "gyro_x".to_string()));
    let running = ctx.data(|d| d.get_temp::<Session>(session_id())).filter(|s| s.end.is_none());
    let session = match running {
        Some(mut s) if now - s.started < JOIN_S => {
            for d in devs.into_iter().filter(has_gyro) {
                if !s.pads.iter().any(|p| p.dev == d) {
                    s.pads.push(PadCal::new(d));
                }
            }
            s
        }
        // Pressing it means picking the pad up, so a press while one runs
        // can't be meant as a fresh start.
        Some(mut s) => {
            s.end = Some((now, Outcome::Cancelled));
            s
        }
        None => {
            let pads: Vec<PadCal> = devs.into_iter().filter(has_gyro).map(PadCal::new).collect();
            let end = pads.is_empty().then_some((now, Outcome::NoGyro));
            Session { pads, started: now, end }
        }
    };
    ctx.data_mut(|d| d.insert_temp(session_id(), session));
    ctx.request_repaint();
}

/// The six raw IMU values a pad's pins show this frame; `None` without a
/// gyro. A pad with no accelerometer is judged still by its gyro alone.
fn read_raw(live: &LiveSignals, dev: &str) -> Option<Sample> {
    const PINS: [&str; 6] = ["gyro_x", "gyro_y", "gyro_z", "accel_x", "accel_y", "accel_z"];
    let mut out = [0.0f32; 6];
    for (i, pin) in PINS.iter().enumerate() {
        match live.get(&(dev.to_string(), pin.to_string())) {
            Some(Signal::Float(v)) => out[i] = *v,
            _ if i < 3 => return None,
            _ => {}
        }
    }
    Some(out)
}

/// This frame's samples for `pad`, from its capture or its pins.
fn pull_samples(pad: &mut PadCal, live: &LiveSignals, now: f64) -> Vec<(f64, Sample)> {
    match pad.source {
        Source::Signals => read_raw(live, &pad.dev).map(|s| vec![(now, s)]).unwrap_or_default(),
        Source::Pending | Source::Capture => {
            let (got, total) = spike::capture_read(&pad.dev, pad.cursor);
            pad.cursor = total;
            if pad.source == Source::Pending {
                if !got.is_empty() {
                    pad.source = Source::Capture;
                } else if pad.armed_at.is_some_and(|t| now - t > CAPTURE_GRACE_S) {
                    spike::capture_stop(&pad.dev);
                    pad.source = Source::Signals;
                }
            }
            // Capture times onto the frame clock.
            let at = Instant::now();
            got.into_iter()
                .map(|(t, s)| (now - at.saturating_duration_since(t).as_secs_f64(), s))
                .collect()
        }
    }
}

/// Every top-level `device.source` node for `dev`, in one tab.
fn device_nodes(snarl: &Snarl<NodeData>, dev: &str) -> Vec<egui_snarl::NodeId> {
    snarl.node_ids()
        .filter(|(_, n)| n.module_id == "device.source"
            && n.params.get("device_id").and_then(|v| v.as_str()) == Some(dev))
        .map(|(id, _)| id)
        .collect()
}

/// Write a pad's gyro calibration to every `device.source` node for it, in
/// every tab — the bias belongs to the pad, not to a patch. Gyro only.
fn apply_gyro(ctx: &egui::Context, tabs: &mut [PatchTab], active_tab: usize, dev: &str, cal: GyroCal) {
    for (ti, tab) in tabs.iter_mut().enumerate() {
        let snarl = &mut tab.canvas.snarl;
        for id in device_nodes(snarl, dev) {
            if let Some(n) = snarl.get_node_mut(id) {
                n.params.insert("gyro_offset".into(), serde_json::json!(cal.offset));
                n.params.insert("gyro_noise_floor".into(), serde_json::json!(cal.floor));
                n.params.insert("gyro_calibrated".into(), Value::Bool(true));
            }
            if ti == active_tab {
                crate::panels::calibration::mark_calibrated(ctx, id);
            }
        }
    }
}

/// Turn a pad's spike filter on at `(window, sensitivity)`, on its nodes in
/// every tab and on the device right away.
fn apply_spike_filter(tabs: &mut [PatchTab], spike_settings: &SpikeSettings, dev: &str, (window, sens): (u8, f32)) {
    for tab in tabs.iter_mut() {
        let snarl = &mut tab.canvas.snarl;
        for id in device_nodes(snarl, dev) {
            if let Some(n) = snarl.get_node_mut(id) {
                n.params.insert("spike_suppress".into(), Value::Bool(true));
                n.params.insert("spike_sensitivity".into(), serde_json::json!(sens));
                n.params.insert("spike_window".into(), Value::Number(window.into()));
            }
        }
    }
    if let Ok(mut map) = spike_settings.write() {
        map.insert(dev.to_string(), (true, sens, window));
    }
}

/// Start capturing `dev`'s spike-filter input. The device thread binds a
/// filter to its id only while the pad has an entry in the settings it
/// pushes, so make sure it has one — its saved settings, else the filter's
/// own defaults, which changes nothing.
fn arm(tabs: &[PatchTab], active_tab: usize, spike_settings: &SpikeSettings, dev: &str) {
    if let Ok(mut map) = spike_settings.write() {
        if !map.contains_key(dev) {
            let saved = tabs.get(active_tab).and_then(|t| {
                device_nodes(&t.canvas.snarl, dev).first()
                    .and_then(|id| t.canvas.snarl.get_node(*id))
                    .map(|n| crate::panels::calibration::spike_params(&n.params))
            });
            map.insert(dev.to_string(), saved.unwrap_or((true, 50.0, 3)));
        }
    }
    spike::capture_start(dev);
}

/// The pad's name as its device node shows it, and its current gyro offset.
fn pad_info(snarl: &Snarl<NodeData>, dev: &str) -> (String, [f32; 3]) {
    let node = device_nodes(snarl, dev).first().and_then(|id| snarl.get_node(*id));
    let name = node.map(|n| n.display_name.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| dev.to_string());
    let mut off = [0.0f32; 3];
    if let Some(a) = node.and_then(|n| n.params.get("gyro_offset")).and_then(|v| v.as_array()) {
        for (i, v) in a.iter().take(3).enumerate() {
            off[i] = v.as_f64().unwrap_or(0.0) as f32;
        }
    }
    (name, off)
}

/// Run the session, if any, and draw its prompt (call once per frame from
/// `FlexInputApp::update`, beside the menu overlay). No-op without one.
pub(crate) fn show(
    ctx: &egui::Context,
    tabs: &mut [PatchTab],
    active_tab: usize,
    live: &LiveSignals,
    spike_settings: &SpikeSettings,
    overlay_fps: u32,
) {
    let Some(mut s) = ctx.data_mut(|d| d.remove_temp::<Session>(session_id())) else {
        // Viewport gone: a new one must get its passthrough again.
        ctx.data_mut(|d| d.remove_temp::<bool>(passthrough_id()));
        return;
    };
    let now = ctx.input(|i| i.time);

    if s.end.is_none() {
        for pad in &mut s.pads {
            if pad.armed_at.is_none() {
                arm(tabs, active_tab, spike_settings, &pad.dev);
                pad.armed_at = Some(now);
            }
            let batch = pull_samples(pad, live, now);
            pad.step(now, &batch);
            pad.refresh_marks(now);
            if let (Some(cal), false) = (pad.gyro, pad.gyro_applied) {
                apply_gyro(ctx, tabs, active_tab, &pad.dev, cal);
                pad.gyro_applied = true;
            }
            if let (SpikeCheck::Done(Some(t)), false) = (&pad.spikes, pad.spikes_applied) {
                if let Some(setting) = t.setting {
                    apply_spike_filter(tabs, spike_settings, &pad.dev, setting);
                }
                pad.spikes_applied = true;
            }
        }
        if s.pads.iter().all(PadCal::finished) {
            s.end = Some((now, Outcome::Done));
        } else if now - s.started > TIMEOUT_S {
            s.end = Some((now, Outcome::TimedOut));
        }
    }
    if s.end.is_some() {
        for pad in s.pads.iter_mut().filter(|p| p.armed_at.is_some()) {
            spike::capture_stop(&pad.dev);
            pad.armed_at = None;
        }
    }

    let alpha = match s.end {
        None => 1.0,
        Some((t, o)) => {
            let e = now - t - s.hold_s(o);
            if e >= FADE_S {
                ctx.data_mut(|d| d.remove_temp::<bool>(passthrough_id()));
                return; // viewport not declared → eframe destroys the OS window
            }
            (1.0 - e.max(0.0) / FADE_S) as f32
        }
    };

    let info: Vec<(String, [f32; 3])> = tabs.get(active_tab)
        .map(|t| s.pads.iter().map(|p| pad_info(&t.canvas.snarl, &p.dev)).collect())
        .unwrap_or_default();

    let monitor_size = ctx
        .input(|i| i.viewport().monitor_size)
        .filter(|m| m.x > 1.0 && m.y > 1.0)
        .unwrap_or(egui::vec2(1920.0, 1080.0));
    let viewport_id = egui::ViewportId::from_hash_of("fxi_gyro_cal_overlay");
    let builder = egui::ViewportBuilder::default()
        .with_title(TITLE)
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_mouse_passthrough(true)
        .with_active(false)
        .with_resizable(false)
        .with_has_shadow(false)
        .with_position(egui::pos2(0.0, 0.0))
        .with_inner_size(monitor_size);

    ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
        // Geometry self-correction from inside the viewport (see overlay.rs).
        let (inner_rect, child_monitor) = vctx.input(|i| {
            (i.viewport().inner_rect, i.viewport().monitor_size)
        });
        if let (Some(inner_rect), Some(want)) = (inner_rect, child_monitor) {
            if (inner_rect.size() - want).length() > 1.0 {
                vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
                vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
            }
        }
        if vctx.data(|d| d.get_temp::<bool>(passthrough_id())).is_none() {
            vctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(true));
            vctx.data_mut(|d| d.insert_temp(passthrough_id(), true));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(vctx, |ui| {
                ui.set_opacity(alpha);
                paint_prompt(ui, ui.max_rect(), &s, &info, now);
            });
    });

    ctx.data_mut(|d| d.insert_temp(session_id(), s));
    // Pace the parent context (immediate viewports render with the parent);
    // this is also the rate the pads are read at.
    ctx.request_repaint_after(Duration::from_secs_f64(1.0 / overlay_fps.max(1) as f64));
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What a spike check found and did, in a line, with its colour.
pub(crate) fn tuning_summary(t: &Tuning) -> (String, Color32) {
    let found = plural(t.report.bursts, "spike", "spikes");
    match t.setting {
        None if t.report.spikes == 0 => ("No spikes found".into(), Color32::from_gray(170)),
        None => (format!("{found} — the spike filter can't remove them"), ORANGE),
        Some((w, sens)) if t.left == 0 => (format!("{found} · spike filter set to window {w}, {sens:.0}%"), CYAN),
        Some((w, sens)) => (format!("{found} · window {w}, {sens:.0}% removes most"), ORANGE),
    }
}

/// What a pad's spike check found, once it has.
fn spike_summary(p: &PadCal) -> Option<(String, Color32)> {
    let SpikeCheck::Done(Some(t)) = &p.spikes else { return None };
    Some(tuning_summary(t))
}

/// One pad's status in a multi-pad prompt.
fn pad_status(p: &PadCal, now: f64) -> (String, Color32) {
    if let Some(s) = spike_summary(p) {
        return s;
    }
    if matches!(p.spikes, SpikeCheck::Analysing(_)) {
        return ("analysing spikes…".into(), CYAN);
    }
    if p.finished() {
        return ("calibrated".into(), GREEN);
    }
    if p.since.is_some() {
        let pct = p.progress(now) * 100.0;
        return if p.gyro.is_some() {
            (format!("checking for spikes {pct:.0}%"), CYAN)
        } else {
            (format!("calibrating {pct:.0}%"), ORANGE)
        };
    }
    ("lay it flat".into(), Color32::from_gray(200))
}

const TOP_H: f32 = 204.0;
const SCOPE_H: f32 = 62.0;
const SCOPE_HEAD: f32 = 18.0;
const SCOPE_GAP: f32 = 8.0;

/// The prompt card, centred a little above the middle of `screen`.
fn paint_prompt(ui: &egui::Ui, screen: egui::Rect, s: &Session, info: &[(String, [f32; 3])], now: f64) {
    let multi = s.pads.len() > 1;
    let head = if multi { SCOPE_HEAD } else { 0.0 };
    let w = 440.0;
    let h = TOP_H + s.pads.len() as f32 * (head + SCOPE_H + SCOPE_GAP) + 10.0;
    let card = egui::Rect::from_center_size(
        egui::pos2(screen.center().x, screen.top() + screen.height() * 0.42),
        egui::vec2(w, h),
    );
    let p = ui.painter();
    p.rect_filled(card, 14.0, Color32::from_rgba_unmultiplied(18, 20, 24, 228));
    p.rect_stroke(card, 14.0, Stroke::new(1.0, Color32::from_white_alpha(40)), egui::StrokeKind::Inside);
    p.text(egui::pos2(card.center().x, card.top() + 26.0), Align2::CENTER_CENTER,
        flexinput_core::imu::GYRO_CAL_NAME, FontId::proportional(19.0), Color32::from_gray(235));

    let vis = egui::pos2(card.center().x, card.top() + 98.0);
    let t = ui.input(|i| i.time);
    let progress = if s.pads.is_empty() {
        0.0
    } else {
        s.pads.iter().map(|p| p.progress(now)).sum::<f32>() / s.pads.len() as f32
    };
    let share = s.pads.iter().map(|p| gyro_share(p.source)).fold(0.0f32, f32::max).max(0.01);
    let sampling = s.pads.iter().any(|p| p.since.is_some() && p.sampling());
    let analysing = s.pads.iter().any(|p| matches!(p.spikes, SpikeCheck::Analysing(_)));
    let all_down = s.pads.iter().all(|p| p.since.is_some() || !p.sampling());
    let spike_phase = s.pads.iter().any(|p| p.since.is_some() && p.gyro.is_some() && p.sampling());
    let moved = s.pads.iter().any(|p| p.since.is_none() && p.sampling()
        && p.moved_at.is_some_and(|m| now - m < MOVED_NOTE_S));
    let laid = if multi { "Lay the controllers flat and let go" } else { "Lay the controller flat and let go" };
    let single_summary = if multi { None } else { s.pads.first().and_then(spike_summary) };

    let (msg, msg_col, sub, sub_col): (&str, Color32, String, Color32) = match s.end {
        Some((t_end, Outcome::Done)) => {
            ring(p, vis, 1.0, share, None);
            check(p, vis, ((now - t_end) / 0.3).clamp(0.0, 1.0) as f32);
            let (sub, col) = single_summary.unwrap_or_default();
            ("Gyro calibrated", GREEN, sub, col)
        }
        Some((_, Outcome::Cancelled)) => {
            cross(p, vis, Color32::from_gray(170));
            ("Calibration cancelled", Color32::from_gray(210), String::new(), Color32::GRAY)
        }
        Some((_, Outcome::TimedOut)) => {
            ring(p, vis, progress, share, None);
            cross(p, vis, ORANGE);
            let any = s.pads.iter().any(|p| p.gyro.is_some());
            ("It didn't hold still — calibration stopped", ORANGE,
             if any { "What finished is kept".into() } else { String::new() }, Color32::from_gray(150))
        }
        Some((_, Outcome::NoGyro)) => {
            cross(p, vis, Color32::from_gray(170));
            ("Nothing to calibrate", Color32::from_gray(210),
             "This controller doesn't report a gyro".into(), Color32::from_gray(150))
        }
        None if analysing && !sampling => {
            ring(p, vis, 1.0, share, Some(t));
            ("Analysing spikes…", Color32::from_gray(230), String::new(), Color32::GRAY)
        }
        None if sampling => {
            ring(p, vis, progress, share, Some(t));
            p.text(vis, Align2::CENTER_CENTER, format!("{:.0}%", progress * 100.0),
                FontId::proportional(17.0), Color32::from_gray(230));
            let msg = if !all_down {
                laid
            } else if spike_phase {
                "Checking for spikes — keep it still"
            } else {
                "Calibrating gyro — keep it still"
            };
            let sub = if !all_down { "Calibrating the ones lying still" } else { "" };
            (msg, Color32::from_gray(230), sub.into(), Color32::from_gray(150))
        }
        None => {
            lay_down(ui, p, vis, t);
            let picked_up = s.pads.iter().any(|p| p.gyro.is_some());
            let sub = if moved && picked_up {
                "Moved — lay it down again to finish the spike check"
            } else if moved {
                "Moved — waiting for it to settle"
            } else {
                "Starts once it's still · press again to cancel"
            };
            (laid, Color32::from_gray(230), sub.into(), Color32::from_gray(150))
        }
    };
    p.text(egui::pos2(card.center().x, card.top() + 164.0), Align2::CENTER_CENTER,
        msg, FontId::proportional(16.0), msg_col);
    if !sub.is_empty() {
        p.text(egui::pos2(card.center().x, card.top() + 187.0), Align2::CENTER_CENTER,
            sub, FontId::proportional(13.0), sub_col);
    }

    let mut y = card.top() + TOP_H;
    for (i, pad) in s.pads.iter().enumerate() {
        let (name, offset) = info.get(i).cloned().unwrap_or_else(|| (pad.dev.clone(), [0.0; 3]));
        if multi {
            p.text(egui::pos2(card.left() + 22.0, y + SCOPE_HEAD * 0.5), Align2::LEFT_CENTER,
                &name, FontId::proportional(13.0), Color32::from_gray(220));
            let (status, col) = pad_status(pad, now);
            p.text(egui::pos2(card.right() - 22.0, y + SCOPE_HEAD * 0.5), Align2::RIGHT_CENTER,
                status, FontId::proportional(12.0), col);
            y += SCOPE_HEAD;
        }
        let rect = egui::Rect::from_min_size(egui::pos2(card.left() + 18.0, y), egui::vec2(w - 36.0, SCOPE_H));
        scope(p, rect, pad, offset);
        y += SCOPE_H + SCOPE_GAP;
    }
}

/// A pad's gyro over the last [`SCOPE_S`], offset removed: roll, pitch, yaw,
/// auto-zoomed, with the spikes the check sees marked above them. From a
/// capture it is every sample the spike filter is fed, before it — spikes
/// the filter removes show here too.
fn scope(p: &egui::Painter, rect: egui::Rect, pad: &PadCal, offset: [f32; 3]) {
    p.rect_filled(rect, 5.0, Color32::from_gray(12));
    p.rect_stroke(rect, 5.0, Stroke::new(1.0, Color32::from_gray(48)), egui::StrokeKind::Inside);
    let plot = rect.shrink2(egui::vec2(8.0, 9.0));
    let mid = plot.center().y;
    p.line_segment([egui::pos2(plot.left(), mid), egui::pos2(plot.right(), mid)],
        Stroke::new(1.0, Color32::from_gray(40)));

    let n = pad.recent.len();
    let small = FontId::proportional(10.0);
    let label = match pad.source {
        Source::Capture => "gyro · raw, before the spike filter",
        Source::Signals => "gyro",
        Source::Pending => "gyro · connecting…",
    };
    p.text(egui::pos2(rect.left() + 6.0, rect.bottom() - 3.0), Align2::LEFT_BOTTOM,
        label, small.clone(), Color32::from_gray(100));
    if n < 2 {
        return;
    }
    let peak = pad.recent.iter()
        .flat_map(|(_, s)| (0..3).map(move |a| (s[a] - offset[a]).abs()))
        .fold(0.0f32, f32::max);
    // ±5 °/s at the least, so a quiet pad's noise doesn't fill the strip.
    let range = (peak * 1.15).max(5.0 / GYRO_REF_DPS);
    let k = plot.height() * 0.5 / range;
    p.text(egui::pos2(rect.right() - 6.0, rect.top() + 3.0), Align2::RIGHT_TOP,
        format!("±{:.0} °/s", range * GYRO_REF_DPS), small, Color32::from_gray(100));

    let x_of = |i: usize| plot.left() + plot.width() * i as f32 / (n - 1) as f32;
    let y_of = |v: f32| (mid - v * k).clamp(plot.top(), plot.bottom());
    let cols = plot.width().max(1.0) as usize;
    for (a, col) in AXIS_COLORS.iter().enumerate() {
        let v = |i: usize| pad.recent[i].1[a] - offset[a];
        let pts: Vec<Pos2> = if n <= cols * 2 {
            (0..n).map(|i| egui::pos2(x_of(i), y_of(v(i)))).collect()
        } else {
            // More samples than pixels: each column's min and max, zig-zagged,
            // so a one-sample spike still reaches its full height.
            let mut pts = Vec::with_capacity(cols * 2);
            for c in 0..cols {
                let (i0, i1) = (c * n / cols, ((c + 1) * n / cols).max(c * n / cols + 1));
                let (lo, hi) = (i0..i1.min(n)).map(v).fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), x| (lo.min(x), hi.max(x)));
                let x = plot.left() + c as f32;
                let (a0, a1) = if c % 2 == 0 { (lo, hi) } else { (hi, lo) };
                pts.push(egui::pos2(x, y_of(a0)));
                pts.push(egui::pos2(x, y_of(a1)));
            }
            pts
        };
        p.add(egui::Shape::line(pts, Stroke::new(1.3, *col)));
    }
    for &tm in &pad.marks {
        let i = pad.recent.partition_point(|(t, _)| *t < tm);
        if i < n {
            let x = x_of(i);
            let tip = egui::pos2(x, rect.top() + 9.0);
            p.add(egui::Shape::convex_polygon(
                vec![tip, egui::pos2(x - 4.0, rect.top() + 2.0), egui::pos2(x + 4.0, rect.top() + 2.0)],
                SPIKE_RED, Stroke::NONE,
            ));
        }
    }
}

const RING_R: f32 = 38.0;

/// Progress ring filling clockwise from the top: orange through the gyro's
/// `share`, cyan through the spike check after it. With `spin` (the clock) a
/// short comet circles inside it while it works.
fn ring(p: &egui::Painter, c: Pos2, progress: f32, share: f32, spin: Option<f64>) {
    use std::f32::consts::{FRAC_PI_2, TAU};
    p.circle_stroke(c, RING_R, Stroke::new(6.0, Color32::from_white_alpha(28)));
    let arc = |r: f32, a0: f32, sweep: f32| -> Vec<Pos2> {
        let n = ((sweep.abs() / TAU) * 72.0).ceil().max(2.0) as usize;
        (0..=n).map(|k| {
            let a = a0 + sweep * k as f32 / n as f32;
            egui::pos2(c.x + r * a.cos(), c.y + r * a.sin())
        }).collect()
    };
    let top = -FRAC_PI_2;
    let gyro_end = progress.min(share);
    if gyro_end > 0.001 {
        p.add(egui::Shape::line(arc(RING_R, top, gyro_end * TAU), Stroke::new(6.0, ORANGE)));
    }
    if progress > share + 0.001 {
        p.add(egui::Shape::line(arc(RING_R, top + share * TAU, (progress - share) * TAU), Stroke::new(6.0, CYAN)));
    }
    if share < 0.999 {
        // Where the spike check starts.
        let a = top + share * TAU;
        let (i, o) = (RING_R - 5.0, RING_R + 5.0);
        p.line_segment([egui::pos2(c.x + i * a.cos(), c.y + i * a.sin()), egui::pos2(c.x + o * a.cos(), c.y + o * a.sin())],
            Stroke::new(1.5, Color32::from_white_alpha(90)));
    }
    if let Some(t) = spin {
        const SEGS: usize = 14;
        let col = if progress > share { CYAN } else { ORANGE };
        let head = (t * 1.3).fract() as f32 * TAU + top;
        let r = RING_R - 11.0;
        for k in 0..SEGS {
            let a0 = head - 0.09 * k as f32;
            let fade = 1.0 - k as f32 / SEGS as f32;
            p.add(egui::Shape::line(arc(r, a0, -0.09), Stroke::new(2.5, col.gamma_multiply(fade * 0.8))));
        }
    }
}

/// A check drawn in over `k` 0..1.
fn check(p: &egui::Painter, c: Pos2, k: f32) {
    let pts = [egui::pos2(c.x - 16.0, c.y + 1.0), egui::pos2(c.x - 5.0, c.y + 12.0), egui::pos2(c.x + 17.0, c.y - 11.0)];
    let l0 = pts[0].distance(pts[1]);
    let l1 = pts[1].distance(pts[2]);
    let mut len = k * (l0 + l1);
    let mut line = vec![pts[0]];
    for (a, b, l) in [(pts[0], pts[1], l0), (pts[1], pts[2], l1)] {
        if len <= 0.0 {
            break;
        }
        line.push(a + (b - a) * (len / l).min(1.0));
        len -= l;
    }
    if line.len() > 1 {
        p.add(egui::Shape::line(line, Stroke::new(5.0, GREEN)));
    }
}

fn cross(p: &egui::Painter, c: Pos2, col: Color32) {
    let d = 13.0;
    let st = Stroke::new(4.5, col);
    p.line_segment([egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)], st);
    p.line_segment([egui::pos2(c.x - d, c.y + d), egui::pos2(c.x + d, c.y - d)], st);
}

/// The pad art (`gamepad_flat_front.svg`, a pad lying flat seen from the
/// front), rasterized at `px` pixels square.
fn pad_texture(ctx: &egui::Context, px: u32) -> Option<egui::TextureHandle> {
    let key = egui::Id::new(("gyro_cal_pad_svg", px));
    if let Some(h) = crate::widgets::tex_cache::get(ctx, key) {
        return Some(h);
    }
    let text = std::str::from_utf8(PAD_SVG).ok()?;
    let img = crate::canvas::viewer::rasterize_svg_recolored(text, px, px, "override", Color32::TRANSPARENT)?;
    let h = ctx.load_texture(format!("gyro_cal_pad_{px}"), img, egui::TextureOptions::LINEAR);
    crate::widgets::tex_cache::put(ctx, key, h.clone());
    Some(h)
}

/// The pad's bottom edge in its art, as a fraction of the square below its
/// centre (the art spans 34–66 % of its height).
const PAD_ART_BOTTOM: f32 = 0.161;
/// Side of the art's square, points.
const PAD_ART_SIZE: f32 = 150.0;

/// The pad settling onto a table, on a loop: it lowers, lands, and a ripple
/// spreads from where it rests.
fn lay_down(ui: &egui::Ui, p: &egui::Painter, c: Pos2, t: f64) {
    const PERIOD: f64 = 2.0;
    let ph = (t / PERIOD).fract() as f32;
    let table_y = c.y + 26.0;
    p.line_segment([egui::pos2(c.x - 82.0, table_y), egui::pos2(c.x + 82.0, table_y)],
        Stroke::new(2.0, Color32::from_white_alpha(70)));

    // Lower over the first half (ease-out), rest for the second.
    let fall = (ph / 0.5).min(1.0);
    let lift = (1.0 - fall).powi(3) * 34.0;
    let fade_in = (ph / 0.12).min(1.0);
    let px = (PAD_ART_SIZE * ui.ctx().pixels_per_point()).round() as u32;
    if let Some(tex) = pad_texture(ui.ctx(), px) {
        let centre = egui::pos2(c.x, table_y - 2.0 - PAD_ART_SIZE * PAD_ART_BOTTOM - lift);
        let rect = egui::Rect::from_center_size(centre, egui::vec2(PAD_ART_SIZE, PAD_ART_SIZE));
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        p.image(tex.id(), rect, uv, Color32::from_gray(225).gamma_multiply(fade_in));
    }

    // Landed: a ripple along the table.
    if ph > 0.5 {
        let k = (ph - 0.5) / 0.5;
        let r = 40.0 + k * 60.0;
        let st = Stroke::new(2.0, ORANGE.gamma_multiply((1.0 - k) * 0.6));
        p.line_segment([egui::pos2(c.x - r, table_y + 5.0), egui::pos2(c.x - r + 14.0, table_y + 5.0)], st);
        p.line_segment([egui::pos2(c.x + r - 14.0, table_y + 5.0), egui::pos2(c.x + r, table_y + 5.0)], st);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f64 = 1.0 / 250.0;
    /// A resting gyro bias, normalized (0.4, −0.3, 0.5 °/s) — above the
    /// quantization snap, which would zero it.
    const BIAS: [f32; 3] = [0.0002, -0.00015, 0.00025];

    /// A pad at rest: bias plus a little noise, gravity on z.
    fn rest(k: usize) -> Sample {
        let n = if k % 2 == 0 { 0.00002 } else { -0.00002 };
        [BIAS[0] + n, BIAS[1] - n, BIAS[2] + n, 0.0, 0.0, 0.125 + n]
    }

    fn pad(source: Source) -> PadCal {
        let mut p = PadCal::new("pad".into());
        p.source = source;
        p
    }

    /// Feed `secs` of samples at 250 Hz, one 60 fps frame at a time.
    fn run(pad: &mut PadCal, from: f64, secs: f64, sample: impl Fn(usize) -> Option<Sample>) -> f64 {
        let n = (secs / DT).round() as usize;
        let mut batch = Vec::new();
        let mut frame_end = from + 1.0 / 60.0;
        for k in 0..n {
            let t = from + k as f64 * DT;
            if t >= frame_end {
                pad.step(frame_end, &batch);
                batch.clear();
                frame_end += 1.0 / 60.0;
            }
            if let Some(s) = sample(k) {
                batch.push((t, s));
            }
        }
        let end = from + n as f64 * DT;
        pad.step(end, &batch);
        end
    }

    fn wait_for_spikes(pad: &mut PadCal, mut t: f64) {
        for _ in 0..500 {
            if !matches!(pad.spikes, SpikeCheck::Analysing(_)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
            t += 0.005;
            pad.step(t, &[]);
        }
    }

    #[test]
    fn a_pad_lying_still_calibrates_to_its_resting_rate() {
        let mut p = pad(Source::Capture);
        let t = run(&mut p, 0.0, STEADY_S + CAL_S - 0.5, |k| Some(rest(k)));
        assert!(p.gyro.is_none(), "not before the stillness and the sampling");
        run(&mut p, t, 0.8, |k| Some(rest(k)));
        let cal = p.gyro.expect("calibrated");
        for i in 0..3 {
            assert!((cal.offset[i] - BIAS[i]).abs() < 1e-5, "{:?}", cal.offset);
        }
        assert!(cal.floor > 0.0 && cal.floor < 0.0001, "{}", cal.floor);
    }

    #[test]
    fn spikes_neither_unsettle_the_pad_nor_bias_its_offset() {
        let mut p = pad(Source::Capture);
        let spiky = |k: usize| {
            let mut s = rest(k);
            if k % 97 == 40 {
                s[2] += 0.1; // 200 °/s for one sample, ~2.5 times a second
            }
            Some(s)
        };
        let t = run(&mut p, 0.0, STEADY_S + SPIKE_S + 0.3, spiky);
        let cal = p.gyro.expect("settles despite the spikes");
        assert!((cal.offset[2] - BIAS[2]).abs() < 1e-5, "{:?}", cal.offset);
        wait_for_spikes(&mut p, t);
        let SpikeCheck::Done(Some(tuning)) = &p.spikes else { panic!("{:?}", p.spikes) };
        assert!(tuning.report.bursts >= 10, "{tuning:?}");
        assert_eq!((tuning.setting.map(|(w, _)| w), tuning.left), (Some(3), 0));
        assert!(p.finished());
    }

    #[test]
    fn a_quiet_pad_finds_no_spikes() {
        let mut p = pad(Source::Capture);
        let t = run(&mut p, 0.0, STEADY_S + SPIKE_S + 0.3, |k| Some(rest(k)));
        wait_for_spikes(&mut p, t);
        let SpikeCheck::Done(Some(tuning)) = &p.spikes else { panic!("{:?}", p.spikes) };
        assert_eq!((tuning.report.spikes, tuning.setting), (0, None));
    }

    #[test]
    fn without_a_spike_filter_it_stops_at_the_gyro() {
        let mut p = pad(Source::Signals);
        run(&mut p, 0.0, STEADY_S + CAL_S + 0.3, |k| Some(rest(k)));
        assert!(p.gyro.is_some());
        assert!(matches!(p.spikes, SpikeCheck::Unavailable));
        assert!(p.finished());
    }

    #[test]
    fn moving_it_starts_the_pad_over() {
        let mut p = pad(Source::Capture);
        let t = run(&mut p, 0.0, STEADY_S + 2.0, |k| Some(rest(k)));
        assert!(p.progress(t) > 0.2);
        // Picked up: a swing on yaw.
        let t = run(&mut p, t, 0.3, |k| {
            let mut s = rest(k);
            s[2] += 0.02 * (k as f32 * 0.3).sin();
            Some(s)
        });
        assert_eq!(p.progress(t), 0.0);
        assert!(p.moved_at.is_some());
        // Set down again: the full stretch again, from scratch.
        let t = run(&mut p, t, STEADY_S + CAL_S - 0.5, |k| Some(rest(k)));
        assert!(p.gyro.is_none());
        run(&mut p, t, 0.8, |k| Some(rest(k)));
        assert!(p.gyro.is_some());
    }

    #[test]
    fn hand_tremor_never_counts_as_still() {
        let mut p = pad(Source::Capture);
        run(&mut p, 0.0, 10.0, |k| {
            let mut s = rest(k);
            let w = (k as f32 * 0.25).sin(); // ~10 Hz at 250 Hz
            s[0] += 0.004 * w;
            s[1] -= 0.003 * w;
            s[4] += 0.006 * w;
            Some(s)
        });
        assert!(p.gyro.is_none());
    }

    #[test]
    fn a_frozen_stream_mid_turn_is_not_a_bias() {
        let mut p = pad(Source::Capture);
        run(&mut p, 0.0, 6.0, |_| Some([0.0, 0.0, 0.02, 0.0, 0.0, 0.125]));
        assert!(p.gyro.is_none());
    }

    #[test]
    fn a_pad_that_stops_streaming_waits() {
        let mut p = pad(Source::Capture);
        let t = run(&mut p, 0.0, 2.0, |k| Some(rest(k)));
        let t = run(&mut p, t, 0.5, |_| None);
        assert_eq!(p.progress(t), 0.0);
        let t = run(&mut p, t, STEADY_S + CAL_S - 0.5, |k| Some(rest(k)));
        assert!(p.gyro.is_none());
        run(&mut p, t, 0.8, |k| Some(rest(k)));
        assert!(p.gyro.is_some());
    }

    #[test]
    fn a_pad_without_an_accelerometer_is_judged_by_its_gyro() {
        let live: LiveSignals = [("gyro_x", 0.1f32), ("gyro_y", 0.2), ("gyro_z", 0.3)]
            .into_iter()
            .map(|(p, v)| (("pad".to_string(), p.to_string()), Signal::Float(v)))
            .collect();
        assert_eq!(read_raw(&live, "pad"), Some([0.1, 0.2, 0.3, 0.0, 0.0, 0.0]));
        assert_eq!(read_raw(&live, "other"), None);
    }
}
