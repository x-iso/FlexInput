//! A raw pad's motion sensor: spotting one, and the guided setup that maps it
//! onto the canonical gyro / accel pins.
//!
//! **Spotting.** An [`Analyzer`] samples the pad's axis-like elements once a
//! frame. A motion sensor never sits still — its reading jitters by a few
//! counts all the time, through many distinct values, even with the pad on a
//! table — while a stick or trigger at rest reads one value, and a counter
//! or timestamp only ever climbs. Those verdicts stick for the session, so
//! Learn can ignore the sensor and the counters (they would otherwise be
//! "moved" constantly) and the node can say a sensor is there. Vendor blobs
//! (`raw_hid`) are read as little-endian 16-bit words at both alignments; the
//! right one is the one whose words sit still apart from that jitter.
//!
//! **Setup.** A [`Wizard`] walks the user through six poses and turns and
//! hands what it recorded to `raw_pad::solve_motion`, which works out which
//! elements are the accelerometer and the gyro, their frame and their scale.

use super::*;

use std::collections::{HashMap, HashSet, VecDeque};

use flexinput_core::raw_pad::{
    solve_motion, ElemKind, HidField, MotionCapture, MotionResult, RawPadProfile, RawSrc, RawValue,
};
use flexinput_devices::raw_hid::{self, MotionHint};

/// Frames of history the verdicts are drawn from (~1.5 s at 60 fps).
const HISTORY: usize = 90;
/// Frames needed before a verdict is drawn at all.
const MIN_FRAMES: usize = 60;

/// What a reading's recent history says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Character {
    /// Always jittering by a little, through many values: a motion sensor.
    Sensor,
    /// Only ever climbing (a big drop is a wrap): a counter or timestamp.
    Counter,
}

/// Judge a series of readings (−1..1 scale), oldest first.
pub(crate) fn characterize(s: &[f32]) -> Option<Character> {
    if s.len() < MIN_FRAMES {
        return None;
    }
    let d: Vec<f32> = s.windows(2).map(|w| w[1] - w[0]).collect();
    let nonzero = d.iter().filter(|x| **x != 0.0).count();
    if (nonzero as f32) < 0.3 * d.len() as f32 {
        return None;
    }
    let ups = d.iter().filter(|x| **x > 0.0).count();
    let small_downs = d.iter().filter(|x| **x < 0.0 && **x > -0.5).count();
    // A counter climbs — steadily, and a real distance. Noise that happens
    // to step up more often than down covers next to none.
    let climbed: f32 = d.iter().filter(|x| **x > -0.5).sum();
    if ups >= 20 && ups as f32 >= 0.85 * (ups + small_downs) as f32 && climbed >= 0.05 {
        return Some(Character::Counter);
    }
    let lo = s.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = s.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut distinct: Vec<u32> = s.iter().map(|v| v.to_bits()).collect();
    distinct.sort_unstable();
    distinct.dedup();
    (hi - lo < 0.15 && distinct.len() >= 4 && d.iter().all(|x| x.abs() < 0.08))
        .then_some(Character::Sensor)
}

fn word_at(frame: &[u8], o: usize) -> f32 {
    i16::from_le_bytes([frame[o], frame[o + 1]]) as f32 / 32767.0
}

/// The motion-sensor words in a blob's recent frames: `(offset, range)` of
/// every word that reads like a sensor, overlaps resolved toward the steadier
/// one (the right alignment jitters by a count; the wrong one by 256), plus
/// the offsets of words that count.
pub(crate) fn blob_words(frames: &VecDeque<Vec<u8>>) -> (Vec<usize>, Vec<usize>) {
    let len = frames.front().map_or(0, |f| f.len());
    if frames.len() < MIN_FRAMES || len < 2 || frames.iter().any(|f| f.len() != len) {
        return (Vec::new(), Vec::new());
    }
    let mut sensors: Vec<(usize, f32)> = Vec::new();
    let mut counters = Vec::new();
    for o in 0..len - 1 {
        let series: Vec<f32> = frames.iter().map(|f| word_at(f, o)).collect();
        match characterize(&series) {
            Some(Character::Sensor) => {
                let lo = series.iter().copied().fold(f32::INFINITY, f32::min);
                let hi = series.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                sensors.push((o, hi - lo));
            }
            Some(Character::Counter) => counters.push(o),
            None => {}
        }
    }
    sensors.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut chosen: Vec<usize> = Vec::new();
    for (o, _) in sensors {
        if chosen.iter().all(|c| c.abs_diff(o) >= 2) && counters.iter().all(|c| c.abs_diff(o) >= 2) {
            chosen.push(o);
        }
    }
    chosen.sort_unstable();
    (chosen, counters)
}

/// What the analyzer has concluded about a raw pad's elements. Every verdict
/// is sticky for the session.
#[derive(Clone, Default)]
pub(crate) struct Verdicts {
    /// Elements that read like a motion sensor.
    pub sensors: HashSet<RawSrc>,
    /// Elements that only count.
    pub counters: HashSet<RawSrc>,
    /// Blob bytes that change on their own (sensor, counter, noise):
    /// `(report, byte)`. Learn ignores them.
    pub noisy_bytes: HashSet<(u8, u16)>,
    /// Sensor words found in blobs, as fields.
    pub words: Vec<RawSrc>,
    frames: usize,
}

impl Verdicts {
    /// Enough history for the verdicts to mean anything.
    pub fn ready(&self) -> bool {
        self.frames >= MIN_FRAMES
    }

    /// Elements Learn must not listen to: they move on their own.
    pub fn restless(&self) -> impl Iterator<Item = RawSrc> + '_ {
        self.sensors.iter().chain(self.counters.iter()).copied()
    }
}

/// A raw pad's recent readings, and what they add up to.
#[derive(Clone, Default)]
pub(crate) struct Analyzer {
    last_t: f64,
    series: HashMap<RawSrc, VecDeque<f32>>,
    /// Blob `(report, first byte, length)` → its recent payload slices.
    blob_frames: HashMap<(u8, u16, u16), VecDeque<Vec<u8>>>,
    v: Verdicts,
}

impl Analyzer {

    /// Take this frame's readings (once per frame, however often the node
    /// is drawn) and update the verdicts.
    pub fn sample(
        &mut self,
        t: f64,
        live: &std::collections::HashMap<(String, String), Signal>,
        dev_id: &str,
        hid: Option<&raw_hid::RawHidInfo>,
        reports: Option<&std::collections::HashMap<u8, Vec<u8>>>,
    ) {
        if t == self.last_t {
            return;
        }
        self.last_t = t;
        self.v.frames += 1;
        for ((d, pin), sig) in live.iter() {
            if d != dev_id {
                continue;
            }
            let Some(src) = RawSrc::from_pin_id(pin) else { continue };
            if src.kind() != ElemKind::Axis {
                continue;
            }
            let ring = self.series.entry(src).or_default();
            ring.push_back(sig.as_float());
            if ring.len() > HISTORY {
                ring.pop_front();
            }
        }
        if let (Some(hid), Some(reports)) = (hid, reports) {
            for &(report, start, len) in &hid.blobs {
                let Some(payload) = reports.get(&report) else { continue };
                let (s, e) = (start as usize, (start + len) as usize);
                if payload.len() < e {
                    continue;
                }
                let ring = self.blob_frames.entry((report, start, len)).or_default();
                ring.push_back(payload[s..e].to_vec());
                if ring.len() > HISTORY {
                    ring.pop_front();
                }
            }
        }
        // Re-judge a few times a second: verdicts need many frames anyway.
        if self.v.frames % 10 != 0 {
            return;
        }
        let v = &mut self.v;
        for (src, ring) in &self.series {
            let series: Vec<f32> = ring.iter().copied().collect();
            match characterize(&series) {
                Some(Character::Sensor) => { v.sensors.insert(*src); }
                Some(Character::Counter) => { v.counters.insert(*src); }
                None => {}
            }
        }
        for (&(report, start, len), frames) in &self.blob_frames {
            if frames.len() < MIN_FRAMES {
                continue;
            }
            for b in 0..len as usize {
                let changes = frames.iter().zip(frames.iter().skip(1)).filter(|(x, y)| x[b] != y[b]).count();
                if changes * 5 >= frames.len() {
                    v.noisy_bytes.insert((report, start + b as u16));
                }
            }
            let (words, counters) = blob_words(frames);
            for o in counters {
                v.noisy_bytes.insert((report, start + o as u16));
                v.noisy_bytes.insert((report, start + o as u16 + 1));
            }
            for o in words {
                let src = RawSrc::Field(HidField::word(report, start + o as u16));
                let overlaps = v.words.iter().any(|w| match w {
                    RawSrc::Field(f) => f.report == report && (f.bit / 8).abs_diff(start + o as u16) < 2,
                    _ => false,
                });
                if !overlaps {
                    v.words.push(src);
                }
                v.noisy_bytes.insert((report, start + o as u16));
                v.noisy_bytes.insert((report, start + o as u16 + 1));
            }
        }
    }

}

/// The elements motion setup records: every axis-like element the layout
/// doesn't use, the sensor words found in blobs and the fields whose usage
/// says "sensor" — minus counters, which would never hold still.
pub(crate) fn candidates(
    analyzer: &Verdicts,
    live: &std::collections::HashMap<(String, String), Signal>,
    dev_id: &str,
    hid: Option<&raw_hid::RawHidInfo>,
    profile: &RawPadProfile,
) -> Vec<RawSrc> {
    let bound: Vec<RawSrc> = profile.binds.iter().map(|b| b.src).collect();
    let mut out: Vec<RawSrc> = live.keys()
        .filter(|(d, _)| d == dev_id)
        .filter_map(|(_, pin)| RawSrc::from_pin_id(pin))
        .filter(|s| s.kind() == ElemKind::Axis)
        .chain(analyzer.words.iter().copied())
        .chain(hid.into_iter().flat_map(|h| h.fields.iter()
            .filter(|(s, _, hint)| hint.is_some() && s.kind() == ElemKind::Axis)
            .map(|(s, _, _)| *s)))
        .filter(|s| !bound.contains(s) && !analyzer.counters.contains(s))
        .collect();
    out.sort_by_key(|s| format!("{s:?}"));
    out.dedup();
    out
}

/// How many elements look like a motion sensor, by jitter or by usage.
pub(crate) fn sensor_count(analyzer: &Verdicts, hid: Option<&raw_hid::RawHidInfo>) -> usize {
    let hinted = hid.map_or(0, |h| h.fields.iter()
        .filter(|(s, _, hint)| matches!(hint, Some(MotionHint::Accel | MotionHint::Gyro))
            && !analyzer.sensors.contains(s))
        .count());
    analyzer.sensors.len() + analyzer.words.len() + hinted
}

/// Read the candidates now: SDL axes and published fields from the live pins,
/// blob words from the latest reports.
pub(crate) fn read_values(
    srcs: &[RawSrc],
    live: &std::collections::HashMap<(String, String), Signal>,
    dev_id: &str,
    reports: Option<&std::collections::HashMap<u8, Vec<u8>>>,
) -> Option<Vec<f32>> {
    srcs.iter().map(|s| match s {
        RawSrc::Field(f) => reports?.get(&f.report).and_then(|p| f.value(p)).map(RawValue::as_f32),
        _ => live.get(&(dev_id.to_string(), s.pin_id())).map(|v| v.as_float()),
    }).collect()
}

/// How still "still" is: every candidate within this much over the window.
const STILL_RANGE: f32 = 0.05;
/// How long a pose must be held.
const STILL_SECS: f64 = 0.7;
/// How far a candidate must stray from the last pose to count as moving.
const MOVE_DELTA: f32 = 0.06;

/// The setup's steps: what to ask, and which integral the move fills.
const STEPS: [(&str, Turn); 6] = [
    ("Lay the controller flat, face up, and let go.", Turn::None),
    ("Tilt its nose up until it stands on its grips, pointing at the ceiling — and hold it there.", Turn::Pitch),
    ("Lay it flat again.", Turn::None),
    ("Roll it onto its right side (right grip down) — and hold it there.", Turn::Roll),
    ("Lay it flat again.", Turn::None),
    ("Turn it to the right on the table, about a quarter turn, and stop.", Turn::Yaw),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Turn {
    None,
    Pitch,
    Roll,
    Yaw,
}

/// The guided motion setup on one node.
#[derive(Clone, Default)]
pub(crate) struct Wizard {
    pub active: bool,
    srcs: Vec<RawSrc>,
    step: usize,
    /// The last pose's readings: where "moved" is measured from and what
    /// the next turn is integrated against (a gyro's bias).
    anchor: Vec<f32>,
    integral: Vec<f32>,
    moved: bool,
    window: VecDeque<(f64, Vec<f32>)>,
    last_t: f64,
    cap: MotionCapture,
    pub result: Option<Result<MotionResult, String>>,
}

impl Wizard {
    pub fn start(srcs: Vec<RawSrc>) -> Self {
        let n = srcs.len();
        Wizard {
            active: true,
            cap: MotionCapture { srcs: srcs.clone(), ..Default::default() },
            srcs,
            integral: vec![0.0; n],
            ..Default::default()
        }
    }

    pub fn srcs(&self) -> &[RawSrc] {
        &self.srcs
    }

    /// Advance on this frame's readings. Returns how long the current pose
    /// has been held, 0..1 of [`STILL_SECS`], for the progress bar.
    pub fn step(&mut self, t: f64, values: &[f32]) -> f32 {
        if self.result.is_some() || values.len() != self.srcs.len() {
            return 0.0;
        }
        let dt = if self.last_t == 0.0 { 0.0 } else { (t - self.last_t).clamp(0.0, 0.1) as f32 };
        if t == self.last_t {
            return self.held(t);
        }
        self.last_t = t;
        let turn = STEPS[self.step].1;
        if turn != Turn::None && !self.anchor.is_empty() {
            for (acc, (v, rest)) in self.integral.iter_mut().zip(values.iter().zip(&self.anchor)) {
                *acc += (v - rest) * dt;
            }
        }
        if self.step == 0 || self.anchor.is_empty() {
            self.moved = true; // the first pose needs no move to reach it
        } else if values.iter().zip(&self.anchor).any(|(v, a)| (v - a).abs() > MOVE_DELTA) {
            self.moved = true;
        }
        self.window.push_back((t, values.to_vec()));
        while self.window.front().is_some_and(|(t0, _)| t - t0 > STILL_SECS) {
            self.window.pop_front();
        }
        let held = self.held(t);
        if self.moved && held >= 1.0 {
            self.finish_step();
        }
        held
    }

    fn held(&self, t: f64) -> f32 {
        let Some((t0, _)) = self.window.front() else { return 0.0 };
        let n = self.srcs.len();
        let still = (0..n).all(|i| {
            let (lo, hi) = self.window.iter().fold((f32::INFINITY, f32::NEG_INFINITY),
                |(lo, hi), (_, v)| (lo.min(v[i]), hi.max(v[i])));
            hi - lo < STILL_RANGE
        });
        if !still || !self.moved {
            return 0.0;
        }
        (((t - t0) / (STILL_SECS * 0.95)) as f32).min(1.0)
    }

    fn finish_step(&mut self) {
        let n = self.srcs.len();
        let mean: Vec<f32> = (0..n)
            .map(|i| self.window.iter().map(|(_, v)| v[i]).sum::<f32>() / self.window.len() as f32)
            .collect();
        let integral = std::mem::replace(&mut self.integral, vec![0.0; n]);
        match self.step {
            0 => self.cap.flat = mean.clone(),
            1 => { self.cap.nose_up = mean.clone(); self.cap.pitch = integral; }
            2 => self.cap.flat_again = mean.clone(),
            3 => { self.cap.right_down = mean.clone(); self.cap.roll = integral; }
            4 => self.cap.flat_yaw = mean.clone(),
            _ => { self.cap.yaw = integral; }
        }
        self.anchor = mean;
        self.moved = false;
        self.window.clear();
        self.step += 1;
        if self.step == STEPS.len() {
            self.result = Some(solve_motion(&self.cap));
        }
    }

    /// The turn on the table only reaches a gyro: a pad with just an
    /// accelerometer never shows it moving.
    pub fn skip_turn(&mut self) {
        if self.step == STEPS.len() - 1 {
            self.cap.yaw = vec![0.0; self.srcs.len()];
            self.step += 1;
            self.result = Some(solve_motion(&self.cap));
        }
    }

    pub fn prompt(&self) -> (usize, &'static str, bool) {
        let i = self.step.min(STEPS.len() - 1);
        (i + 1, STEPS[i].0, STEPS[i].1 == Turn::Yaw)
    }
}

fn state_id(node: NodeId, dev_id: &str) -> (egui::Id, egui::Id) {
    (egui::Id::new(("raw_pad_analyzer", node.0, dev_id)), egui::Id::new(("raw_pad_motion", node.0, dev_id)))
}

/// Sample the pad into its analyzer and return the verdicts.
pub(crate) fn analyze(
    ctx: &egui::Context,
    node: NodeId,
    dev_id: &str,
    live: &std::collections::HashMap<(String, String), Signal>,
) -> Verdicts {
    let hid = raw_hid::hid_info(dev_id);
    let reports = hid.as_ref().and_then(|_| raw_hid::latest_reports(dev_id));
    let t = ctx.input(|i| i.time);
    let (aid, _) = state_id(node, dev_id);
    let a = ctx.data_mut(|d| {
        let a = d.get_temp_mut_or_default::<Analyzer>(aid);
        a.sample(t, live, dev_id, hid.as_deref(), reports.as_ref());
        a.v.clone()
    });
    // Keep frames coming until there is a verdict to show.
    if !a.ready() {
        request_repaint_throttled(ctx);
    }
    a
}

fn names(srcs: &[RawSrc], dev_id: &str) -> String {
    let hid = raw_hid::hid_info(dev_id);
    srcs.iter()
        .map(|s| hid.as_ref().and_then(|h| h.label_of(*s).map(str::to_string)).unwrap_or_else(|| s.label()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The motion row of a raw pad's header: what was spotted, the setup while it
/// runs, and its result. Returns the motion maps when the user accepts them.
pub(crate) fn show_motion_section(
    ui: &mut egui::Ui,
    node: NodeId,
    dev_id: &str,
    live: &std::collections::HashMap<(String, String), Signal>,
    analyzer: &Verdicts,
    profile: &RawPadProfile,
) -> Option<MotionResult> {
    let (_, wid) = state_id(node, dev_id);
    let mut wiz: Wizard = ui.ctx().data(|d| d.get_temp(wid)).unwrap_or_default();
    let hid = raw_hid::hid_info(dev_id);
    let mut accepted = None;

    if !wiz.active {
        let has_motion = profile.gyro.is_some() || profile.accel.is_some();
        let spotted = sensor_count(analyzer, hid.as_deref());
        ui.horizontal(|ui| {
            let label = if has_motion {
                let parts: Vec<&str> = [("gyro", profile.gyro.is_some()), ("accel", profile.accel.is_some())]
                    .into_iter().filter(|(_, on)| *on).map(|(n, _)| n).collect();
                egui::RichText::new(format!("Motion: {} ✓", parts.join(" + "))).small()
            } else if spotted >= 3 {
                egui::RichText::new(format!("Motion sensor spotted ({spotted} axes)"))
                    .small().color(Color32::from_rgb(127, 201, 127))
            } else if analyzer.ready() {
                egui::RichText::new("No motion sensor spotted").small().weak()
            } else {
                egui::RichText::new("Looking for a motion sensor…").small().weak()
            };
            ui.label(label);
            let button = if has_motion { "Redo setup" } else { "Set up motion" };
            let enabled = analyzer.ready();
            let resp = ui.add_enabled(enabled, egui::Button::new(egui::RichText::new(button).small()))
                .on_hover_text("Six quick poses tell FlexInput which axes are the gyro and the \
                                accelerometer, which way round they sit and how they scale. \
                                Works on any axis — including ones found inside the controller's \
                                vendor data.")
                .on_disabled_hover_text("Watching the controller for a moment first…");
            if resp.clicked() {
                let srcs = candidates(analyzer, live, dev_id, hid.as_deref(), profile);
                wiz = Wizard::start(srcs);
            }
        });
    }

    if wiz.active {
        request_repaint_throttled(ui.ctx());
        let reports = raw_hid::latest_reports(dev_id);
        let mut cancel = false;
        match wiz.result.clone() {
            None => {
                let t = ui.input(|i| i.time);
                let held = match read_values(wiz.srcs(), live, dev_id, reports.as_ref()) {
                    Some(v) => wiz.step(t, &v),
                    None => 0.0,
                };
                let (n, text, is_turn) = wiz.prompt();
                ui.label(egui::RichText::new(format!("Motion setup {n}/6 — {text}"))
                    .small().strong().color(Color32::from_rgb(106, 167, 255)));
                ui.horizontal(|ui| {
                    ui.add(egui::ProgressBar::new(held).desired_width(120.0).desired_height(6.0));
                    if is_turn && ui.small_button("Skip").on_hover_text(
                        "No gyro? The turn on the table only shows up on one.").clicked()
                    {
                        wiz.skip_turn();
                    }
                    if ui.small_button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                if wiz.srcs().len() < 3 {
                    ui.label(egui::RichText::new("This controller shows fewer than three free axes — \
                                                  there is nothing to set up.").small().weak());
                }
            }
            Some(Ok(result)) => {
                let describe = |m: &Option<flexinput_core::raw_pad::ImuMap>, what: &str| {
                    m.as_ref().map(|m| format!("{what} on {}", names(&m.src, dev_id)))
                };
                let found: Vec<String> = [describe(&result.accel, "Accelerometer"), describe(&result.gyro, "Gyro")]
                    .into_iter().flatten().collect();
                ui.label(egui::RichText::new(found.join(" · ")).small().color(Color32::from_rgb(127, 201, 127)));
                ui.horizontal(|ui| {
                    if ui.small_button("Use it").clicked() {
                        accepted = Some(result.clone());
                        cancel = true;
                    }
                    if ui.small_button("Redo").clicked() {
                        let srcs = wiz.srcs().to_vec();
                        wiz = Wizard::start(srcs);
                    }
                    if ui.small_button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            }
            Some(Err(why)) => {
                ui.label(egui::RichText::new(why).small().color(Color32::from_rgb(232, 180, 65)));
                ui.horizontal(|ui| {
                    if ui.small_button("Try again").clicked() {
                        let srcs = wiz.srcs().to_vec();
                        wiz = Wizard::start(srcs);
                    }
                    if ui.small_button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            }
        }
        if cancel {
            wiz = Wizard::default();
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(wid, wiz));
    accepted
}

/// Whether motion setup is running on this node (Learn stays off meanwhile).
pub(crate) fn motion_setup_active(ctx: &egui::Context, node: NodeId, dev_id: &str) -> bool {
    let (_, wid) = state_id(node, dev_id);
    ctx.data(|d| d.get_temp::<Wizard>(wid)).is_some_and(|w| w.active)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic, symmetric jitter through many values, like a resting
    /// sensor's noise.
    fn noise(n: usize, center: f32, amp: f32) -> Vec<f32> {
        let mut x: u32 = 12345;
        (0..n).map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            center + amp * (((x >> 16) % 2001) as f32 / 1000.0 - 1.0)
        }).collect()
    }

    #[test]
    fn a_resting_sensor_jitters_a_stick_does_not_and_a_counter_climbs() {
        assert_eq!(characterize(&noise(90, 0.25, 0.002)), Some(Character::Sensor));
        assert_eq!(characterize(&vec![0.0; 90]), None, "a stick at rest");
        // A stick swept by hand: big steps.
        let swept: Vec<f32> = (0..90).map(|i| ((i as f32) / 10.0).sin()).collect();
        assert_eq!(characterize(&swept), None);
        // An 8-bit stick dithering between two values is not a sensor.
        let dither: Vec<f32> = (0..90).map(|i| if i % 2 == 0 { 0.0 } else { 0.0078 }).collect();
        assert_eq!(characterize(&dither), None);
        // A timestamp, wrapping once.
        let ts: Vec<f32> = (0..90).map(|i| ((i as f32 * 0.03 + 0.9) % 2.0) - 1.0).collect();
        assert_eq!(characterize(&ts), Some(Character::Counter));
        assert_eq!(characterize(&noise(30, 0.0, 0.01)), None, "too few frames to judge");
    }

    #[test]
    fn blob_words_find_the_sensor_at_the_right_alignment() {
        // A blob: [junk, gx lo, gx hi, ax lo, ax hi, flag] — gyro near zero
        // (its bytes flip 00/FF), accel near 0x1000.
        let frames: VecDeque<Vec<u8>> = (0..90).map(|i| {
            let gx: i16 = ((i * 7919) % 7) as i16 - 3;
            let ax: i16 = 0x1000 + ((i * 104729) % 9) as i16 - 4;
            let [g0, g1] = gx.to_le_bytes();
            let [a0, a1] = ax.to_le_bytes();
            vec![0x55, g0, g1, a0, a1, 0]
        }).collect();
        let (words, counters) = blob_words(&frames);
        assert_eq!(words, vec![1, 3]);
        assert!(counters.is_empty());
    }

    #[test]
    fn the_wizard_records_poses_only_once_held_still_after_a_move() {
        let srcs = vec![RawSrc::Axis(0), RawSrc::Axis(1), RawSrc::Axis(2)];
        let mut w = Wizard::start(srcs);
        let mut t = 1.0;
        let mut feed = |w: &mut Wizard, v: [f32; 3], secs: f64| {
            let end = t + secs;
            while t < end {
                t += 1.0 / 60.0;
                w.step(t, &v);
            }
        };
        feed(&mut w, [0.0, 0.0, 0.25], 1.0);
        assert_eq!(w.step, 1, "flat recorded");
        feed(&mut w, [0.0, 0.0, 0.25], 2.0);
        assert_eq!(w.step, 1, "still flat: the nose-up pose needs a move first");
        feed(&mut w, [-0.25, 0.0, 0.0], 1.0);
        assert_eq!(w.step, 2);
        assert_eq!(w.cap.nose_up, vec![-0.25, 0.0, 0.0]);
    }
}
