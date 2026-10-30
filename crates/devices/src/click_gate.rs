//! Holding the IMU through the jolt of a button click.
//!
//! A microswitch soldered to the same board as the IMU knocks the sensor every
//! time it snaps, on press and on release. The result looks like a flick, but
//! rings for several reports with a sign that flips, so [`crate::spike`] can't
//! take it: a sample's neighbours are as far off as it is, and a window wide
//! enough to outlast the jolt would delay every sample by half of it.
//!
//! Unlike a spike, a click announces itself. So this doesn't judge the signal
//! at all: an edge on a button picked for it holds the six IMU axes at their
//! value from just before, for as long as that button's jolt lasts, at no cost
//! to any sample outside it. The hold starts sharply, to catch the jolt's first
//! sample, then slopes back to the live signal: that fades out whatever is left
//! of the jolt, and meets a pad that turned meanwhile without a step.
//!
//! # Where it runs
//!
//! On the I/O thread's merged poll ([`ClickGates::apply`]), after every backend,
//! because that is the one place every backend's buttons and motion meet, by
//! pin name, whatever the connection. Times are wall clock for the same reason:
//! the pins repeat each report for as many polls as fit, so a count of samples
//! means nothing here.
//!
//! # Look-ahead
//!
//! A pad can report the jolt before the click (a debounced button is late), and
//! samples already published can't be taken back. Look-ahead delays the whole
//! IMU stream to leave room to hold those too. It is off unless asked for and
//! kept short: it costs its full length on every sample, all the time, and past
//! a few ms that hurts more than a jolt's first sample does.
//!
//! # Measuring
//!
//! [`JoltAnalyzer`] names the buttons that jolt and how long for, from what the
//! pins carry before the gate ([`measure_start`]). Each click is judged against
//! a rolling median as wide as the longest jolt worth holding, so a hand moving
//! the pad around it doesn't count and a click doesn't need a still table.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use flexinput_core::Signal;

const IMU_PINS: [&str; 6] = ["gyro_x", "gyro_y", "gyro_z", "accel_x", "accel_y", "accel_z"];

/// Longest hold a measurement proposes, and the most a user can set.
pub const MAX_HOLD_MS: f32 = 120.0;
/// Most look-ahead a user can set, or a measurement suggests.
pub const MAX_LOOKAHEAD_MS: f32 = 5.0;
/// Longest slope back to the live signal a user can set.
pub const MAX_RELEASE_MS: f32 = 100.0;
/// That slope, until set.
pub const DEFAULT_RELEASE_MS: f32 = 10.0;
/// Hold for a button picked by hand, before it has been measured.
pub const DEFAULT_HOLD_MS: f32 = 30.0;

/// The value to hold is the median of this much signal before the hold, so
/// that the start of a jolt already published doesn't get held for its length.
const REFERENCE: Duration = Duration::from_millis(30);

/// One pad's settings. Compared by value, so the UI only replaces it on a change.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClickGateConfig {
    pub enabled: bool,
    /// The buttons that jolt the sensor, each with how long after its edges
    /// the IMU is held, in ms.
    pub buttons: Vec<(String, f32)>,
    /// How far the IMU stream is delayed, to hold what comes before an edge.
    pub lookahead_ms: f32,
    /// How long a hold takes to slope back to the live signal, once it ends.
    pub release_ms: f32,
}

/// Per-device settings, shared with the I/O thread.
pub type ClickGateSettings = Arc<RwLock<HashMap<String, Arc<ClickGateConfig>>>>;

/// A running hold: `value` replaces the stream from `start` until `end`, then
/// slopes back to it.
#[derive(Debug, Clone, Copy)]
struct Hold {
    start: Instant,
    end: Instant,
    value: [f32; 6],
}

#[derive(Debug)]
struct Gate {
    cfg: Arc<ClickGateConfig>,
    lookahead: Duration,
    release: Duration,
    button_keys: Vec<(String, String)>,
    imu_keys: [(String, String); 6],
    /// Last state of each button; `None` until first seen, so that connecting
    /// with one held down isn't an edge.
    pressed: Vec<Option<bool>>,
    /// The last value of each axis: a poll can leave some out.
    latest: [f32; 6],
    /// Input, newest at the back, far enough back for look-ahead + [`REFERENCE`].
    history: VecDeque<(Instant, [f32; 6])>,
    hold: Option<Hold>,
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    match n {
        0 => 0.0,
        _ if n % 2 == 1 => v[n / 2],
        _ => 0.5 * (v[n / 2 - 1] + v[n / 2]),
    }
}

fn pin_key(dev: &str, pin: &str) -> (String, String) {
    (dev.to_string(), pin.to_string())
}

impl Gate {
    fn new(dev: &str, cfg: Arc<ClickGateConfig>) -> Self {
        Self {
            lookahead: Duration::from_secs_f32(cfg.lookahead_ms.clamp(0.0, MAX_LOOKAHEAD_MS) / 1000.0),
            release: Duration::from_secs_f32(cfg.release_ms.clamp(0.0, MAX_RELEASE_MS) / 1000.0),
            button_keys: cfg.buttons.iter().map(|(p, _)| pin_key(dev, p)).collect(),
            imu_keys: IMU_PINS.map(|p| pin_key(dev, p)),
            pressed: vec![None; cfg.buttons.len()],
            latest: [0.0; 6],
            history: VecDeque::new(),
            hold: None,
            cfg,
        }
    }

    /// The value to hold through a hold starting at `start`.
    fn reference(&self, start: Instant) -> [f32; 6] {
        let from = start.checked_sub(REFERENCE).unwrap_or(start);
        let span: Vec<&[f32; 6]> = self.history.iter()
            .filter(|(t, _)| *t >= from && *t < start)
            .map(|(_, s)| s)
            .collect();
        if span.is_empty() {
            return self.history.front().map_or(self.latest, |(_, s)| *s);
        }
        let mut out = [0.0; 6];
        let mut axis = Vec::with_capacity(span.len());
        for (a, o) in out.iter_mut().enumerate() {
            axis.clear();
            axis.extend(span.iter().map(|s| s[a]));
            *o = median(&mut axis);
        }
        out
    }

    /// An edge of a button whose jolt lasts `hold`.
    fn begin_hold(&mut self, now: Instant, hold: Duration) {
        let start = now.checked_sub(self.lookahead).unwrap_or(now);
        let end = now + hold;
        match &mut self.hold {
            // Still running at the start of this one, slope included: one long
            // hold, at the value from before the first.
            Some(h) if start <= h.end + self.release => h.end = h.end.max(end),
            _ => {
                let value = self.reference(start);
                self.hold = Some(Hold { start, end, value });
            }
        }
    }

    fn step(&mut self, signals: &mut HashMap<(String, String), Signal>, now: Instant) {
        let mut present = [false; 6];
        for (a, key) in self.imu_keys.iter().enumerate() {
            if let Some(Signal::Float(v)) = signals.get(key) {
                self.latest[a] = *v;
                present[a] = true;
            }
        }
        for i in 0..self.button_keys.len() {
            let Some(Signal::Bool(b)) = signals.get(&self.button_keys[i]) else { continue };
            let b = *b;
            if self.pressed[i].is_some_and(|was| was != b) {
                let ms = self.cfg.buttons[i].1.clamp(0.0, MAX_HOLD_MS);
                self.begin_hold(now, Duration::from_secs_f32(ms / 1000.0));
            }
            self.pressed[i] = Some(b);
        }
        self.history.push_back((now, self.latest));
        let keep = self.lookahead + REFERENCE + Duration::from_millis(5);
        if let Some(oldest) = now.checked_sub(keep) {
            // One older than the span stays: on a slow stream it's what the
            // delayed read below finds.
            while self.history.len() > 1 && self.history[1].0 < oldest {
                self.history.pop_front();
            }
        }

        let emit_at = now.checked_sub(self.lookahead).unwrap_or(now);
        let sample = self.history.iter().rev()
            .find(|(t, _)| *t <= emit_at)
            .or(self.history.front())
            .map_or(self.latest, |(_, s)| *s);
        let out = match self.hold {
            Some(h) if emit_at >= h.end + self.release => {
                self.hold = None;
                sample
            }
            Some(h) if emit_at >= h.end => {
                let t = (emit_at - h.end).as_secs_f32() / self.release.as_secs_f32();
                std::array::from_fn(|a| h.value[a] + (sample[a] - h.value[a]) * t)
            }
            Some(h) if emit_at >= h.start => h.value,
            _ => sample,
        };
        for (a, key) in self.imu_keys.iter().enumerate() {
            if present[a] {
                signals.insert(key.clone(), Signal::Float(out[a]));
            }
        }
    }
}

/// Every pad's gate, on the I/O thread.
#[derive(Debug, Default)]
pub struct ClickGates {
    gates: HashMap<String, Gate>,
}

impl ClickGates {
    /// Run this poll's signals through the gates, after recording them for
    /// whichever pads are being measured.
    pub fn apply(
        &mut self,
        settings: &HashMap<String, Arc<ClickGateConfig>>,
        signals: &mut HashMap<(String, String), Signal>,
        now: Instant,
    ) {
        record_measures(signals, now);
        if settings.is_empty() && self.gates.is_empty() {
            return;
        }
        self.gates.retain(|dev, g| settings.get(dev).is_some_and(|c| Arc::ptr_eq(c, &g.cfg)));
        for (dev, cfg) in settings {
            if !cfg.enabled || cfg.buttons.is_empty() {
                continue;
            }
            self.gates.entry(dev.clone())
                .or_insert_with(|| Gate::new(dev, Arc::clone(cfg)))
                .step(signals, now);
        }
    }
}

// ── Measuring ────────────────────────────────────────────────────────────────

/// One poll of a pad being measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeasureSample {
    pub at: Instant,
    /// Normalized gyro, as it reaches the gate.
    pub gyro: [f32; 3],
    /// A bit per button, in the order given to [`measure_start`].
    pub mask: u64,
}

/// Most samples held between reads: a couple of seconds at the fastest polling.
const MEASURE_MAX: usize = 8192;
/// A measurement nobody has read for this long has been left behind.
const MEASURE_IDLE: Duration = Duration::from_secs(3);

struct Measure {
    button_keys: Vec<(String, String)>,
    gyro_keys: [(String, String); 3],
    mask: u64,
    samples: VecDeque<MeasureSample>,
    read_at: Instant,
}

/// Measurements running — a poll pays one relaxed load while there are none.
static MEASURING: AtomicUsize = AtomicUsize::new(0);

fn measures() -> &'static Mutex<HashMap<String, Measure>> {
    static MEASURES: OnceLock<Mutex<HashMap<String, Measure>>> = OnceLock::new();
    MEASURES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Start recording `dev`'s gyro and `buttons` (64 at most), replacing any
/// recording already running.
pub fn measure_start(dev: &str, buttons: &[String]) {
    let Ok(mut map) = measures().lock() else { return };
    let m = Measure {
        button_keys: buttons.iter().take(64).map(|p| pin_key(dev, p)).collect(),
        gyro_keys: [pin_key(dev, "gyro_x"), pin_key(dev, "gyro_y"), pin_key(dev, "gyro_z")],
        mask: 0,
        samples: VecDeque::new(),
        read_at: Instant::now(),
    };
    if map.insert(dev.to_string(), m).is_none() {
        MEASURING.fetch_add(1, Ordering::Relaxed);
    }
}

/// Stop recording `dev`.
pub fn measure_stop(dev: &str) {
    let Ok(mut map) = measures().lock() else { return };
    if map.remove(dev).is_some() {
        MEASURING.fetch_sub(1, Ordering::Relaxed);
    }
}

/// What has been recorded for `dev` since the last read; `None` when it isn't
/// being recorded. Reading keeps it going: one left unread stops by itself.
pub fn measure_read(dev: &str) -> Option<Vec<MeasureSample>> {
    let mut map = measures().lock().ok()?;
    let m = map.get_mut(dev)?;
    m.read_at = Instant::now();
    Some(m.samples.drain(..).collect())
}

fn record_measures(signals: &HashMap<(String, String), Signal>, now: Instant) {
    if MEASURING.load(Ordering::Relaxed) == 0 {
        return;
    }
    let Ok(mut map) = measures().lock() else { return };
    let before = map.len();
    map.retain(|_, m| now.saturating_duration_since(m.read_at) < MEASURE_IDLE);
    MEASURING.fetch_sub(before - map.len(), Ordering::Relaxed);
    for m in map.values_mut() {
        for (i, key) in m.button_keys.iter().enumerate() {
            if let Some(Signal::Bool(b)) = signals.get(key) {
                m.mask = (m.mask & !(1 << i)) | ((*b as u64) << i);
            }
        }
        let mut gyro = [0.0; 3];
        let mut any = false;
        for (a, key) in m.gyro_keys.iter().enumerate() {
            if let Some(Signal::Float(v)) = signals.get(key) {
                gyro[a] = *v;
                any = true;
            }
        }
        if any {
            m.samples.push_back(MeasureSample { at: now, gyro, mask: m.mask });
            if m.samples.len() > MEASURE_MAX {
                m.samples.pop_front();
            }
        }
    }
}

/// Half-width of the rolling median a click is judged against. A jolt lasting
/// up to this long stands clear of it; a hand moving the pad, slower than this,
/// is followed by it.
const BASELINE_R: Duration = Duration::from_millis(75);
/// How far before an edge a jolt is looked for...
const BEFORE: Duration = Duration::from_millis(60);
/// ...and after it. Long enough for a jolt, short enough to fit between the
/// press and the release of a quick tap.
const AFTER: Duration = Duration::from_millis(85);
/// The stretch before all that the noise is measured from.
const QUIET: Duration = Duration::from_millis(40);
/// A jolt is this far past the noise, in robust standard deviations...
const JOLT_SIGMA: f32 = 6.0;
/// ...and never less than this, normalized: 4 °/s (±2000 °/s full scale).
const MIN_JOLT: f32 = 0.002;
/// Added to that, as a share of how fast the pad is turning: the median can't
/// quite follow a hand round the top of a swing.
const MOTION_SHARE: f32 = 0.1;
/// Quiet gaps up to this long are part of the same jolt: it rings.
const JOLT_GAP_MS: f32 = 15.0;
/// Anything longer than this was the pad moving, whatever the click did.
const MAX_JOLT_MS: f32 = 100.0;
/// Added to a measured hold, for a jolt a little longer than any measured.
const HOLD_MARGIN_MS: f32 = 3.0;
/// Clicks with a jolt it takes to call a button a jolting one.
const MIN_JOLTS: u32 = 2;

#[derive(Debug, Clone, Default)]
struct Tally {
    judged: u32,
    /// The largest distance from the baseline around any click, jolt or not.
    largest: f32,
    peak: f32,
    /// Per jolt, ms from its first sample to the sample after its last.
    lengths: Vec<f32>,
    /// Per jolt, ms after the edge its last sample was replaced.
    ends: Vec<f32>,
    /// Per jolt, ms before the edge its first sample arrived. Read as a median:
    /// a hand moving just before one click shouldn't set what every click costs.
    leads: Vec<f32>,
}

/// What clicking a button does to the gyro, so far.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ButtonJolt {
    /// Clicks judged (presses and releases).
    pub clicks: u32,
    /// Those that jolted.
    pub jolts: u32,
    /// The largest movement seen around a click, jolt or not, normalized.
    pub largest: f32,
    /// The biggest jolt, normalized.
    pub peak: f32,
    /// How long a jolt lasts, ms.
    pub length_ms: f32,
    /// How long after an edge to hold for, ms.
    pub hold_ms: f32,
    /// How long before an edge a jolt typically starts, ms.
    pub lead_ms: f32,
}

impl ButtonJolt {
    /// Clicking this button jolts the gyro: seen more than once, on at least
    /// half its clicks (a press and a release can differ).
    pub fn jolting(&self) -> bool {
        self.jolts >= MIN_JOLTS && self.jolts * 2 >= self.clicks
    }
}

/// Finds the buttons that jolt, from a [`measure_read`] stream.
#[derive(Debug, Clone)]
pub struct JoltAnalyzer {
    samples: VecDeque<MeasureSample>,
    /// Edges not yet judged, with their button.
    edges: VecDeque<(Instant, usize)>,
    prev_edge: Option<Instant>,
    mask: Option<u64>,
    tallies: Vec<Tally>,
}

/// 90th percentile: the largest, until there are enough to spare one.
fn high(v: &[f32]) -> f32 {
    let mut v = v.to_vec();
    v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    match v.len() {
        0 => 0.0,
        n => v[(n * 9).div_ceil(10) - 1],
    }
}

impl JoltAnalyzer {
    pub fn new(buttons: usize) -> Self {
        Self {
            samples: VecDeque::new(),
            edges: VecDeque::new(),
            prev_edge: None,
            mask: None,
            tallies: vec![Tally::default(); buttons.min(64)],
        }
    }

    /// Forget what has been measured.
    pub fn reset(&mut self) {
        *self = Self::new(self.tallies.len());
    }

    /// Is button `i` down, as of the last sample?
    pub fn pressed(&self, i: usize) -> bool {
        self.mask.is_some_and(|m| m & (1 << i) != 0)
    }

    pub fn report(&self, i: usize) -> ButtonJolt {
        let Some(t) = self.tallies.get(i) else { return ButtonJolt::default() };
        ButtonJolt {
            clicks: t.judged,
            jolts: t.ends.len() as u32,
            largest: t.largest,
            peak: t.peak,
            length_ms: high(&t.lengths),
            hold_ms: if t.ends.is_empty() { 0.0 } else { (high(&t.ends) + HOLD_MARGIN_MS).min(MAX_HOLD_MS) },
            lead_ms: median(&mut t.leads.clone()),
        }
    }

    pub fn push(&mut self, s: MeasureSample) {
        if let Some(was) = self.mask {
            let changed = was ^ s.mask;
            for i in 0..self.tallies.len() {
                if changed & (1 << i) != 0 {
                    self.edges.push_back((s.at, i));
                }
            }
        }
        self.mask = Some(s.mask);
        // A pin repeats a report for as many polls as fit: only a new reading
        // is a sample, at the time it arrived.
        if self.samples.back().is_none_or(|b| b.gyro != s.gyro) {
            self.samples.push_back(s);
        }
        let judge_before = s.at.checked_sub(AFTER + BASELINE_R);
        while let Some(&(at, i)) = self.edges.front() {
            if judge_before.is_none_or(|t| at > t) {
                break;
            }
            self.edges.pop_front();
            self.judge(at, i);
            self.prev_edge = Some(at);
        }
        let from = self.edges.front().map_or(s.at, |(at, _)| *at);
        if let Some(keep) = from.checked_sub(QUIET + BEFORE + BASELINE_R) {
            while self.samples.front().is_some_and(|f| f.at < keep) {
                self.samples.pop_front();
            }
        }
    }

    /// Did the click at `at` jolt? Left out if another click is too close.
    fn judge(&mut self, at: Instant, i: usize) {
        if self.prev_edge.is_some_and(|p| at.saturating_duration_since(p) < BEFORE)
            || self.edges.front().is_some_and(|(next, _)| next.saturating_duration_since(at) < AFTER)
        {
            return;
        }
        // Into ms from the edge: signed, and easier to read than Instants.
        let ms = |t: Instant| {
            if t >= at { (t - at).as_secs_f32() * 1000.0 } else { -((at - t).as_secs_f32() * 1000.0) }
        };
        let times: Vec<f32> = self.samples.iter().map(|s| ms(s.at)).collect();
        let r_ms = BASELINE_R.as_secs_f32() * 1000.0;
        let before_ms = BEFORE.as_secs_f32() * 1000.0;
        let after_ms = AFTER.as_secs_f32() * 1000.0;
        let quiet_ms = QUIET.as_secs_f32() * 1000.0;

        // Per sample in reach: its largest distance from the rolling median.
        let mut window = Vec::new();
        let mut reach: Vec<(f32, f32)> = Vec::new();
        let mut speed = 0.0f32;
        for (k, s) in self.samples.iter().enumerate() {
            let t = times[k];
            if t < -before_ms - quiet_ms || t > after_ms {
                continue;
            }
            let mut largest = 0.0f32;
            for a in 0..3 {
                window.clear();
                window.extend(self.samples.iter().zip(&times)
                    .filter(|(_, u)| (**u - t).abs() <= r_ms)
                    .map(|(o, _)| o.gyro[a]));
                let base = median(&mut window);
                speed = speed.max(base.abs());
                largest = largest.max((s.gyro[a] - base).abs());
            }
            reach.push((t, largest));
        }
        // The noise is whatever comes before the jolt can.
        let mut quiet: Vec<f32> = reach.iter().filter(|(t, _)| *t < -before_ms).map(|(_, r)| *r).collect();
        if quiet.len() < 3 || reach.len() - quiet.len() < 3 {
            return; // too little signal around it to say
        }
        let thresh = (JOLT_SIGMA * 1.4826 * median(&mut quiet)).max(MIN_JOLT) + MOTION_SHARE * speed;

        let tally = &mut self.tallies[i];
        tally.judged += 1;
        let span = &reach[reach.iter().position(|(t, _)| *t >= -before_ms).unwrap_or(0)..];
        tally.largest = span.iter().fold(tally.largest, |m, (_, r)| m.max(*r));
        let Some(top) = span.iter().enumerate()
            .max_by(|a, b| a.1.1.partial_cmp(&b.1.1).unwrap_or(std::cmp::Ordering::Equal))
            .filter(|(_, (_, r))| *r > thresh)
            .map(|(k, _)| k)
        else {
            return;
        };
        // Out from the peak, over the gaps it rings across.
        let (mut first, mut last) = (top, top);
        for k in (0..top).rev() {
            if span[first].0 - span[k].0 > JOLT_GAP_MS { break; }
            if span[k].1 > thresh { first = k; }
        }
        for k in top + 1..span.len() {
            if span[k].0 - span[last].0 > JOLT_GAP_MS { break; }
            if span[k].1 > thresh { last = k; }
        }
        let start = span[first].0;
        // The pin keeps the last jolted reading until the next one arrives.
        let end = span.get(last + 1).map_or(span[last].0, |(t, _)| *t);
        if end - start > MAX_JOLT_MS {
            return;
        }
        tally.peak = tally.peak.max(span[top].1);
        tally.lengths.push(end - start);
        tally.ends.push(end.max(0.0));
        tally.leads.push((-start).max(0.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV: &str = "test:pad";
    const BTN: &str = "btn_rb";

    fn signals(gyro: f32, pressed: bool) -> HashMap<(String, String), Signal> {
        let mut m = HashMap::new();
        for p in IMU_PINS {
            m.insert(pin_key(DEV, p), Signal::Float(gyro));
        }
        m.insert(pin_key(DEV, BTN), Signal::Bool(pressed));
        m
    }

    fn cfg(hold_ms: f32, lookahead_ms: f32) -> HashMap<String, Arc<ClickGateConfig>> {
        let c = ClickGateConfig { enabled: true, buttons: vec![(BTN.into(), hold_ms)], lookahead_ms, release_ms: 0.0 };
        HashMap::from([(DEV.to_string(), Arc::new(c))])
    }

    fn gyro_out(s: &HashMap<(String, String), Signal>) -> f32 {
        match s.get(&pin_key(DEV, "gyro_x")) {
            Some(Signal::Float(v)) => *v,
            other => panic!("{other:?}"),
        }
    }

    /// Run `(gyro, pressed)` per ms through the gates; gyro out per ms.
    fn run(settings: &HashMap<String, Arc<ClickGateConfig>>, input: &[(f32, bool)]) -> Vec<f32> {
        let mut g = ClickGates::default();
        let t0 = Instant::now();
        input.iter().enumerate().map(|(i, (v, b))| {
            let mut s = signals(*v, *b);
            g.apply(settings, &mut s, t0 + Duration::from_millis(i as u64));
            gyro_out(&s)
        }).collect()
    }

    /// 100 ms at rest, then a press at 100 with a jolt from `jolt_at` for 20 ms.
    fn clicked(jolt_at: usize) -> Vec<(f32, bool)> {
        (0..200).map(|i| {
            let v = if (jolt_at..jolt_at + 20).contains(&i) { 0.5 } else { 0.01 };
            (v, i >= 100)
        }).collect()
    }

    #[test]
    fn an_edge_holds_the_jolt_out_and_nothing_else() {
        let out = run(&cfg(25.0, 0.0), &clicked(100));
        assert!(out[100..125].iter().all(|v| (*v - 0.01).abs() < 1e-6), "{:?}", &out[100..125]);
        assert_eq!(out[130], 0.01);
    }

    #[test]
    fn the_release_holds_too() {
        let mut input = clicked(100);
        for (i, s) in input.iter_mut().enumerate() {
            s.1 = (100..150).contains(&i);
            if (150..160).contains(&i) { s.0 = -0.5; }
        }
        let out = run(&cfg(25.0, 0.0), &input);
        assert!(out[150..165].iter().all(|v| v.abs() < 0.02), "{:?}", &out[150..165]);
    }

    #[test]
    fn motion_outside_a_hold_is_untouched_and_undelayed() {
        let input: Vec<(f32, bool)> = (0..200).map(|i| (i as f32 * 1e-3, i >= 100)).collect();
        let out = run(&cfg(10.0, 0.0), &input);
        for i in (0..100).chain(115..200) {
            assert_eq!(out[i], input[i].0, "sample {i}");
        }
    }

    #[test]
    fn a_hold_during_motion_keeps_about_the_speed_it_had() {
        let input: Vec<(f32, bool)> = (0..200).map(|i| (i as f32 * 1e-3, i >= 100)).collect();
        let out = run(&cfg(10.0, 0.0), &input);
        // The median of the last 30 ms: 15 samples back, at this slope.
        assert!((out[105] - 0.085).abs() < 2e-3, "{}", out[105]);
    }

    #[test]
    fn a_jolt_already_starting_at_the_edge_is_not_what_gets_held() {
        // Published before the edge, so it can't be removed without look-ahead,
        // but it must not be frozen on the output either.
        let out = run(&cfg(25.0, 0.0), &clicked(95));
        assert_eq!(out[97], 0.5, "already out");
        assert!(out[100..115].iter().all(|v| (*v - 0.01).abs() < 1e-6), "{:?}", &out[100..115]);
    }

    #[test]
    fn lookahead_holds_what_comes_before_the_edge_for_its_delay() {
        let out = run(&cfg(25.0, 5.0), &clicked(97));
        assert!(out.iter().all(|v| (*v - 0.01).abs() < 1e-6), "{out:?}");
        let ramp: Vec<(f32, bool)> = (0..100).map(|i| (i as f32 * 1e-3, false)).collect();
        let out = run(&cfg(25.0, 5.0), &ramp);
        assert_eq!(out[50], ramp[45].0, "delayed by look-ahead");
    }

    /// A pad that turned during the hold, stepping up as it was pressed.
    fn stepped(pressed: impl Fn(usize) -> bool) -> Vec<(f32, bool)> {
        (0..200).map(|i| (if i >= 100 { 1.0 } else { 0.0 }, pressed(i))).collect()
    }

    fn with_release(hold_ms: f32, release_ms: f32) -> HashMap<String, Arc<ClickGateConfig>> {
        let mut s = cfg(hold_ms, 0.0);
        Arc::make_mut(s.get_mut(DEV).unwrap()).release_ms = release_ms;
        s
    }

    #[test]
    fn a_hold_slopes_back_to_the_live_signal() {
        let out = run(&with_release(10.0, 10.0), &stepped(|i| i >= 100));
        assert_eq!(out[99], 0.0);
        assert_eq!(out[100], 0.0, "sharp: the first sample is already held");
        assert!(out[108] < 1e-6);
        for (i, want) in [(112, 0.2), (115, 0.5), (118, 0.8)] {
            assert!((out[i] - want).abs() < 0.02, "at {i}: {}", out[i]);
        }
        assert_eq!(out[121], 1.0, "and then it is live");
    }

    #[test]
    fn no_release_steps_straight_back() {
        let out = run(&with_release(10.0, 0.0), &stepped(|i| i >= 100));
        assert_eq!(out[108], 0.0);
        assert_eq!(out[111], 1.0);
    }

    #[test]
    fn a_click_during_the_slope_goes_back_to_holding() {
        // Hold to 110, slope to 120, released at 115.
        let out = run(&with_release(10.0, 10.0), &stepped(|i| (100..115).contains(&i)));
        assert!(out[114] > 0.3, "sloping: {}", out[114]);
        assert_eq!(out[116], 0.0, "held again, at the value from before the first");
        assert!(out[130] > 0.3, "and sloping from the second's end: {}", out[130]);
    }

    #[test]
    fn lookahead_past_its_limit_is_held_to_it() {
        let ramp: Vec<(f32, bool)> = (0..100).map(|i| (i as f32 * 1e-3, false)).collect();
        let out = run(&cfg(25.0, 40.0), &ramp);
        assert_eq!(out[50], ramp[45].0, "a setting saved when it could be longer");
    }

    #[test]
    fn clicks_closer_than_a_hold_make_one_long_hold() {
        let input: Vec<(f32, bool)> = (0..200).map(|i| {
            (if (100..150).contains(&i) { 0.5 } else { 0.01 }, (100..120).contains(&i))
        }).collect();
        let out = run(&cfg(30.0, 0.0), &input);
        assert!(out[100..150].iter().all(|v| (*v - 0.01).abs() < 1e-6), "{:?}", &out[100..150]);
    }

    #[test]
    fn off_or_with_no_buttons_it_changes_nothing() {
        let input = clicked(100);
        let raw: Vec<f32> = input.iter().map(|s| s.0).collect();
        let mut off = cfg(25.0, 10.0);
        Arc::make_mut(off.get_mut(DEV).unwrap()).enabled = false;
        assert_eq!(run(&off, &input), raw);
        let mut none = cfg(25.0, 10.0);
        Arc::make_mut(none.get_mut(DEV).unwrap()).buttons.clear();
        assert_eq!(run(&none, &input), raw);
        assert_eq!(run(&HashMap::new(), &input), raw);
    }

    #[test]
    fn connecting_with_a_button_down_is_not_an_edge() {
        let input: Vec<(f32, bool)> = (0..50).map(|i| (i as f32, true)).collect();
        let out = run(&cfg(25.0, 0.0), &input);
        assert_eq!(out[10], 10.0);
    }

    #[test]
    fn a_pad_that_leaves_a_pin_out_of_a_poll_keeps_going() {
        let mut g = ClickGates::default();
        let settings = cfg(25.0, 0.0);
        let t0 = Instant::now();
        let mut s = signals(0.1, false);
        g.apply(&settings, &mut s, t0);
        s.remove(&pin_key(DEV, BTN));
        s.remove(&pin_key(DEV, "accel_x"));
        g.apply(&settings, &mut s, t0 + Duration::from_millis(1));
        assert!(!s.contains_key(&pin_key(DEV, "accel_x")), "not invented");
        assert_eq!(gyro_out(&s), 0.1);
    }

    #[test]
    fn a_new_setting_replaces_the_gate() {
        let mut g = ClickGates::default();
        let t0 = Instant::now();
        let mut s = signals(0.1, false);
        g.apply(&cfg(25.0, 0.0), &mut s, t0);
        let new = cfg(1.0, 0.0);
        g.apply(&new, &mut s, t0);
        assert!(Arc::ptr_eq(&g.gates[DEV].cfg, &new[DEV]));
        g.apply(&HashMap::new(), &mut s, t0);
        assert!(g.gates.is_empty());
    }

    // ── Measuring ────────────────────────────────────────────────────────────

    /// Deterministic, roughly normal noise (σ ≈ 1).
    fn gauss(i: usize) -> f32 {
        let mut x = (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let mut sum = 0.0;
        for _ in 0..4 {
            x ^= x >> 33;
            x = x.wrapping_mul(0xff51_afd7_ed55_8ccd).wrapping_add(0x1234_5678);
            x ^= x >> 29;
            sum += (x >> 11) as f32 / (1u64 << 53) as f32;
        }
        (sum - 2.0) * 3.0f32.sqrt()
    }

    /// A stream as the pins carry it: a new report every 5 ms, each repeated
    /// by 1 kHz polling. `level(ms)` is the gyro, `mask(ms)` the buttons; the
    /// reports carry noise of 0.5 °/s.
    fn measure(ms: usize, level: impl Fn(usize) -> [f32; 3], mask: impl Fn(usize) -> u64, buttons: usize) -> JoltAnalyzer {
        let mut a = JoltAnalyzer::new(buttons);
        let t0 = Instant::now();
        for i in 0..ms {
            let report = i / 5 * 5;
            let mut g = level(report);
            for (k, v) in g.iter_mut().enumerate() {
                *v += gauss(report * 3 + k) * 0.00025;
            }
            a.push(MeasureSample { at: t0 + Duration::from_millis(i as u64), gyro: g, mask: mask(report) });
        }
        a
    }

    /// Button 0 pressed for 200 ms, every 500 ms, from 500.
    fn clicks(ms: usize) -> bool {
        ms >= 500 && (ms - 500) % 500 < 200
    }

    /// A ringing jolt of `len` ms at each edge of [`clicks`], `lead` ms early.
    fn jolted(ms: usize, len: usize, lead: usize) -> [f32; 3] {
        let at = ms + lead;
        let since = if at >= 500 { (at - 500) % 500 } else { 9999 };
        let since_edge = if since < 200 { since } else { since - 200 };
        let on = at >= 500 && since_edge < len;
        let v = if on { if since_edge / 5 % 2 == 0 { 0.02 } else { -0.015 } } else { 0.0 };
        [v, -v * 0.5, 0.0]
    }

    #[test]
    fn it_names_the_button_and_how_long_it_jolts() {
        let a = measure(3000, |t| jolted(t, 25, 0), |t| clicks(t) as u64, 2);
        let r = a.report(0);
        assert!(r.jolting(), "{r:?}");
        assert!(r.clicks >= 8, "{r:?}");
        assert!((0.018..0.024).contains(&r.peak), "{r:?}");
        assert!((25.0..=40.0).contains(&r.hold_ms), "{r:?}");
        assert!((20.0..=35.0).contains(&r.length_ms), "{r:?}");
        assert_eq!(r.lead_ms, 0.0, "{r:?}");
        assert_eq!(a.report(1), ButtonJolt::default(), "never pressed");
    }

    #[test]
    fn a_button_that_doesnt_jolt_is_not_called_one() {
        let a = measure(3000, |_| [0.0; 3], |t| clicks(t) as u64, 1);
        let r = a.report(0);
        assert!(r.clicks >= 8 && !r.jolting(), "{r:?}");
        assert_eq!(r.jolts, 0, "{r:?}");
        assert!(r.largest > 0.0 && r.largest < 0.002, "what it did see: {r:?}");
        assert_eq!(r.length_ms, 0.0, "{r:?}");
    }

    #[test]
    fn a_jolt_before_the_edge_is_measured_as_lead() {
        let a = measure(3000, |t| jolted(t, 25, 10), |t| clicks(t) as u64, 1);
        let r = a.report(0);
        assert!(r.jolting(), "{r:?}");
        assert!((5.0..=15.0).contains(&r.lead_ms), "{r:?}");
    }

    #[test]
    fn the_lead_is_a_typical_clicks_not_the_worst() {
        // Jolts start with the edge, but one click also has the pad knocked
        // just before it, running into its jolt.
        let level = |t: usize| {
            if (985..1000).contains(&t) { [0.02, 0.0, 0.0] } else { jolted(t, 25, 0) }
        };
        let r = measure(3000, level, |t| clicks(t) as u64, 1).report(0);
        assert!(r.jolting(), "{r:?}");
        assert_eq!(r.lead_ms, 0.0, "{r:?}");
    }

    #[test]
    fn a_hand_moving_the_pad_is_not_a_jolt() {
        // 100 °/s swings at 1.5 Hz, through every click.
        let swing = |t: usize| [(t as f32 * 0.001 * 9.42).sin() * 0.05, 0.0, 0.0];
        let a = measure(3000, swing, |t| clicks(t) as u64, 1);
        let r = a.report(0);
        assert!(r.clicks >= 8 && !r.jolting(), "{r:?}");
    }

    #[test]
    fn a_jolt_shows_through_a_hand_moving_the_pad() {
        let both = |t: usize| {
            let j = jolted(t, 25, 0);
            [(t as f32 * 0.001 * 9.42).sin() * 0.05 + j[0], j[1], j[2]]
        };
        let a = measure(3000, both, |t| clicks(t) as u64, 1);
        assert!(a.report(0).jolting(), "{:?}", a.report(0));
    }

    #[test]
    fn it_keeps_each_button_apart() {
        let mask = |t: usize| {
            let second = t >= 2000 && (t - 2000) % 500 < 200;
            (t < 2000 && clicks(t)) as u64 | (second as u64) << 1
        };
        let level = |t: usize| if t < 1900 { jolted(t, 25, 0) } else { [0.0; 3] };
        let a = measure(4000, level, mask, 2);
        assert!(a.report(0).jolting(), "{:?}", a.report(0));
        let quiet = a.report(1);
        assert!(quiet.clicks >= 4 && quiet.jolts == 0, "{quiet:?}");
    }

    #[test]
    fn clicks_on_top_of_each_other_are_left_out() {
        let a = measure(2000, |t| jolted(t, 25, 0), |t| (t >= 500 && (t - 500) % 40 < 20) as u64, 1);
        assert_eq!(a.report(0).clicks, 0);
    }

    #[test]
    fn one_jolt_is_not_enough_to_call_it() {
        let r = ButtonJolt { clicks: 1, jolts: 1, ..Default::default() };
        assert!(!r.jolting());
        let r = ButtonJolt { clicks: 8, jolts: 2, ..Default::default() };
        assert!(!r.jolting(), "the odd jolt on a button that mostly doesn't");
        let r = ButtonJolt { clicks: 8, jolts: 4, ..Default::default() };
        assert!(r.jolting(), "only its presses jolting still counts");
    }

    #[test]
    fn measuring_records_what_reaches_the_gate() {
        let dev = "test:measure";
        let mut s = HashMap::new();
        s.insert(pin_key(dev, "gyro_x"), Signal::Float(0.5));
        s.insert(pin_key(dev, "btn_a"), Signal::Bool(true));
        measure_start(dev, &["btn_b".into(), "btn_a".into()]);
        let now = Instant::now();
        ClickGates::default().apply(&HashMap::new(), &mut s, now);
        let got = measure_read(dev).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].gyro[0], got[0].mask), (0.5, 0b10));
        assert!(measure_read(dev).unwrap().is_empty(), "drained");
        measure_stop(dev);
        assert!(measure_read(dev).is_none());
    }
}
