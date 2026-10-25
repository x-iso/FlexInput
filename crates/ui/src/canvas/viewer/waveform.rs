//! Waveform scopes — the Oscilloscope and the Trigger Scope: node bodies,
//! pinned displays and their two control rows.
//!
//! One painter ([`paint_scope`]) serves both modules, bodies and pinned
//! displays alike; a [`Spec`] says what differs — the param keys (`osc_*` /
//! `ts_*`), where the data channels start (the Trigger Scope's column 0 is its
//! trigger), which way time runs (back from the newest sample, or on from the
//! trigger), and whether there is Freeze.
//!
//! Each channel reads in what its wire carries — °/s for a gyro, G for an
//! accelerometer, the bus value otherwise (`extra.scope_channels`, traced from
//! the wiring by `refresh_motion_units`) — on value and time guides laid on
//! round values and labelled, with optional minor guides.
//!
//! Each kind of signal keeps its own scale. When channels of different kinds
//! are wired, the kind picked (`*_axis`, the unit button beside Scale) reads on
//! the left axis and is what the Scale field sets; the next kind reads on the
//! right axis. `*_scale` always holds the picked kind's scale — the others wait
//! in `*_scale_{kind}` and swap in when picked — so the pinned row and gamepad
//! navigation edit whichever kind is shown.
//!
//! Also: per-channel statistics as a fixed-width table that refreshes four
//! times a second, dashed peak lines, a hover readout (time and every
//! channel's value under the pointer), and a scrollbar — on the Oscilloscope
//! while it is frozen (Freeze stops its history taking samples), back through
//! what it holds; on the Trigger Scope always, back into what led up to the
//! trigger (each capture starts with the scope's rolling history), the trigger
//! marked.
//! Long windows draw each pixel column's min and max, so a one-sample spike
//! keeps its full height. The plot is sized by its own corner grip, stored on
//! the node; until set it is as wide as the control rows under it.

use std::collections::HashMap;

use super::*;

/// What a channel carries: its scale and its unit go by this.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Gyro,
    Accel,
    Value,
}

impl Kind {
    /// Also the order the axes are offered in.
    const ALL: [Kind; 3] = [Kind::Gyro, Kind::Accel, Kind::Value];

    fn of(u: Option<MotionUnit>) -> Self {
        match u {
            Some(MotionUnit::Gyro) => Kind::Gyro,
            Some(MotionUnit::Accel) => Kind::Accel,
            None => Kind::Value,
        }
    }

    fn idx(self) -> usize {
        self as usize
    }

    fn key(self) -> &'static str {
        match self { Kind::Gyro => "gyro", Kind::Accel => "accel", Kind::Value => "value" }
    }

    fn from_key(k: &str) -> Option<Self> {
        Kind::ALL.into_iter().find(|kind| kind.key() == k)
    }

    /// The physical value a bus 1.0 stands for.
    fn full_scale(self) -> f32 {
        match self {
            Kind::Gyro => MotionUnit::Gyro.full_scale(),
            Kind::Accel => MotionUnit::Accel.full_scale(),
            Kind::Value => 1.0,
        }
    }

    fn suffix(self) -> &'static str {
        match self { Kind::Gyro => " °/s", Kind::Accel => " G", Kind::Value => "" }
    }

    /// The unit button's text.
    fn label(self) -> &'static str {
        match self { Kind::Gyro => "°/s", Kind::Accel => "G", Kind::Value => "value" }
    }
}

/// A scope module's param names.
struct Keys {
    win: &'static str,
    scale: &'static str,
    auto: &'static str,
    uni: &'static str,
    grid_x: &'static str,
    grid_y: &'static str,
    sub: &'static str,
    labels: &'static str,
    stats: &'static str,
    peaks: &'static str,
    freeze: &'static str,
    axis: &'static str,
    /// Which kind `scale` currently belongs to.
    scale_kind: &'static str,
    /// Each kind's scale while another kind is picked, in [`Kind::ALL`] order.
    kind_scales: [&'static str; 3],
    /// Where the view is scrolled, ms: how far back on a frozen Oscilloscope,
    /// how far before the trigger the window starts on a Trigger Scope.
    scroll: &'static str,
    /// The plot's size, once set by its grip.
    w: &'static str,
    h: &'static str,
}

/// What differs between the two scope modules.
pub(crate) struct Spec {
    keys: Keys,
    /// Data column of channel 0: the Trigger Scope's column 0 is its trigger.
    col0: usize,
    /// Time runs on from the trigger at the left edge (Trigger Scope), rather
    /// than back from the newest sample at the right (Oscilloscope).
    from_trigger: bool,
    /// Has Freeze (and scrolling back while frozen).
    freeze: bool,
    /// Prefix of the body's egui ids.
    salt: &'static str,
}

pub(crate) const OSC: Spec = Spec {
    keys: Keys {
        win: "osc_win_ms", scale: "osc_scale", auto: "osc_auto", uni: "osc_uni",
        grid_x: "osc_grid_x", grid_y: "osc_grid_y", sub: "osc_grid_sub",
        labels: "osc_labels", stats: "osc_stats", peaks: "osc_peaks", freeze: "osc_freeze",
        axis: "osc_axis", scale_kind: "osc_scale_kind",
        kind_scales: ["osc_scale_gyro", "osc_scale_accel", "osc_scale_value"],
        scroll: "osc_scroll_ms", w: "osc_w", h: "osc_h",
    },
    col0: 0,
    from_trigger: false,
    freeze: true,
    salt: "osc",
};

pub(crate) const TRIG: Spec = Spec {
    keys: Keys {
        win: "ts_win_ms", scale: "ts_scale", auto: "ts_auto", uni: "ts_uni",
        grid_x: "ts_grid_x", grid_y: "ts_grid_y", sub: "ts_grid_sub",
        labels: "ts_labels", stats: "ts_stats", peaks: "ts_peaks", freeze: "ts_freeze",
        axis: "ts_axis", scale_kind: "ts_scale_kind",
        kind_scales: ["ts_scale_gyro", "ts_scale_accel", "ts_scale_value"],
        scroll: "ts_scroll_ms", w: "ts_w", h: "ts_h",
    },
    col0: 1,
    from_trigger: true,
    freeze: false,
    salt: "ts",
};

/// The view settings, from the node's params.
#[derive(Clone, Copy, Debug)]
struct View {
    win_ms: f32,
    /// Bus value at the top of the plot (Bi: ± it), for the picked kind.
    scale: f32,
    auto: bool,
    uni: bool,
    /// Divisions across the time axis and the value axis (0 = no guides).
    grid_x: u32,
    grid_y: u32,
    /// Minor guides per division (1 = none).
    sub: u32,
    labels: bool,
    stats: bool,
    peaks: bool,
    freeze: bool,
    /// Where the view is scrolled, ms (see [`Keys::scroll`]).
    scroll_ms: f32,
    /// The kind picked for the left axis and the Scale field.
    axis: Option<Kind>,
    /// Each kind's scale while another kind is picked.
    stored: [Option<f32>; 3],
}

#[derive(Clone, Copy, Debug)]
enum ParamDefault {
    Num(f64),
    Flag(bool),
}

/// Smallest Scale: ±0.002 °/s on a gyro.
const MIN_SCALE: f32 = 1e-6;
const MAX_SCALE: f32 = 100.0;
/// How often the statistics table takes new numbers, s.
const STATS_HOLD_S: f64 = 0.25;
/// The plot's height until its grip sets one, and the floor for both sides.
const DEFAULT_H: f32 = 220.0;
const MIN_W: f32 = 120.0;
const MIN_H: f32 = 60.0;
/// The plot's width before the control rows have been measured once.
const FALLBACK_W: f32 = 420.0;
const SCROLLBAR_H: f32 = 10.0;

impl Spec {
    /// Params and their defaults; written into the node when missing, so
    /// gamepad navigation (which reads a missing toggle as off) agrees with
    /// the body.
    fn defaults(&self) -> [(&'static str, ParamDefault); 11] {
        let k = &self.keys;
        [
            (k.win, ParamDefault::Num(200.0)),
            (k.scale, ParamDefault::Num(1.0)),
            (k.auto, ParamDefault::Flag(false)),
            (k.uni, ParamDefault::Flag(false)),
            (k.grid_x, ParamDefault::Num(4.0)),
            (k.grid_y, ParamDefault::Num(4.0)),
            (k.sub, ParamDefault::Num(1.0)),
            (k.labels, ParamDefault::Flag(true)),
            (k.stats, ParamDefault::Flag(true)),
            (k.peaks, ParamDefault::Flag(false)),
            (k.freeze, ParamDefault::Flag(false)),
        ]
    }

    fn read(&self, n: &NodeData) -> View {
        let k = &self.keys;
        let num = |key: &str, d: f64| n.params.get(key).and_then(|v| v.as_f64()).unwrap_or(d);
        let flag = |key: &str, d: bool| n.params.get(key).and_then(|v| v.as_bool()).unwrap_or(d);
        View {
            win_ms: num(k.win, 200.0).clamp(10.0, 10_000.0) as f32,
            scale: (num(k.scale, 1.0) as f32).clamp(MIN_SCALE, MAX_SCALE),
            auto: flag(k.auto, false),
            uni: flag(k.uni, false),
            grid_x: num(k.grid_x, 4.0).clamp(0.0, 20.0) as u32,
            grid_y: num(k.grid_y, 4.0).clamp(0.0, 20.0) as u32,
            sub: num(k.sub, 1.0).clamp(1.0, 10.0) as u32,
            labels: flag(k.labels, true),
            stats: flag(k.stats, true),
            peaks: flag(k.peaks, false),
            freeze: self.freeze && flag(k.freeze, false),
            scroll_ms: num(k.scroll, 0.0) as f32,
            axis: n.params.get(k.axis).and_then(|v| v.as_str()).and_then(Kind::from_key),
            stored: std::array::from_fn(|i| {
                n.params.get(k.kind_scales[i]).and_then(|v| v.as_f64()).map(|v| (v as f32).clamp(MIN_SCALE, MAX_SCALE))
            }),
        }
    }
}

impl View {
    /// Value divisions between zero and the top of the plot.
    fn divs_above_zero(&self) -> f32 {
        if self.uni { self.grid_y as f32 } else { self.grid_y as f32 * 0.5 }
    }

    /// The picked kind if it is on the scope, else the first that is.
    fn selected(&self, present: &[Kind]) -> Option<Kind> {
        self.axis.filter(|k| present.contains(k)).or(present.first().copied())
    }
}

/// Keep the Scale param the scale of `sel`: when the shown kind changes, park
/// the old kind's scale in its own param and bring the new kind's back.
fn sync_selected_scale(params: &mut HashMap<String, Value>, keys: &Keys, sel: Kind) {
    let held = params.get(keys.scale_kind).and_then(|v| v.as_str()).and_then(Kind::from_key);
    if held == Some(sel) {
        return;
    }
    if let (Some(old), Some(cur)) = (held, params.get(keys.scale).cloned()) {
        params.insert(keys.kind_scales[old.idx()].into(), cur);
    }
    if let Some(v) = params.get(keys.kind_scales[sel.idx()]).cloned() {
        params.insert(keys.scale.into(), v);
    }
    params.insert(keys.scale_kind.into(), Value::String(sel.key().into()));
}

fn init_params(snarl: &mut Snarl<NodeData>, id: NodeId, spec: &Spec) {
    let present = scope_frame(snarl, id, spec).map(|f| f.present()).unwrap_or_default();
    let Some(node) = snarl.get_node_mut(id) else { return };
    for (key, def) in spec.defaults() {
        if !node.params.contains_key(key) {
            let v = match def {
                ParamDefault::Num(x) => serde_json::json!(x),
                ParamDefault::Flag(b) => Value::Bool(b),
            };
            node.params.insert(key.into(), v);
        }
    }
    if let Some(sel) = spec.read(node).selected(&present) {
        sync_selected_scale(&mut node.params, &spec.keys, sel);
    }
}

/// Per-channel statistics over the visible window, in bus units.
#[derive(Clone, Copy, Debug)]
struct ChStats {
    n: usize,
    now: f32,
    min: f32,
    max: f32,
    mean: f32,
    sd: f32,
}

impl ChStats {
    const NONE: ChStats = ChStats { n: 0, now: 0.0, min: 0.0, max: 0.0, mean: 0.0, sd: 0.0 };
}

fn channel_stats(samples: &[&[Option<f32>]], col: usize) -> ChStats {
    let (mut n, mut sum, mut sq) = (0usize, 0.0f64, 0.0f64);
    let (mut min, mut max, mut now) = (f32::INFINITY, f32::NEG_INFINITY, 0.0);
    for v in samples.iter().filter_map(|s| s.get(col).copied().flatten()) {
        n += 1;
        sum += v as f64;
        sq += (v as f64) * (v as f64);
        min = min.min(v);
        max = max.max(v);
        now = v;
    }
    if n == 0 {
        return ChStats::NONE;
    }
    let mean = sum / n as f64;
    let var = (sq / n as f64 - mean * mean).max(0.0);
    ChStats { n, now, min, max, mean: mean as f32, sd: var.sqrt() as f32 }
}

/// Decimals and width (characters, sign included) to print a value of `kind`
/// on a plot of `scale` (bus): fixed for as long as the scale is, so a column
/// of them never moves. Wide enough for ten times the scale.
fn value_format(kind: Kind, scale: f32) -> (usize, usize) {
    let phys = (scale * kind.full_scale()).max(1e-9);
    let dec = (2 - phys.log10().floor() as i32).clamp(0, 6) as usize;
    let int_digits = ((phys * 10.0).log10().floor() as i32 + 1).max(1) as usize;
    (dec, 1 + int_digits + if dec > 0 { 1 + dec } else { 0 })
}

/// The statistics table: a header and one row per channel, every column
/// fixed-width so nothing shifts as the numbers change. `rows` are `(name,
/// kind, stats, scale)`; `fit` is how many characters the scope has room for.
/// The columns kept are chosen by that room alone, so the table's shape only
/// changes when the scope is resized.
fn stats_table(rows: &[(String, Kind, ChStats, f32)], fit: usize) -> (String, Vec<String>) {
    const COLS: [&str; 5] = ["now", "min", "max", "mean", "σ"];
    let w = rows.iter().map(|(_, k, _, s)| value_format(*k, *s).1).max().unwrap_or(6).max(4);
    let unit_w = rows.iter().map(|(_, k, _, _)| k.suffix().trim_start().chars().count()).max().unwrap_or(0);
    let name_w = rows.iter().map(|(n, ..)| n.chars().count()).max().unwrap_or(0).min(16);
    let width = |name: usize, cols: usize| name + (if name > 0 { 1 } else { 0 }) + cols * (w + 1) + unit_w;
    let (name_w, ncols) = [(name_w, 5), (name_w, 4), (name_w, 3), (0, 3), (0, 1)]
        .into_iter()
        .find(|(nw, c)| width(*nw, *c) <= fit)
        .unwrap_or((0, 1));
    let name_cell = |s: &str| {
        if name_w == 0 {
            return String::new();
        }
        let n = s.chars().count();
        let cut: String = if n > name_w { s.chars().take(name_w - 1).chain(['…']).collect() } else { s.into() };
        format!("{cut:<name_w$} ")
    };
    let mut header = name_cell("");
    for c in &COLS[..ncols] {
        header.push_str(&format!("{c:>w$} "));
    }
    let lines = rows.iter().map(|(name, kind, s, scale)| {
        let (dec, _) = value_format(*kind, *scale);
        let fs = kind.full_scale();
        let mut line = name_cell(name);
        let vals = [s.now, s.min, s.max, s.mean, s.sd];
        for (i, v) in vals[..ncols].iter().enumerate() {
            let x = v * fs;
            line.push_str(&if i == 4 { format!("{x:>w$.dec$} ") } else { format!("{x:>+w$.dec$} ") });
        }
        line.push_str(&format!("{:<unit_w$}", kind.suffix().trim_start()));
        line
    }).collect();
    (header, lines)
}

/// A label with a dark backing, so it reads over traces and guides. Returns
/// its width.
fn backed_text(p: &egui::Painter, pos: egui::Pos2, align: egui::Align2, text: String, size: f32, col: Color32) -> f32 {
    let galley = p.layout_no_wrap(text, egui::FontId::proportional(size), col);
    let rect = align.anchor_size(pos, galley.size());
    p.rect_filled(rect.expand2(egui::vec2(2.0, 0.0)), 2.0, Color32::from_black_alpha(150));
    p.galley(rect.min, galley, col);
    rect.width()
}

/// The samples a scope draws from.
enum Rows {
    /// The Oscilloscope's rolling history.
    History(std::sync::Arc<std::collections::VecDeque<Vec<Option<f32>>>>),
    /// The Trigger Scope's capture — its lead-up, then the window from the
    /// trigger — filling while armed and held after.
    Capture(std::sync::Arc<crate::canvas::node::TrigCapture>),
}

/// Everything the painter needs about one scope this frame.
struct Frame<'s> {
    spec: &'s Spec,
    id: NodeId,
    view: View,
    rows: Rows,
    /// Per INPUT (the Trigger Scope's include its trigger).
    channels: Vec<ScopeChannel>,
    wired: Vec<bool>,
    /// Data channels.
    n_channels: usize,
}

impl Frame<'_> {
    fn input(&self, ch: usize) -> usize {
        ch + self.spec.col0
    }

    fn kind_of(&self, ch: usize) -> Kind {
        Kind::of(self.channels.get(self.input(ch)).and_then(|c| c.unit))
    }

    fn wired(&self, ch: usize) -> bool {
        self.wired.get(self.input(ch)).copied().unwrap_or(false)
    }

    /// The kinds on the scope's wired channels, in axis order.
    fn present(&self) -> Vec<Kind> {
        Kind::ALL.into_iter()
            .filter(|k| (0..self.n_channels).any(|ch| self.wired(ch) && self.kind_of(ch) == *k))
            .collect()
    }

    /// Left axis: the picked kind; right: the next one on the scope.
    fn axes(&self) -> (Option<Kind>, Option<Kind>) {
        let present = self.present();
        let left = self.view.selected(&present);
        (left, present.into_iter().find(|k| Some(*k) != left))
    }

    fn name(&self, ch: usize) -> String {
        self.channels.get(self.input(ch)).map(|c| c.name.clone()).filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("ch{}", ch + 1))
    }
}

fn scope_frame<'s>(snarl: &Snarl<NodeData>, id: NodeId, spec: &'s Spec) -> Option<Frame<'s>> {
    let n = snarl.get_node(id)?;
    let n_inputs = n.inputs.len();
    let wired = (0..n_inputs)
        .map(|i| !snarl.in_pin(InPinId { node: id, input: i }).remotes.is_empty())
        .collect();
    let rows = if spec.from_trigger {
        if n.extra.trig_armed {
            Rows::Capture(std::sync::Arc::clone(&n.extra.trig_acc))
        } else {
            Rows::Capture(n.extra.trig_capture.clone().unwrap_or_default())
        }
    } else {
        Rows::History(std::sync::Arc::clone(&n.extra.history))
    };
    Some(Frame {
        spec,
        id,
        view: spec.read(n),
        rows,
        channels: n.extra.scope_channels.clone(),
        wired,
        n_channels: n_inputs.saturating_sub(spec.col0).max(1),
    })
}

/// Auto's scale for a kind: its channels' peak, rounded up so the value guides
/// land on round numbers in its unit when they are labelled.
fn auto_scale(view: &View, peak: f32, kind: Kind) -> f32 {
    let peak = if peak > 0.0 { peak } else { 1.0 };
    let divs = view.divs_above_zero();
    if !view.labels || divs <= 0.0 {
        return peak;
    }
    let fs = kind.full_scale() as f64;
    let step = crate::panels::scope_export::nice_step(peak as f64 * fs / divs as f64);
    (step * divs as f64 / fs) as f32
}

/// Each kind's scale this frame (bus units): Auto fits each kind to its own
/// channels; otherwise the picked kind has the Scale field's and the others
/// their own — fitted, if they have never been set.
fn kind_scales(f: &Frame, stats: &[ChStats]) -> [f32; 3] {
    let sel = f.view.selected(&f.present());
    let peak = |k: Kind| {
        stats.iter().enumerate()
            .filter(|(ch, s)| s.n > 0 && f.kind_of(*ch) == k)
            .map(|(_, s)| s.min.abs().max(s.max.abs()))
            .fold(0.0f32, f32::max)
    };
    Kind::ALL.map(|k| {
        let s = if f.view.auto {
            auto_scale(&f.view, peak(k), k)
        } else if Some(k) == sel {
            f.view.scale
        } else {
            f.view.stored[k.idx()].unwrap_or_else(|| auto_scale(&f.view, peak(k), k))
        };
        s.clamp(MIN_SCALE, MAX_SCALE)
    })
}

/// Samples per window, at the engine's rate.
fn window_samples(view: &View) -> usize {
    ((view.win_ms / 1000.0 * current_sample_rate().max(1) as f32) as usize).max(2)
}

/// Where the view sits in what the scope holds, in samples: `start` is the
/// window's first sample, `total` what the scrollbar spans.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    total: usize,
    win: usize,
    start: usize,
}

fn view_span(f: &Frame) -> Span {
    let win = window_samples(&f.view);
    let scroll = (f.view.scroll_ms / 1000.0 * current_sample_rate().max(1) as f32).round();
    match &f.rows {
        Rows::History(h) => {
            // Only a frozen view scrolls, back from the newest window.
            let newest = h.len().saturating_sub(win);
            let back = if f.view.freeze { (scroll.max(0.0) as usize).min(newest) } else { 0 };
            Span { total: h.len(), win, start: newest - back }
        }
        Rows::Capture(c) => {
            // The window starts `scroll` before the trigger. A capture still
            // filling will reach `t0 + win`, so it is measured as if it had.
            let total = c.rows.len().max(c.t0 + win);
            let last = total.saturating_sub(win);
            let start = (c.t0 as f32 - scroll).clamp(0.0, last as f32) as usize;
            Span { total, win, start }
        }
    }
}

/// Whether the scrollbar shows: on a frozen Oscilloscope, and on a Trigger
/// Scope, once there is more than the window to see.
fn scrollable(f: &Frame) -> bool {
    let sp = view_span(f);
    sp.total > sp.win && match f.rows {
        Rows::History(_) => f.view.freeze,
        Rows::Capture(_) => true,
    }
}

/// The scroll param (ms) that puts the window at `start`.
fn scroll_ms_for(f: &Frame, start: usize) -> f32 {
    let rate = current_sample_rate().max(1) as f32;
    match &f.rows {
        Rows::History(h) => h.len().saturating_sub(window_samples(&f.view)).saturating_sub(start) as f32 / rate * 1000.0,
        Rows::Capture(c) => (c.t0 as f32 - start as f32) / rate * 1000.0,
    }
}

/// Paint the scope into `rect`; `hover` is the pointer, for the readout.
fn paint_scope(
    ui: &egui::Ui,
    rect: egui::Rect,
    f: &Frame,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
    hover: Option<egui::Pos2>,
) {
    let v = f.view;
    let col0 = f.spec.col0;
    let painter = ui.painter_at(rect);
    let (bg, outline) = graph_chrome(graph_ov);
    painter.rect_filled(rect, 2.0, bg);
    let (grid_faint, grid_axis) = graph_grid_colors(graph_ov);
    let grid_minor = grid_faint.gamma_multiply(0.6);

    // The window's samples, oldest first (borrowed, not copied).
    let rate = current_sample_rate().max(1) as f32;
    let osc_win = window_samples(&v);
    let sp = view_span(f);
    let visible: Vec<&[Option<f32>]> = match &f.rows {
        Rows::History(h) => h.range(sp.start..(sp.start + osc_win).min(h.len())).map(Vec::as_slice).collect(),
        Rows::Capture(c) => {
            let len = c.rows.len();
            c.rows[sp.start.min(len)..(sp.start + osc_win).min(len)].iter().map(Vec::as_slice).collect()
        }
    };
    let n = visible.len();
    let waiting = matches!(&f.rows, Rows::Capture(c) if c.rows.len() <= c.t0);
    // A visible sample's time, ms: back from the newest held (≤ 0), or from
    // the trigger.
    let t_of = |i: f32| -> f32 {
        match &f.rows {
            Rows::History(h) => (sp.start as f32 + i - (h.len() as f32 - 1.0)) / rate * 1000.0,
            Rows::Capture(c) => (sp.start as f32 + i - c.t0 as f32) / rate * 1000.0,
        }
    };

    let stats: Vec<ChStats> = (0..f.n_channels).map(|ch| channel_stats(&visible, ch + col0)).collect();
    let scales = kind_scales(f, &stats);
    let (left, right) = f.axes();
    let left_k = left.unwrap_or(Kind::Value);
    let scale = scales[left_k.idx()];
    let scale_of = |ch: usize| scales[f.kind_of(ch).idx()];

    // Value → y on a scale. Bi: ±scale over 90% of the height; Uni: 0..scale
    // over 92%.
    let y_on = |val: f32, scale: f32| -> f32 {
        let norm = val / scale;
        if v.uni {
            rect.bottom() - norm.clamp(0.0, 1.0) * rect.height() * 0.92
        } else {
            rect.center().y - norm.clamp(-1.0, 1.0) * rect.height() * 0.45
        }
    };
    let y_of = |val: f32| y_on(val, scale);
    // Sample index (in the window) → x: the newest at the right edge, or the
    // trigger at the left.
    let span = (osc_win as f32 - 1.0).max(1.0);
    let x_of = |i: f32| if f.spec.from_trigger {
        rect.left() + i / span * rect.width()
    } else {
        rect.right() - (n as f32 - 1.0 - i) / span * rect.width()
    };

    // ── Value guides ──
    if v.grid_y > 0 {
        let lo = if v.uni { 0.0 } else { -scale };
        let total = v.grid_y * v.sub;
        for k in 0..=total {
            let val = lo + (scale - lo) * k as f32 / total as f32;
            let y = y_of(val);
            let major = k % v.sub == 0;
            let is_zero = val.abs() < scale * 1e-4;
            let (w, col) = if is_zero { (1.0, grid_axis) } else if major { (0.5, grid_faint) } else { (0.5, grid_minor) };
            if !major && rect.height() * 0.9 / (total as f32) < 4.0 {
                continue;
            }
            painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(w, col));
        }
    } else if !v.uni {
        let y = rect.center().y;
        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(1.0, grid_axis));
    }
    if v.uni {
        painter.line_segment([egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.bottom())], egui::Stroke::new(1.0, grid_axis));
    }

    // ── The trigger, where it is in view ──
    if let Rows::Capture(c) = &f.rows {
        if c.rows.len() > c.t0 && (sp.start..sp.start + n).contains(&c.t0) {
            let x = x_of((c.t0 - sp.start) as f32);
            let amber = Color32::from_rgb(255, 196, 90);
            painter.extend(egui::Shape::dashed_line(
                &[egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(1.0, amber.gamma_multiply(0.8)), 4.0, 3.0,
            ));
            painter.add(egui::Shape::convex_polygon(
                vec![egui::pos2(x - 4.0, rect.top()), egui::pos2(x + 4.0, rect.top()), egui::pos2(x, rect.top() + 6.0)],
                amber, egui::Stroke::NONE,
            ));
        }
    }

    // ── Time guides ──
    if v.grid_x > 0 {
        let total = v.grid_x * v.sub;
        for k in 1..total {
            let x = rect.left() + rect.width() * k as f32 / total as f32;
            let major = k % v.sub == 0;
            if !major && rect.width() / (total as f32) < 4.0 {
                continue;
            }
            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(0.5, if major { grid_faint } else { grid_minor }));
        }
    }

    // ── Traces: each pixel column's min and max once samples outnumber pixels ──
    let cols = (rect.width().ceil() as usize).max(2);
    for ch in 0..f.n_channels {
        let col = graph_channel_color(graph_ov, ch);
        let ch_scale = scale_of(ch);
        let y_of = |val: f32| y_on(val, ch_scale);
        let c_in = ch + col0;
        let pts: Vec<egui::Pos2> = if n <= cols * 2 {
            visible.iter().enumerate()
                .filter_map(|(i, s)| s.get(c_in).copied().flatten().map(|val| egui::pos2(x_of(i as f32), y_of(val))))
                .collect()
        } else {
            let mut pts = Vec::with_capacity(cols * 2);
            for c in 0..cols {
                let (i0, i1) = (c * n / cols, ((c + 1) * n / cols).max(c * n / cols + 1).min(n));
                let (lo, hi) = visible[i0..i1].iter()
                    .filter_map(|s| s.get(c_in).copied().flatten())
                    .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), x| (lo.min(x), hi.max(x)));
                if lo > hi {
                    continue;
                }
                let x = x_of((i0 + i1) as f32 * 0.5);
                let (a, b) = if c % 2 == 0 { (lo, hi) } else { (hi, lo) };
                pts.push(egui::pos2(x, y_of(a)));
                pts.push(egui::pos2(x, y_of(b)));
            }
            pts
        };
        if pts.len() >= 2 {
            painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, col)));
        }
        // Dashed lines at the window's min and max.
        if v.peaks && stats[ch].n > 0 {
            for val in [stats[ch].min, stats[ch].max] {
                let y = y_of(val);
                painter.extend(egui::Shape::dashed_line(
                    &[egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                    egui::Stroke::new(1.0, col.gamma_multiply(0.7)), 5.0, 4.0,
                ));
            }
        }
    }

    // ── Labels ──
    // Widths of the value-label columns, left and right, for the statistics.
    let (mut left_w, mut right_w) = (0.0f32, 0.0f32);
    let size = 10.0;
    let label_col = Color32::from_gray(185);
    // x → the (fractional) visible sample under it: the inverse of `x_of`.
    let i_at = |x: f32| if f.spec.from_trigger {
        (x - rect.left()) / rect.width() * span
    } else {
        (n as f32 - 1.0) - (rect.right() - x) / rect.width() * span
    };
    // A time as a label: signed, ms or s.
    let fmt_t = |t: f32, secs: bool, dec: usize| {
        let (v, u) = if secs { (t / 1000.0, " s") } else { (t, " ms") };
        let m = format!("{:.*}", dec, v.abs());
        if m.trim_start_matches(['0', '.']).is_empty() {
            format!("0{u}")
        } else {
            format!("{}{m}{u}", if v < 0.0 { "−" } else { "+" })
        }
    };
    if v.labels {
        if v.grid_y > 0 && rect.height() >= 40.0 {
            // Every label must fit its division; thin them out on a short plot.
            let px = rect.height() * 0.9 / v.grid_y as f32;
            let every = ((13.0 / px).ceil() as u32).max(1);
            let top_k = v.grid_y / every * every;
            // `(kind, its scale)` per side.
            let sides = [Some((left_k, scale)), right.map(|k| (k, scales[k.idx()]))];
            for k in (0..=v.grid_y).step_by(every as usize) {
                let lo = if v.uni { 0.0 } else { -scale };
                let y = y_of(lo + (scale - lo) * k as f32 / v.grid_y as f32).clamp(rect.top() + 6.0, rect.bottom() - 6.0);
                for (side, axis) in sides.iter().enumerate() {
                    let Some((kind, s)) = *axis else { continue };
                    let lo = if v.uni { 0.0 } else { -s };
                    let step = (s - lo) / v.grid_y as f32;
                    let fs = kind.full_scale();
                    let dec = crate::panels::scope_export::decimals((step * fs) as f64).min(5);
                    let x = (lo + step * k as f32) * fs;
                    let num = if x.abs() < 1e-9 { "0".to_string() } else { format!("{:+.*}", dec, x).replacen('-', "−", 1) };
                    let txt = if k == top_k { format!("{num}{}", kind.suffix()) } else { num };
                    if side == 0 {
                        left_w = left_w.max(backed_text(&painter, egui::pos2(rect.left() + 3.0, y), egui::Align2::LEFT_CENTER, txt, size, label_col));
                    } else {
                        right_w = right_w.max(backed_text(&painter, egui::pos2(rect.right() - 3.0, y), egui::Align2::RIGHT_CENTER, txt, size, label_col));
                    }
                }
            }
        }
        if v.grid_x > 0 && rect.width() >= 120.0 {
            let step_ms = v.win_ms / v.grid_x as f32;
            let secs = t_of(i_at(rect.left())).abs().max(t_of(i_at(rect.right())).abs()) >= 2000.0;
            let dec = crate::panels::scope_export::decimals((step_ms / if secs { 1000.0 } else { 1.0 }) as f64).min(3);
            let every = ((60.0 / (rect.width() / v.grid_x as f32)).ceil() as u32).max(1);
            for k in (1..v.grid_x).step_by(every as usize) {
                let x = rect.left() + rect.width() * k as f32 / v.grid_x as f32;
                let txt = fmt_t(t_of(i_at(x)), secs, dec);
                let _ = backed_text(&painter, egui::pos2(x, rect.bottom() - 2.0), egui::Align2::CENTER_BOTTOM, txt, size, label_col);
            }
        }
    }

    // ── Statistics: a fixed-width table, refreshed four times a second ──
    if v.stats {
        let now_t = ui.input(|i| i.time);
        let key = egui::Id::new(("scope_stats_hold", f.spec.salt, f.id));
        // Held numbers, refreshed at once when the view moves (scrolled) or
        // holds still (frozen).
        let held: Vec<ChStats> = ui.ctx().data_mut(|d| {
            let e = d.get_temp_mut_or_insert_with(key, || (f64::NEG_INFINITY, usize::MAX, Vec::<ChStats>::new()));
            if now_t - e.0 >= STATS_HOLD_S || e.1 != sp.start || e.2.len() != stats.len() || v.freeze {
                *e = (now_t, sp.start, stats.clone());
            }
            e.2.clone()
        });
        let chans: Vec<usize> = (0..f.n_channels)
            .filter(|ch| f.wired(*ch) && held.get(*ch).is_some_and(|s| s.n > 0))
            .collect();
        let rows: Vec<(String, Kind, ChStats, f32)> = chans.iter()
            .map(|&ch| (f.name(ch), f.kind_of(ch), held[ch], scale_of(ch)))
            .collect();
        if !rows.is_empty() {
            let font = egui::FontId::monospace(10.0);
            let cw = painter.layout_no_wrap("0".into(), font.clone(), Color32::WHITE).size().x.max(1.0);
            let x0 = rect.left() + 4.0 + if left_w > 0.0 { left_w + 6.0 } else { 0.0 };
            let x1 = rect.right() - 4.0 - if right_w > 0.0 { right_w + 6.0 } else { 0.0 };
            let fit = ((x1 - x0) / cw).floor().max(0.0) as usize;
            let (header, lines) = stats_table(&rows, fit);
            let line_h = painter.layout_no_wrap("0".into(), font.clone(), Color32::WHITE).size().y;
            let fits = ((rect.height() - 8.0) / (line_h + 1.0)).floor() as usize;
            let shown = lines.len().min(fits.saturating_sub(1));
            if shown > 0 {
                let table_w = header.chars().count().max(lines[0].chars().count()) as f32 * cw;
                let bg_rect = egui::Rect::from_min_size(egui::pos2(x0, rect.top() + 3.0),
                    egui::vec2(table_w, (shown + 1) as f32 * (line_h + 1.0)));
                painter.rect_filled(bg_rect.expand2(egui::vec2(3.0, 1.0)), 2.0, Color32::from_black_alpha(165));
                let mut y = rect.top() + 3.0;
                painter.text(egui::pos2(x0, y), egui::Align2::LEFT_TOP, header, font.clone(), Color32::from_gray(130));
                for (line, &ch) in lines.into_iter().zip(&chans).take(shown) {
                    y += line_h + 1.0;
                    painter.text(egui::pos2(x0, y), egui::Align2::LEFT_TOP, line, font.clone(), graph_channel_color(graph_ov, ch));
                }
            }
        }
    }

    // ── Hover: the time and every channel's value under the pointer ──
    if let Some(p) = hover.filter(|p| rect.contains(*p)) {
        let idx_f = if f.spec.from_trigger {
            (p.x - rect.left()) / rect.width() * span
        } else {
            (n as f32 - 1.0) - (rect.right() - p.x) / rect.width() * span
        };
        if n > 0 && idx_f >= -0.5 && idx_f <= n as f32 - 0.5 {
            let idx = (idx_f.round().max(0.0) as usize).min(n - 1);
            let x = x_of(idx as f32);
            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(1.0, Color32::from_white_alpha(110)));
            let t_ms = t_of(idx as f32);
            let mut lines: Vec<(String, Color32)> = vec![(
                if t_ms.abs() >= 1000.0 { fmt_t(t_ms, true, 3) } else { fmt_t(t_ms, false, 1) },
                Color32::from_gray(200),
            )];
            let name_w = (0..f.n_channels).filter(|ch| f.wired(*ch)).map(|ch| f.name(ch).chars().count()).max().unwrap_or(0);
            for ch in (0..f.n_channels).filter(|ch| f.wired(*ch)) {
                let Some(val) = visible[idx].get(ch + col0).copied().flatten() else { continue };
                let kind = f.kind_of(ch);
                let col = graph_channel_color(graph_ov, ch);
                painter.circle_filled(egui::pos2(x, y_on(val, scale_of(ch))), 3.0, col);
                let (dec, w) = value_format(kind, scale_of(ch));
                lines.push((format!("{:<name_w$} {:>+w$.dec$}{}", f.name(ch), val * kind.full_scale(), kind.suffix()), col));
            }
            let font = egui::FontId::monospace(10.0);
            let galleys: Vec<_> = lines.into_iter().map(|(t, c)| painter.layout_no_wrap(t, font.clone(), c)).collect();
            let bw = galleys.iter().map(|g| g.size().x).fold(0.0f32, f32::max);
            let bh: f32 = galleys.iter().map(|g| g.size().y + 1.0).sum();
            let bx = if x + 10.0 + bw < rect.right() - 2.0 { x + 10.0 } else { x - 10.0 - bw };
            let by = (p.y - bh * 0.5).clamp(rect.top() + 2.0, (rect.bottom() - 2.0 - bh).max(rect.top() + 2.0));
            painter.rect_filled(egui::Rect::from_min_size(egui::pos2(bx, by), egui::vec2(bw, bh)).expand(3.0), 3.0, Color32::from_black_alpha(200));
            let mut y = by;
            for g in galleys {
                let h = g.size().y;
                painter.galley(egui::pos2(bx, y), g, Color32::WHITE);
                y += h + 1.0;
            }
        }
    }

    if waiting {
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Waiting for trigger…",
            egui::FontId::proportional(11.0), Color32::from_gray(90));
    }
    // Frozen: a blue frame, and the word at the bottom left, clear of the
    // statistics and the value labels.
    if v.freeze {
        let ice = Color32::from_rgb(140, 200, 255);
        painter.rect_stroke(rect.shrink(1.0), 2.0, egui::Stroke::new(2.0, ice), egui::StrokeKind::Inside);
        let x = rect.left() + 4.0 + if left_w > 0.0 { left_w + 6.0 } else { 0.0 };
        let _ = backed_text(&painter, egui::pos2(x, rect.bottom() - 3.0), egui::Align2::LEFT_BOTTOM, "FROZEN".into(), 10.0, ice);
    }
    if let Some(stroke) = outline {
        painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
    }
}

/// The scrollbar under a scope: the thumb is the window, the track all the
/// scope holds — a frozen Oscilloscope's history, a Trigger Scope's capture
/// with its lead-up (the trigger marked). Drag the thumb, or click the track to
/// jump there. Returns the new scroll param, ms.
fn scrollbar(ui: &mut egui::Ui, rect: egui::Rect, f: &Frame, salt: impl std::hash::Hash) -> Option<f32> {
    let sp = view_span(f);
    let resp = ui.interact(rect, ui.id().with(("scope_scroll", salt)), egui::Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, Color32::from_gray(28));
    if sp.total <= sp.win {
        return None;
    }
    let to_x = |i: f32| rect.left() + i / sp.total as f32 * rect.width();
    if let Rows::Capture(c) = &f.rows {
        let x = to_x(c.t0 as f32);
        p.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(2.0, Color32::from_rgb(255, 196, 90).gamma_multiply(0.7)));
    }
    let thumb_w = (sp.win as f32 / sp.total as f32 * rect.width()).max(12.0);
    let x0 = to_x(sp.start as f32).min(rect.right() - thumb_w);
    let thumb = egui::Rect::from_min_max(egui::pos2(x0, rect.top() + 1.0), egui::pos2(x0 + thumb_w, rect.bottom() - 1.0));
    let active = resp.dragged() || resp.hovered();
    p.rect_filled(thumb, 3.0, if active { Color32::from_gray(150) } else { Color32::from_gray(105) });
    // The pointer's x puts the thumb's centre there.
    let from_pointer = |x: f32| {
        let start = ((x - rect.left()) / rect.width() * sp.total as f32 - sp.win as f32 * 0.5)
            .clamp(0.0, (sp.total - sp.win) as f32);
        scroll_ms_for(f, start.round() as usize)
    };
    if resp.dragged() || resp.clicked() {
        return resp.interact_pointer_pos().map(|pos| from_pointer(pos.x));
    }
    None
}

/// The plot's corner grip: drag to size it. Returns the new size.
fn size_grip(ui: &mut egui::Ui, plot: egui::Rect, salt: impl std::hash::Hash) -> Option<egui::Vec2> {
    let grip = egui::Rect::from_min_size(plot.right_bottom() - egui::vec2(12.0, 12.0), egui::vec2(12.0, 12.0));
    let resp = ui.interact(grip, ui.id().with(("scope_grip", salt)), egui::Sense::drag());
    let col = if resp.hovered() || resp.dragged() { Color32::from_gray(200) } else { Color32::from_gray(120) };
    let p = ui.painter();
    for d in [4.0, 8.0, 12.0] {
        p.line_segment([egui::pos2(grip.right() - d, grip.bottom() - 1.0), egui::pos2(grip.right() - 1.0, grip.bottom() - d)],
            egui::Stroke::new(1.0, col));
    }
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    }
    resp.dragged().then(|| plot.size() + resp.drag_delta())
}

/// The kind the Scale field sets now, and the kinds on the scope.
fn scale_kind(snarl: &Snarl<NodeData>, id: NodeId, spec: &Spec) -> (Kind, Vec<Kind>) {
    let Some(f) = scope_frame(snarl, id, spec) else { return (Kind::Value, Vec::new()) };
    let present = f.present();
    (f.view.selected(&present).unwrap_or(Kind::Value), present)
}

/// The first control row's state: what it shows, then what was picked.
struct Controls {
    win_ms: f32,
    /// The Scale field, in the kind's unit.
    phys: f32,
    kind: Kind,
    present: Vec<Kind>,
    auto: bool,
    uni: bool,
    freeze: bool,
    /// The unit button's pick, applied (scales swapped) next frame.
    pick: Option<Kind>,
    scale_edited: bool,
}

impl Controls {
    fn of(snarl: &Snarl<NodeData>, id: NodeId, spec: &Spec, view: &View) -> Self {
        let (kind, present) = scale_kind(snarl, id, spec);
        Self {
            win_ms: view.win_ms,
            phys: view.scale * kind.full_scale(),
            kind,
            present,
            auto: view.auto,
            uni: view.uni,
            freeze: view.freeze,
            pick: None,
            scale_edited: false,
        }
    }

    fn write(&self, node: &mut NodeData, spec: &Spec) {
        let k = &spec.keys;
        if let Some(n) = Number::from_f64(self.win_ms as f64) { node.params.insert(k.win.into(), Value::Number(n)); }
        if self.scale_edited {
            if let Some(n) = Number::from_f64((self.phys / self.kind.full_scale()) as f64) {
                node.params.insert(k.scale.into(), Value::Number(n));
            }
        }
        node.params.insert(k.auto.into(), Value::Bool(self.auto));
        node.params.insert(k.uni.into(), Value::Bool(self.uni));
        if spec.freeze {
            node.params.insert(k.freeze.into(), Value::Bool(self.freeze));
            if !self.freeze {
                node.params.insert(k.scroll.into(), serde_json::json!(0.0));
            }
        }
        if let Some(kind) = self.pick {
            node.params.insert(k.axis.into(), Value::String(kind.key().into()));
        }
    }
}

/// The first control row: Win, Scale (Auto shows "auto") and its unit button,
/// Auto, Bi/Uni, and Freeze where the module has it. `flex` is the pinned
/// container, whose surplus width the Win slider takes. Returns the field
/// rects in gamepad-nav order — Win, Scale, unit, Auto, Uni, Freeze — and
/// whether anything changed.
fn controls_row_ui(ui: &mut egui::Ui, c: &mut Controls, spec: &Spec, small: bool, flex: Option<egui::Vec2>) -> ([egui::Rect; 6], bool) {
    let t = |s: String| if small { egui::RichText::new(s).small().weak() } else { egui::RichText::new(s).weak() };
    let rt = |s: &str| if small { egui::RichText::new(s).small() } else { egui::RichText::new(s) };
    let mut fr = [egui::Rect::NOTHING; 6];
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(t("Win".into()));
        if let Some(container) = flex {
            ui.spacing_mut().slider_width = pin_flex_width(ui, container, 70.0);
        }
        let r = ui.add(egui::Slider::new(&mut c.win_ms, 10.0f32..=10_000.0).logarithmic(true).show_value(false));
        fr[0] = r.rect;
        changed |= r.changed();
        ui.label(t(if c.win_ms >= 1000.0 { format!("{:.1}s", c.win_ms / 1000.0) } else { format!("{:.0}ms", c.win_ms) }));
        ui.separator();
        ui.label(t("Scale".into()));
        if c.auto {
            fr[1] = ui.label(t("auto".into())).rect;
        } else {
            let fs = c.kind.full_scale();
            let speed = (c.phys.abs() * 0.01).max(MIN_SCALE * fs);
            let mut dv = egui::DragValue::new(&mut c.phys).speed(speed).range(MIN_SCALE * fs..=MAX_SCALE * fs).max_decimals(4);
            if !c.kind.suffix().is_empty() {
                dv = dv.suffix(c.kind.suffix());
            }
            let r = ui.add(dv).on_hover_text("The value at the top of the plot (±, in Bi)");
            fr[1] = r.rect;
            c.scale_edited |= r.changed();
            changed |= r.changed();
        }
        // The unit button, with two kinds or more on the scope.
        if c.present.len() >= 2 {
            let next = c.present[(c.present.iter().position(|k| *k == c.kind).unwrap_or(0) + 1) % c.present.len()];
            let r = ui.button(rt(&format!("{} ⇄", c.kind.label()))).on_hover_text(format!(
                "Scale and left axis in {} — each kind of signal keeps its own scale.\nClick for {}.",
                c.kind.label(), next.label()));
            fr[2] = r.rect;
            if r.clicked() {
                c.pick = Some(next);
                changed = true;
            }
        }
        let r = ui.checkbox(&mut c.auto, rt("Auto"))
            .on_hover_text("Fit each kind of signal's scale to its peak, rounded so the guides land on round values");
        fr[3] = r.rect;
        changed |= r.changed();
        ui.separator();
        let was = c.uni;
        let rb = ui.selectable_value(&mut c.uni, false, rt("Bi"));
        let ru = ui.selectable_value(&mut c.uni, true, rt("Uni"));
        fr[4] = rb.rect.union(ru.rect);
        changed |= c.uni != was;
        if spec.freeze {
            ui.separator();
            let r = ui.toggle_value(&mut c.freeze, rt("Freeze"))
                .on_hover_text("Stop taking samples and hold what is shown; a scrollbar then goes back through it");
            fr[5] = r.rect;
            changed |= r.changed();
        }
    });
    (fr, changed)
}

/// The guide / label / statistics settings, laid out by `ui` (one row).
/// Returns the six field rects (gamepad nav order) and whether anything
/// changed. Shared by the bodies and the pinned rows.
fn grid_row_ui(ui: &mut egui::Ui, view: &mut View, small: bool) -> ([egui::Rect; 6], bool) {
    let t = |s: &str| if small { egui::RichText::new(s).small().weak() } else { egui::RichText::new(s).weak() };
    let c = |s: &str| if small { egui::RichText::new(s).small() } else { egui::RichText::new(s) };
    let mut fr = [egui::Rect::NOTHING; 6];
    let mut changed = false;
    ui.horizontal(|ui| {
        let mut count = |ui: &mut egui::Ui, label: &str, v: &mut u32, lo: u32, hi: u32, hint: &str| {
            ui.label(t(label));
            let r = ui.add(egui::DragValue::new(v).range(lo..=hi).speed(0.1)).on_hover_text(hint);
            changed |= r.changed();
            r.rect
        };
        fr[0] = count(ui, "Grid H", &mut view.grid_x, 0, 20, "Time divisions across the window (0 = none)");
        fr[1] = count(ui, "V", &mut view.grid_y, 0, 20, "Value divisions across the scale (0 = none)");
        fr[2] = count(ui, "Sub", &mut view.sub, 1, 10, "Minor guides per division (1 = none)");
        ui.separator();
        for (i, (label, v, hint)) in [
            ("Labels", &mut view.labels, "Values on the guides — in °/s for a gyro, G for an accelerometer — and the time along the bottom"),
            ("Stats", &mut view.stats, "Per channel over the window: now, min, max, mean (a DC offset) and σ (the noise)"),
            ("Peaks", &mut view.peaks, "Dashed lines at each channel's min and max over the window"),
        ].into_iter().enumerate() {
            let r = ui.checkbox(v, c(label)).on_hover_text(hint);
            changed |= r.changed();
            fr[3 + i] = r.rect;
        }
    });
    (fr, changed)
}

fn write_grid(node: &mut NodeData, spec: &Spec, view: &View) {
    let k = &spec.keys;
    for (key, x) in [(k.grid_x, view.grid_x), (k.grid_y, view.grid_y), (k.sub, view.sub)] {
        node.params.insert(key.into(), serde_json::json!(x));
    }
    for (key, b) in [(k.labels, view.labels), (k.stats, view.stats), (k.peaks, view.peaks)] {
        node.params.insert(key.into(), Value::Bool(b));
    }
}

/// A pinned display: the whole container, less a scrollbar while frozen.
fn render_display(spec: &Spec, inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>,
                  container: egui::Vec2, graph_ov: Option<&crate::canvas::node::PinGraphOverride>) {
    // Conditional vsync — see scope_should_request_repaint.
    let _ = scope_should_request_repaint(inner_id, snarl, ui.ctx());
    init_params(snarl, inner_id, spec);
    let avail = egui::vec2(container.x.max(40.0), container.y.max(24.0));
    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::hover());
    let Some(frame) = scope_frame(snarl, inner_id, spec) else { return };
    let (plot, bar) = if scrollable(&frame) {
        let (a, b) = rect.split_top_bottom_at_y(rect.bottom() - SCROLLBAR_H - 2.0);
        (a, Some(b.with_min_y(b.top() + 2.0)))
    } else {
        (rect, None)
    };
    paint_scope(ui, plot, &frame, graph_ov, resp.hover_pos());
    if let Some(bar) = bar {
        if let Some(ms) = scrollbar(ui, bar, &frame, ("pinned", spec.salt, inner_id)) {
            if let Some(node) = snarl.get_node_mut(inner_id) {
                node.params.insert(spec.keys.scroll.into(), serde_json::json!(ms));
            }
        }
    }
    request_repaint_throttled(ui.ctx());
}

/// A pinned first control row.
fn render_controls(spec: &Spec, inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    init_params(snarl, inner_id, spec);
    let Some(view) = snarl.get_node(inner_id).map(|n| spec.read(n)) else { return };
    let mut c = Controls::of(snarl, inner_id, spec, &view);
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(420.0, 22.0));
    let (fr, changed) = controls_row_ui(ui, &mut c, spec, false, Some(container));
    let n = if spec.freeze { 6 } else { 5 };
    publish_nav_field_rects(ui, inner_id, &fr[..n]);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            c.write(node, spec);
        }
    }
}

/// A pinned guides row.
fn render_grid_row(spec: &Spec, inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    init_params(snarl, inner_id, spec);
    let Some(mut view) = snarl.get_node(inner_id).map(|n| spec.read(n)) else { return };
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(380.0, 22.0));
    let (fr, changed) = grid_row_ui(ui, &mut view, false);
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            write_grid(node, spec, &view);
        }
    }
}

/// The node body: the plot (with its grip, and a scrollbar while frozen),
/// the two control rows, and the channel count.
fn show_body(spec: &Spec, node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    // Request vsync only while the input signal is animating (see
    // `scope_should_request_repaint` for the gate's logic).
    let _ = scope_should_request_repaint(node_id, snarl, ui.ctx());
    init_params(snarl, node_id, spec);
    let Some(frame) = scope_frame(snarl, node_id, spec) else { return };
    let mut view = frame.view;
    let n_inputs = inputs.len();

    // Size: what the grip set, else as wide as the control rows (measured last
    // frame) and DEFAULT_H tall. Never narrower than the rows.
    let rows_w_id = egui::Id::new(("scope_rows_w", spec.salt, node_id));
    let rows_w = ui.ctx().data(|d| d.get_temp::<f32>(rows_w_id));
    let stored = snarl.get_node(node_id).map(|n| {
        let get = |k: &str| n.params.get(k).and_then(|v| v.as_f64()).map(|v| v as f32);
        (get(spec.keys.w), get(spec.keys.h))
    }).unwrap_or((None, None));
    let min_w = rows_w.unwrap_or(MIN_W).max(MIN_W);
    let size = egui::vec2(
        stored.0.unwrap_or(rows_w.unwrap_or(FALLBACK_W)).max(min_w),
        stored.1.unwrap_or(DEFAULT_H).max(MIN_H),
    );

    let mut display_rect: Option<egui::Rect> = None;
    let mut new_size = None;
    let mut scroll_to = None;
    ui.vertical(|ui| {
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
        display_rect = Some(rect);
        paint_scope(ui, rect, &frame, None, resp.hover_pos());
        new_size = size_grip(ui, rect, (spec.salt, node_id));
        if scrollable(&frame) {
            let (bar, _) = ui.allocate_exact_size(egui::vec2(size.x, SCROLLBAR_H), egui::Sense::hover());
            scroll_to = scrollbar(ui, bar, &frame, ("body", spec.salt, node_id));
        }

        // ── Controls ─────────────────────────────────────────────────────────
        let mut c = Controls::of(snarl, node_id, spec, &view);
        let (ctrl_fr, changed) = controls_row_ui(ui, &mut c, spec, true, None);
        let ctrl_rect = ctrl_fr.iter().fold(egui::Rect::NOTHING, |a, r| a.union(*r));
        register_exposable_element(ui, node_id, "controls", ctrl_rect);

        // ── Guides / labels / statistics ─────────────────────────────────────
        let (grid_fr, grid_changed) = grid_row_ui(ui, &mut view, true);
        let grid_rect = grid_fr.iter().fold(egui::Rect::NOTHING, |a, r| a.union(*r));
        register_exposable_element(ui, node_id, "grid_row", grid_rect);
        let w = (ctrl_rect.right().max(grid_rect.right()) - rect.left()).max(MIN_W);
        if rows_w.is_none_or(|old| (old - w).abs() > 0.5) {
            ui.ctx().data_mut(|d| d.insert_temp(rows_w_id, w));
        }

        if changed || grid_changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                c.write(node, spec);
                write_grid(node, spec, &view);
            }
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ch").small().weak());
            if ui.small_button("+").on_hover_text("Add channel").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    // Names count data channels: the trigger is not one.
                    let next = node.inputs.len() + 1 - spec.col0;
                    node.inputs.push(PinDescriptor::new(format!("ch{}", next), SignalType::Float));
                }
            }
            // At least one data channel (plus the Trigger Scope's trigger).
            if n_inputs > spec.col0 + 1 && ui.small_button("−").on_hover_text("Remove channel").clicked() {
                remove_input_pin(node_id, n_inputs - 1, inputs, snarl);
            }
        });
    });
    if let Some(node) = snarl.get_node_mut(node_id) {
        if let Some(s) = new_size {
            node.params.insert(spec.keys.w.into(), serde_json::json!(s.x.max(min_w)));
            node.params.insert(spec.keys.h.into(), serde_json::json!(s.y.max(MIN_H)));
        }
        if let Some(ms) = scroll_to {
            node.params.insert(spec.keys.scroll.into(), serde_json::json!(ms));
        }
    }
    if let Some(r) = display_rect { register_exposable_element(ui, node_id, "display", r); }
}

// ── The two modules ──────────────────────────────────────────────────────────

pub(crate) fn show_oscilloscope_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    show_body(&OSC, node_id, inputs, ui, snarl);
}

pub(crate) fn render_oscilloscope_display(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>,
    container: egui::Vec2, graph_ov: Option<&crate::canvas::node::PinGraphOverride>) {
    render_display(&OSC, inner_id, ui, snarl, container, graph_ov);
}

pub(crate) fn render_oscilloscope_controls(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    render_controls(&OSC, inner_id, ui, snarl, container);
}

pub(crate) fn render_oscilloscope_grid_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    render_grid_row(&OSC, inner_id, ui, snarl, container);
}

pub(crate) fn show_trigscope_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    show_body(&TRIG, node_id, inputs, ui, snarl);
}

/// Bare trigger-scope display for sub-patch pinned layouts.
pub(crate) fn render_trigscope_display(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>,
    container: egui::Vec2, graph_ov: Option<&crate::canvas::node::PinGraphOverride>) {
    render_display(&TRIG, inner_id, ui, snarl, container, graph_ov);
}

/// Bare trigger-scope controls row for sub-patch pinned layouts.
pub(crate) fn render_trigscope_controls(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    render_controls(&TRIG, inner_id, ui, snarl, container);
}

pub(crate) fn render_trigscope_grid_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    render_grid_row(&TRIG, inner_id, ui, snarl, container);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> View {
        View {
            win_ms: 200.0, scale: 1.0, auto: false, uni: false,
            grid_x: 4, grid_y: 4, sub: 1, labels: true, stats: true, peaks: false, freeze: false,
            scroll_ms: 0.0, axis: None, stored: [None; 3],
        }
    }

    fn frame<'s>(spec: &'s Spec, view: View, units: &[Option<MotionUnit>], wired: &[bool]) -> Frame<'s> {
        Frame {
            spec,
            id: NodeId(0),
            view,
            rows: Rows::History(Default::default()),
            channels: units.iter().map(|u| ScopeChannel { unit: *u, name: String::new() }).collect(),
            wired: wired.to_vec(),
            n_channels: units.len() - spec.col0,
        }
    }

    #[test]
    fn the_axes_follow_the_pick_among_the_kinds_on_the_wires() {
        let g = Some(MotionUnit::Gyro);
        let a = Some(MotionUnit::Accel);
        let f = frame(&OSC, defaults(), &[a, g], &[true, true]);
        assert_eq!(f.present(), vec![Kind::Gyro, Kind::Accel]);
        assert_eq!(f.axes(), (Some(Kind::Gyro), Some(Kind::Accel)), "gyro first by default");
        let f = frame(&OSC, View { axis: Some(Kind::Accel), ..defaults() }, &[a, g], &[true, true]);
        assert_eq!(f.axes(), (Some(Kind::Accel), Some(Kind::Gyro)), "picked: accel left");
        let f = frame(&OSC, View { axis: Some(Kind::Accel), ..defaults() }, &[g, None], &[true, true]);
        assert_eq!(f.axes(), (Some(Kind::Gyro), Some(Kind::Value)), "a pick that isn't wired falls back");
        let f = frame(&OSC, defaults(), &[g, a], &[false, true]);
        assert_eq!(f.present(), vec![Kind::Accel], "an unwired channel doesn't count");
    }

    #[test]
    fn the_trigger_is_not_a_channel() {
        let g = Some(MotionUnit::Gyro);
        // Input 0 is the trigger: a gyro there puts nothing on the axes.
        let f = frame(&TRIG, defaults(), &[g, None], &[true, true]);
        assert_eq!((f.n_channels, f.present()), (1, vec![Kind::Value]));
        let f = frame(&TRIG, defaults(), &[None, g], &[true, true]);
        assert_eq!(f.present(), vec![Kind::Gyro]);
    }

    #[test]
    fn each_kind_keeps_its_own_scale_through_a_switch() {
        for spec in [&OSC, &TRIG] {
            let k = &spec.keys;
            let mut p = HashMap::new();
            p.insert(k.scale.to_string(), serde_json::json!(0.001));
            // First sight: the Scale param is the gyro's.
            sync_selected_scale(&mut p, k, Kind::Gyro);
            assert_eq!(p[k.scale_kind], "gyro");
            // Switch to accel (never set): the gyro's is parked.
            sync_selected_scale(&mut p, k, Kind::Accel);
            assert_eq!(p[k.kind_scales[0]], serde_json::json!(0.001));
            p.insert(k.scale.to_string(), serde_json::json!(0.25));
            // And back: each comes back as it was.
            sync_selected_scale(&mut p, k, Kind::Gyro);
            assert_eq!((p[k.scale].clone(), p[k.kind_scales[1]].clone()), (serde_json::json!(0.001), serde_json::json!(0.25)));
        }
    }

    #[test]
    fn auto_scale_rounds_to_round_guides_in_the_unit() {
        let mut view = defaults();
        view.auto = true;
        // 0.137 °/s peak, 4 value divisions (2 above zero): 0.1 °/s guides.
        let peak = 0.137 / 2000.0;
        let s = auto_scale(&view, peak, Kind::Gyro);
        assert!((s * 2000.0 - 0.2).abs() < 1e-5, "{}", s * 2000.0);
        // Without labels it is the bare peak.
        view.labels = false;
        assert_eq!(auto_scale(&view, peak, Kind::Gyro), peak);
    }

    #[test]
    fn statistics_cover_the_window() {
        let rows: Vec<Vec<Option<f32>>> = [1.0, 3.0, -1.0, 1.0].iter().map(|v| vec![Some(*v), None]).collect();
        let rows: Vec<&[Option<f32>]> = rows.iter().map(Vec::as_slice).collect();
        let s = channel_stats(&rows, 0);
        assert_eq!((s.n, s.now, s.min, s.max, s.mean), (4, 1.0, -1.0, 3.0, 1.0));
        assert!((s.sd - 2f32.sqrt()).abs() < 1e-6);
        assert_eq!(channel_stats(&rows, 1).n, 0);
    }

    /// The jiggle: rows must keep the same length, and every value its column,
    /// however the numbers change.
    #[test]
    fn the_statistics_table_never_moves() {
        let scale = 0.2 / 2000.0;
        let row = |now: f32, mean: f32| {
            let s = ChStats { n: 10, now: now / 2000.0, min: -0.15 / 2000.0, max: 0.31 / 2000.0, mean: mean / 2000.0, sd: 0.02 / 2000.0 };
            ("Gyro Z (yaw)".to_string(), Kind::Gyro, s, scale)
        };
        let a = stats_table(&[row(0.112, 0.119)], 200);
        let b = stats_table(&[row(-1.5, -0.004)], 200);
        assert_eq!(a.0, b.0, "same header");
        assert_eq!(a.1[0].chars().count(), b.1[0].chars().count(), "{:?} vs {:?}", a.1[0], b.1[0]);
        assert_eq!(a.0.chars().count() + 3, a.1[0].chars().count(), "header columns line up with the values (+ unit)");
        // Narrow scopes drop whole columns, never reflow on the values.
        let narrow = stats_table(&[row(0.112, 0.119)], 30);
        assert!(narrow.1[0].chars().count() <= 30, "{:?}", narrow.1[0]);
        assert!(!narrow.0.contains("mean"));
    }

    #[test]
    fn value_formats_follow_the_scale() {
        assert_eq!(value_format(Kind::Gyro, 0.2 / 2000.0), (3, 6));   // "+0.112", room for ±2 °/s
        assert_eq!(value_format(Kind::Accel, 0.02 / 8.0).0, 4);
        assert_eq!(value_format(Kind::Gyro, 1.0).0, 0);
    }

    #[test]
    fn a_frozen_view_scrolls_back_within_what_it_holds() {
        let rate = current_sample_rate() as f32;
        let mut f = frame(&OSC, View { freeze: true, win_ms: 100.0, ..defaults() }, &[None], &[true]);
        f.rows = Rows::History(std::sync::Arc::new((0..1000).map(|_| vec![Some(0.0)]).collect()));
        let win = (0.1 * rate) as usize;
        assert_eq!(view_span(&f), Span { total: 1000, win, start: 1000 - win });
        f.view.scroll_ms = 1e9; // far past the start: held at the oldest window
        assert_eq!(view_span(&f).start, 0);
        assert!(scrollable(&f));
        assert_eq!(scroll_ms_for(&f, 1000 - win), 0.0, "the newest window is no scroll");
        // Not frozen: no scrolling, whatever the param says.
        f.view.freeze = false;
        assert_eq!(view_span(&f).start, 1000 - win);
        assert!(!scrollable(&f));
    }

    #[test]
    fn a_trigger_scope_scrolls_back_before_its_trigger() {
        let rate = current_sample_rate() as f32;
        let win = (0.1 * rate) as usize;
        let cap = crate::canvas::node::TrigCapture { rows: (0..1000 + win).map(|_| vec![Some(0.0); 2]).collect(), t0: 1000 };
        let mut f = frame(&TRIG, View { win_ms: 100.0, ..defaults() }, &[None, None], &[true, true]);
        f.rows = Rows::Capture(std::sync::Arc::new(cap));
        assert_eq!(view_span(&f), Span { total: 1000 + win, win, start: 1000 }, "opens on the trigger");
        assert!(scrollable(&f));
        f.view.scroll_ms = 50.0; // the window starts 50 ms before the trigger
        assert_eq!(view_span(&f).start, 1000 - (0.05 * rate).round() as usize);
        assert!((scroll_ms_for(&f, 1000 - (0.05 * rate) as usize) - 50.0).abs() < 1.0);
        f.view.scroll_ms = 1e9;
        assert_eq!(view_span(&f).start, 0, "no further back than the lead-up");
        // Still filling: measured as if complete, so the view doesn't shift.
        let filling = crate::canvas::node::TrigCapture { rows: (0..1010).map(|_| vec![Some(0.0); 2]).collect(), t0: 1000 };
        f.rows = Rows::Capture(std::sync::Arc::new(filling));
        f.view.scroll_ms = 0.0;
        assert_eq!(view_span(&f), Span { total: 1000 + win, win, start: 1000 });
    }
}
