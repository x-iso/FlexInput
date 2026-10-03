//! Area Mapper geometry: an XY point (a stick or a touch point) laid onto
//! cells, as concentric RINGS (circle) or stacked ROWS (rectangle), each band
//! cut into its own cells.
//!
//! One model serves both shapes. A band is the space between two EDGES — radii
//! for a circle, heights for a rectangle — and each band carries its own CUTS —
//! angles for a circle, x positions for a rectangle. So a circle can be 4-way
//! near the centre and 8-way at the rim, and a rectangle can have five columns
//! in its middle row and three elsewhere. A circle has no seam at 12 o'clock:
//! its cuts wrap, and a cell may straddle the top.
//!
//! Every cell has a STABLE id that mapping cards bind to, kept across edits the
//! way Touch Zones keeps its zone ids: a split keeps the old id on the low side
//! and gives the new side a fresh one.
//!
//! Any border may be a GRADIENT: within `width` of it the two cells on either
//! side crossfade instead of switching, and a point near a corner of two
//! gradients splits its weight among up to four cells. The weights always sum
//! to 1.
//!
//! Symmetric editing mirrors across the vertical and horizontal axes (quarter
//! symmetry): a cut 30° right of up also sits 30° left of up and the same pair
//! around down. It is an editing constraint, not stored geometry — every
//! operation here takes `symmetric` and acts on the whole mirror set.
//!
//! A circle's ring border can be SQUARED: `square` takes the ring from a
//! circle (0) to a square (1) — a square deadzone inside round outer rings —
//! either by blending the round and the square distance, or (`corners`) as a
//! square whose corners are rounded off, the corner radius shrinking from the
//! whole circle to nothing. A rectangle's cell can be ROUNDED like a bubble:
//! `cell_round` swaps the cell's rectangle for a rounded one of the SAME AREA,
//! up to an ellipse (a circle for a square cell). Where shapes overlap (a
//! bubble bulges past its sides into its neighbours' rectangles, or two
//! bubbles meet), `cell_pressure` decides: the higher-pressure cell pushes
//! into the lower, and at equal pressure they share the overlap by their
//! gauges — so a bubble among plain cells rounds only as far as they let it,
//! a pressurised one becomes its whole circle (a round deadzone in a square
//! grid), equal bubbles meet in flat seams and rows cut differently settle
//! honeycomb-like. The gaps a bubble leaves (its old corners) go to the plain
//! cells around it — bubbles share a gap only among themselves. With nothing
//! rounded this is exactly the plain grid. The cuts and edges stay as the layout's skeleton (what is
//! dragged); a sector border stays a ray.
//!
//! Coordinates are centred, ±1 full scale, +Y up (the stick convention).
//! Circle: band coordinate = radius, cut coordinate = angle as a fraction of a
//! turn, clockwise from 12 o'clock. Rectangle: band coordinate = y (band 0 at
//! the bottom), cut coordinate = x.

use serde_json::{json, Value};

/// Smallest gap kept between two borders of one kind, in that border's units.
pub const MIN_GAP: f32 = 0.01;
/// Two border positions closer than this are the same border.
const SAME: f32 = 1e-3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Circle,
    Rect,
}

impl Shape {
    pub fn as_str(self) -> &'static str {
        match self {
            Shape::Circle => "circle",
            Shape::Rect => "rect",
        }
    }
    pub fn from_str(s: &str) -> Shape {
        if s == "rect" { Shape::Rect } else { Shape::Circle }
    }
}

/// How a gradient drives a DIGITAL output (a key or button) from a cell's
/// fractional weight. Analog outputs take the weight itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigitalMode {
    /// Fixed period, duty = weight.
    Pwm,
    /// A tap train whose frequency follows the weight (the Remapper's plain
    /// analog→digital conversion).
    Taps,
    /// Held while the weight is at or above `threshold`.
    Threshold,
}

impl DigitalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DigitalMode::Pwm => "pwm",
            DigitalMode::Taps => "taps",
            DigitalMode::Threshold => "threshold",
        }
    }
    pub fn from_str(s: &str) -> DigitalMode {
        match s {
            "taps" => DigitalMode::Taps,
            "threshold" => DigitalMode::Threshold,
            _ => DigitalMode::Pwm,
        }
    }
}

/// How the PWM of cells sharing a gradient lines up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Phase-locked: the cells take turns within one period, each for its
    /// weight's share, so two keys emulate one analog direction.
    Alternate,
    /// Each cell pulses on its own clock; pulses may overlap.
    Independent,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Alternate => "alternate",
            Phase::Independent => "independent",
        }
    }
    pub fn from_str(s: &str) -> Phase {
        if s == "independent" { Phase::Independent } else { Phase::Alternate }
    }
}

/// A gradient border's settings. They live on the border, not on a mapping
/// card, because they apply to every mapping in the cells it fades between.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    /// Full width of the crossfade band, in the border's own units (radius or
    /// height for an edge, turns or x for a cut), centred on the border.
    pub width: f32,
    /// Crossfade profile, 0..1 → 0..1 across the band (low side → high side).
    /// Empty = linear.
    pub curve: Vec<[f32; 2]>,
    pub digital: DigitalMode,
    /// PWM period, or the tap train's slowest period.
    pub period_ms: f32,
    /// Weight at which a Threshold output holds.
    pub threshold: f32,
    pub phase: Phase,
}

impl Gradient {
    /// A gradient with default settings and the given width.
    pub fn with_width(width: f32) -> Gradient {
        Gradient {
            width,
            curve: Vec::new(),
            digital: DigitalMode::Pwm,
            period_ms: 100.0,
            threshold: 0.5,
            phase: Phase::Alternate,
        }
    }

    pub fn to_value(&self) -> Value {
        let mut v = json!({
            "w": self.width,
            "dig": self.digital.as_str(),
            "period_ms": self.period_ms,
            "thr": self.threshold,
            "phase": self.phase.as_str(),
        });
        if !self.curve.is_empty() {
            v["curve"] = json!(self.curve);
        }
        v
    }

    pub fn from_value(v: &Value) -> Option<Gradient> {
        let width = v.get("w")?.as_f64()? as f32;
        let f = |k: &str, d: f32| v.get(k).and_then(|x| x.as_f64()).map(|x| x as f32).unwrap_or(d);
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
        Some(Gradient {
            width: width.max(MIN_GAP),
            curve: v.get("curve").and_then(|c| c.as_array()).map(|a| {
                a.iter().filter_map(|p| {
                    let q = p.as_array()?;
                    Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
                }).collect()
            }).unwrap_or_default(),
            digital: DigitalMode::from_str(s("dig")),
            period_ms: f("period_ms", 100.0).max(1.0),
            threshold: f("thr", 0.5).clamp(0.0, 1.0),
            phase: Phase::from_str(s("phase")),
        })
    }
}

/// A border: an edge between two bands, or a cut between two cells of a band.
#[derive(Debug, Clone, PartialEq)]
pub struct Border {
    pub pos: f32,
    pub gradient: Option<Gradient>,
    /// A circle's ring border only: 0 round … 1 square. `pos` is then the
    /// ring's size in its own norm — its radius along the axes.
    pub square: f32,
    /// How `square` morphs the ring: false blends the round and the square
    /// distance; true rounds a square's corners, radius `1 - square` of its
    /// half-size.
    pub corners: bool,
}

impl Border {
    pub fn hard(pos: f32) -> Border {
        Border { pos, gradient: None, square: 0.0, corners: false }
    }

    fn to_value(&self) -> Value {
        let mut v = json!({ "p": self.pos });
        if let Some(g) = &self.gradient {
            v["g"] = g.to_value();
        }
        if self.square > 0.0 {
            v["sq"] = json!(self.square);
        }
        if self.corners {
            v["rc"] = json!(true);
        }
        v
    }

    fn from_value(v: &Value) -> Option<Border> {
        Some(Border {
            pos: v.get("p")?.as_f64()? as f32,
            gradient: v.get("g").and_then(Gradient::from_value),
            square: v.get("sq").and_then(|x| x.as_f64()).map(|x| (x as f32).clamp(0.0, 1.0)).unwrap_or(0.0),
            corners: v.get("rc").and_then(|x| x.as_bool()).unwrap_or(false),
        })
    }

    /// This border's ring shape (circle edges).
    fn ring(&self) -> RingShape {
        RingShape { square: self.square, corners: self.corners }
    }
}

/// One ring (circle) or row (rectangle) and the cells it is cut into.
///
/// Circle: `cells.len() == max(cuts.len(), 1)`, and cell `i` runs clockwise
/// from cut `i` to cut `i + 1` (the last wraps to cut 0). A single cut makes no
/// second cell. Rectangle: `cells.len() == cuts.len() + 1`, and cell `i` runs
/// from cut `i - 1` (or the left side) to cut `i` (or the right side).
#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    pub cuts: Vec<Border>,
    pub cells: Vec<u32>,
}

/// Names a border for editing and for gradient lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BorderRef {
    /// The edge between band `i` and band `i + 1`.
    Edge(usize),
    /// Cut `cut` of band `band`.
    Cut { band: usize, cut: usize },
}

/// One cell's share of a point. `governor` names the gradient border whose
/// fade is deepest for this cell (the smallest of its factors) with that
/// factor — the border whose digital settings the cell's keys follow. `None`
/// when no gradient touches the point (the weight is then 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellWeight {
    pub band: usize,
    pub index: usize,
    pub id: u32,
    pub weight: f32,
    pub governor: Option<(BorderRef, f32)>,
}

/// One way an analog layer can press its keys (see
/// [`AreaLayout::analog_moves`]): a set of cells held together, for a share
/// of the time.
#[derive(Debug, Clone, PartialEq)]
pub struct AnalogMove {
    /// Stable within the band: 0 nothing, `1 + i` cell `i` alone, `1 + n + i`
    /// cells `i` and the next together (`n` = the band's cell count).
    pub slot: usize,
    pub ids: Vec<u32>,
    pub share: f32,
}

/// How an analog layer reads the stick's push past its deadzone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalogTune {
    /// The radius from which the push counts as full (the stable full-press
    /// zone lies beyond it).
    pub full: f32,
    /// The push (0..1 between the deadzone and `full`) that holds half the
    /// time; 0.5 is linear, lower lifts light pushes.
    pub mid: f32,
}

impl Default for AnalogTune {
    fn default() -> Self {
        AnalogTune { full: 1.0, mid: 0.5 }
    }
}

impl AnalogTune {
    /// The share of time a push at radius `r` holds, with deadzone `dz`.
    pub fn push(&self, r: f32, dz: f32) -> f32 {
        let full = self.full.max(dz + 0.02);
        let m = ((r - dz) / (full - dz)).clamp(0.0, 1.0);
        let mid = self.mid.clamp(0.05, 0.95);
        if (mid - 0.5).abs() < 1e-4 || m <= 0.0 {
            m
        } else {
            m.powf(0.5f32.ln() / mid.ln())
        }
    }
}

/// See `AreaLayout::analog_frame`.
struct AnalogFrame {
    band: usize,
    r: f32,
    m: f32,
    dirs: Vec<(usize, f32, f32)>,
}

/// The whole layout.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaLayout {
    pub shape: Shape,
    /// Band edges, ascending. `edges.len() + 1 == bands.len()`.
    pub edges: Vec<Border>,
    /// Bands, inner → outer (circle) or bottom → top (rectangle).
    pub bands: Vec<Band>,
    /// A rectangle only: per-cell corner rounding, `(cell id, 0 … 1)`, 1 being
    /// a radius of half the cell's narrower side. Cells not listed are square.
    pub cell_round: Vec<(u32, f32)>,
    /// A rectangle only: per-cell pressure, `(cell id, 0 … 1)` — where shapes
    /// overlap, the higher-pressure cell pushes into the lower. Cells not
    /// listed have 0.
    pub cell_pressure: Vec<(u32, f32)>,
}

/// A region of the layout to draw, as a patch parameterised over `(s, t)` in
/// the unit square — see [`AreaLayout::region_point`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Region {
    Cell { band: usize, index: usize },
    /// The crossfade band of an edge, `half` its half-width.
    EdgeBand { edge: usize, half: f32 },
    /// The crossfade band of a cut.
    CutBand { band: usize, cut: usize, half: f32 },
}

/// A circle ring's shape: how square, and how it gets there.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RingShape {
    square: f32,
    corners: bool,
}

impl RingShape {
    /// The ring's gauge of a point: the size a ring of this shape through the
    /// point has (its radius along the axes). Homogeneous, so a ring is the
    /// level set `norm == pos`.
    fn norm(self, x: f32, y: f32) -> f32 {
        let len = (x * x + y * y).sqrt();
        if self.corners {
            if len < 1e-9 {
                return 0.0;
            }
            len / rounded_square_radius(1.0 - self.square, x / len, y / len)
        } else {
            (1.0 - self.square) * len + self.square * x.abs().max(y.abs())
        }
    }

    /// [`norm`](Self::norm) of the unit direction at angle fraction `v`.
    fn norm_dir(self, v: f32) -> f32 {
        let (x, y) = dir_of(v);
        self.norm(x, y)
    }
}

/// How far the unit direction `(dx, dy)` reaches to the edge of a square of
/// half-size 1 whose corners are rounded with radius `c` (1 = a circle).
fn rounded_square_radius(c: f32, dx: f32, dy: f32) -> f32 {
    let (ax, ay) = { let (a, b) = (dx.abs(), dy.abs()); if a >= b { (a, b) } else { (b, a) } };
    let a = 1.0 - c;
    if ay <= a * ax {
        // The flat side x = 1.
        return 1.0 / ax.max(1e-9);
    }
    // The corner circle, centre (a, a), radius c: |t·d − (a, a)| = c.
    let b = a * (ax + ay);
    b + (b * b - 2.0 * a * a + c * c).max(0.0).sqrt()
}

/// The unit direction at angle fraction `v` (clockwise from 12 o'clock, +Y up).
fn dir_of(v: f32) -> (f32, f32) {
    let a = v * std::f32::consts::TAU;
    (a.sin(), a.cos())
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The cut coordinate's mirror images for one shape: each transform maps a cut
/// position to its image and says whether the image lives in the mirrored band
/// (rectangle up/down mirror) or the same band.
fn cut_transforms(shape: Shape) -> [(fn(f32) -> f32, bool); 4] {
    match shape {
        Shape::Circle => [
            (|a| a, false),
            (|a| wrap01(1.0 - a), false), // left/right
            (|a| wrap01(0.5 - a), false), // up/down
            (|a| wrap01(0.5 + a), false), // both
        ],
        Shape::Rect => [
            (|x| x, false),
            (|x| -x, false), // left/right
            (|x| x, true),   // up/down: same x, mirrored row
            (|x| -x, true),  // both
        ],
    }
}

/// A position into `[0, 1)`.
fn wrap01(a: f32) -> f32 {
    let w = a.rem_euclid(1.0);
    if w >= 1.0 { 0.0 } else { w }
}

/// Signed circular distance `a − b`, in `(-0.5, 0.5]` turns.
fn wrap_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(1.0);
    if d > 0.5 { d - 1.0 } else { d }
}

/// Set `value` (0 … 1, 0 = unlisted) for each of `ids` in a per-cell list.
fn set_per_cell(list: &mut Vec<(u32, f32)>, ids: &[u32], value: f32) {
    let value = value.clamp(0.0, 1.0);
    for id in ids {
        list.retain(|(c, _)| c != id);
        if value > 0.0 {
            list.push((*id, value));
        }
    }
    list.sort_by_key(|(c, _)| *c);
}

/// The `pos` range of a ring of squareness `square` that stays clear of the
/// rings `inner` and `outer` in EVERY direction: a ring's radius along a
/// direction is `pos / norm(direction)`, so containment is a per-direction
/// ratio. Both norms are symmetric under the eight mirror/rotation images of
/// the square, so directions from the axis to the diagonal cover them all.
fn ring_bounds(inner: Option<&Border>, outer: Option<&Border>, shape: RingShape) -> (f32, f32) {
    let (mut lo, mut hi) = (MIN_GAP, 1.0 - MIN_GAP);
    const STEPS: usize = 32;
    for k in 0..=STEPS {
        let v = k as f32 / STEPS as f32 / 8.0; // 0 … 45°
        let n = shape.norm_dir(v);
        if let Some(b) = inner {
            lo = lo.max(b.pos * n / b.ring().norm_dir(v) + MIN_GAP);
        }
        if let Some(b) = outer {
            hi = hi.min(b.pos * n / b.ring().norm_dir(v) - MIN_GAP);
        }
    }
    (lo, hi)
}

impl AreaLayout {
    /// The default layout for a shape: a circle is a centre plus a 4-way
    /// ring, a rectangle a 3×3 grid. Cells are numbered by direction — 0 the
    /// centre, 1 up, then clockwise in eighths to 8 up-left (a 4-way ring uses
    /// 1, 3, 5, 7) — so a card keeps its direction when the shape or preset is
    /// switched.
    pub fn default_for(shape: Shape) -> AreaLayout {
        match shape {
            Shape::Circle => AreaLayout::circle_ways(4),
            Shape::Rect => AreaLayout::rect_grid(),
        }
    }

    /// A circle: a centre (radius 0.3) and a ring of `n` sectors (`n` divides
    /// 8), the first centred on up. Cells are numbered by direction as in
    /// [`default_for`](Self::default_for).
    pub fn circle_ways(n: usize) -> AreaLayout {
        let n = n.clamp(1, 8);
        let step = (8 / n.max(1)).max(1) as u32;
        let cuts: Vec<Border> = (0..n).map(|k| Border::hard((k as f32 + 0.5) / n as f32)).collect();
        // Cell k starts at cut k, so it faces direction k + 1 (the last wraps
        // round to up).
        let cells: Vec<u32> = (0..n).map(|k| ((k + 1) % n) as u32 * step + 1).collect();
        AreaLayout {
            shape: Shape::Circle,
            edges: vec![Border::hard(0.3)],
            bands: vec![
                Band { cuts: Vec::new(), cells: vec![0] },
                Band { cuts, cells },
            ],
            cell_round: Vec::new(),
            cell_pressure: Vec::new(),
        }
    }

    /// A circle split into an inner disc (cell 0) and an outer ring (cell 1)
    /// at `radius` — the layer for a modifier held by how far, not which way.
    pub fn circle_rings(radius: f32) -> AreaLayout {
        AreaLayout {
            shape: Shape::Circle,
            edges: vec![Border::hard(radius.clamp(MIN_GAP, 1.0 - MIN_GAP))],
            bands: vec![
                Band { cuts: Vec::new(), cells: vec![0] },
                Band { cuts: Vec::new(), cells: vec![1] },
            ],
            cell_round: Vec::new(),
            cell_pressure: Vec::new(),
        }
    }

    /// The 3×3 rectangle, numbered by direction.
    pub fn rect_grid() -> AreaLayout {
        let third = 1.0 / 3.0;
        let row = |ids: [u32; 3]| Band {
            cuts: vec![Border::hard(-third), Border::hard(third)],
            cells: ids.to_vec(),
        };
        AreaLayout {
            shape: Shape::Rect,
            edges: vec![Border::hard(-third), Border::hard(third)],
            // Bottom row first.
            bands: vec![row([6, 5, 4]), row([7, 0, 3]), row([8, 1, 2])],
            cell_round: Vec::new(),
            cell_pressure: Vec::new(),
        }
    }

    // ── Serialisation ────────────────────────────────────────────────────────

    pub fn to_value(&self) -> Value {
        let mut v = json!({
            "shape": self.shape.as_str(),
            "edges": self.edges.iter().map(Border::to_value).collect::<Vec<_>>(),
            "bands": self.bands.iter().map(|b| json!({
                "cuts": b.cuts.iter().map(Border::to_value).collect::<Vec<_>>(),
                "cells": b.cells,
            })).collect::<Vec<_>>(),
        });
        for (key, list) in [("cell_round", &self.cell_round), ("cell_pressure", &self.cell_pressure)] {
            if !list.is_empty() {
                v[key] = Value::Object(list.iter().map(|(id, r)| (id.to_string(), json!(r))).collect());
            }
        }
        v
    }

    /// Parse [`to_value`](Self::to_value)'s output. `None` on malformed data,
    /// including counts that break the band/cell invariants.
    pub fn from_value(v: &Value) -> Option<AreaLayout> {
        let shape = Shape::from_str(v.get("shape")?.as_str()?);
        let edges: Vec<Border> = v.get("edges")?.as_array()?.iter()
            .map(Border::from_value).collect::<Option<_>>()?;
        let bands: Vec<Band> = v.get("bands")?.as_array()?.iter().map(|b| {
            Some(Band {
                cuts: b.get("cuts")?.as_array()?.iter().map(Border::from_value).collect::<Option<_>>()?,
                cells: b.get("cells")?.as_array()?.iter()
                    .map(|c| c.as_u64().map(|c| c as u32)).collect::<Option<_>>()?,
            })
        }).collect::<Option<_>>()?;
        let per_cell = |key: &str| -> Vec<(u32, f32)> {
            let mut list: Vec<(u32, f32)> = v.get(key).and_then(|o| o.as_object())
                .map(|o| o.iter().filter_map(|(k, r)| {
                    Some((k.parse::<u32>().ok()?, (r.as_f64()? as f32).clamp(0.0, 1.0)))
                }).filter(|(_, r)| *r > 0.0).collect())
                .unwrap_or_default();
            list.sort_by_key(|(id, _)| *id);
            list
        };
        let layout = AreaLayout {
            shape, edges, bands,
            cell_round: per_cell("cell_round"),
            cell_pressure: per_cell("cell_pressure"),
        };
        layout.is_well_formed().then_some(layout)
    }

    fn is_well_formed(&self) -> bool {
        if self.bands.len() != self.edges.len() + 1 {
            return false;
        }
        self.bands.iter().all(|b| b.cells.len() == self.cells_for(b.cuts.len()))
    }

    /// How many cells a band with `n_cuts` cuts holds.
    fn cells_for(&self, n_cuts: usize) -> usize {
        match self.shape {
            Shape::Circle => n_cuts.max(1),
            Shape::Rect => n_cuts + 1,
        }
    }

    // ── Queries ──────────────────────────────────────────────────────────────

    /// `(band coordinate, cut coordinate)` of a centred point (+Y up): the
    /// round radius and angle for a circle, y and x for a rectangle. A squared
    /// ring measures the point in its own norm instead — see
    /// [`band_of`](Self::band_of).
    pub fn coords(&self, x: f32, y: f32) -> (f32, f32) {
        match self.shape {
            Shape::Circle => {
                let tau = std::f32::consts::TAU;
                ((x * x + y * y).sqrt(), x.atan2(y).rem_euclid(tau) / tau)
            }
            Shape::Rect => (y.clamp(-1.0, 1.0), x.clamp(-1.0, 1.0)),
        }
    }

    /// The cut coordinate of a point: its angle fraction, or its x.
    pub fn cut_coord(&self, x: f32, y: f32) -> f32 {
        self.coords(x, y).1
    }

    /// A rectangle cell's corner rounding (0 … 1).
    pub fn cell_round_of(&self, id: u32) -> f32 {
        self.cell_round.iter().find(|(c, _)| *c == id).map(|(_, r)| *r).unwrap_or(0.0)
    }

    /// A rectangle cell's pressure (0 … 1).
    pub fn cell_pressure_of(&self, id: u32) -> f32 {
        self.cell_pressure.iter().find(|(c, _)| *c == id).map(|(_, r)| *r).unwrap_or(0.0)
    }

    /// A rectangle cell's bounds `[x0, x1, y0, y1]`.
    pub fn cell_bounds(&self, band: usize, index: usize) -> [f32; 4] {
        let [x0, x1] = self.cell_span(band, index);
        let [y0, y1] = self.band_span(band);
        [x0, x1, y0, y1]
    }

    /// Is a rectangle laid out as bubbles (any cell rounded)?
    pub fn is_bubbly(&self) -> bool {
        self.shape == Shape::Rect && self.cell_round.iter().any(|(_, r)| *r > 0.0)
    }

    /// How a rectangle cell measures a point: below 1 inside its shape, 1 on
    /// it. Its own rectangle when not rounded (so the plain grid is the
    /// smallest-measure partition exactly); rounded, a rounded rectangle of
    /// the same area — corner radius `round` of the half-size, 1 an ellipse.
    pub fn cell_gauge(&self, band: usize, index: usize, x: f32, y: f32) -> f32 {
        let [x0, x1, y0, y1] = self.cell_bounds(band, index);
        let (hw, hh) = (((x1 - x0) * 0.5).max(1e-6), ((y1 - y0) * 0.5).max(1e-6));
        let (qx, qy) = ((x - (x0 + x1) * 0.5) / hw, (y - (y0 + y1) * 0.5) / hh);
        let round = self.cell_round_of(self.bands[band].cells[index]);
        if round <= 0.0 {
            return qx.abs().max(qy.abs());
        }
        let len = (qx * qx + qy * qy).sqrt();
        if len < 1e-9 {
            return 0.0;
        }
        // Same area as the square of half-size 1: 4 − (4 − π)·round².
        let area = 4.0 - (4.0 - std::f32::consts::PI) * round * round;
        let k = (4.0 / area).sqrt();
        len / (k * rounded_square_radius(round, qx / len, qy / len))
    }

    /// The cell owning a point in a bubbly layout, leaving out `skip` (to ask
    /// who would own it instead): `(band, index, gauge)`.
    ///
    /// Inside one or more cells' own shapes (a plain cell's is its
    /// rectangle), the smallest gauge wins, each scaled down by its pressure —
    /// at full pressure a cell takes its whole shape. Outside every shape (the
    /// corners a bubble gives up), the plain cells around claim it before any
    /// bubble does, unless the nearest plain cell is far off (a gap among
    /// bubbles); a cell's pressure makes it a little less eager there.
    fn bubble_owner(&self, x: f32, y: f32, skip: Option<(usize, usize)>) -> Option<(usize, usize, f32)> {
        let (x, y) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0));
        let mut inside: Option<(usize, usize, f32, f32)> = None; // (b, i, g, key)
        let mut plain: Option<(usize, usize, f32, f32)> = None;
        let mut bubble: Option<(usize, usize, f32, f32)> = None;
        let keep = |slot: &mut Option<(usize, usize, f32, f32)>, c: (usize, usize, f32, f32)| {
            if slot.is_none_or(|o| c.3 < o.3 || (c.3 == o.3 && c.2 < o.2)) {
                *slot = Some(c);
            }
        };
        for (b, band) in self.bands.iter().enumerate() {
            for i in 0..band.cells.len() {
                if skip == Some((b, i)) {
                    continue;
                }
                let id = band.cells[i];
                let g = self.cell_gauge(b, i, x, y);
                let pressure = self.cell_pressure_of(id);
                if g <= 1.0 {
                    keep(&mut inside, (b, i, g, g * (1.0 - 0.9 * pressure)));
                } else if self.cell_round_of(id) > 0.0 {
                    keep(&mut bubble, (b, i, g, g * (1.0 + pressure)));
                } else {
                    keep(&mut plain, (b, i, g, g * (1.0 + pressure)));
                }
            }
        }
        const GAP_REACH: f32 = 1.5;
        let pick = inside.or(match (plain, bubble) {
            (Some(p), Some(q)) => Some(if p.3 <= q.3 * GAP_REACH { p } else { q }),
            (p, q) => p.or(q),
        });
        pick.map(|(b, i, g, _)| (b, i, g))
    }

    /// The border two rectangle cells meet across, if they share one: the cut
    /// between neighbours in a row, the edge between overlapping cells of
    /// neighbouring rows.
    fn border_between(&self, a: (usize, usize), b: (usize, usize)) -> Option<BorderRef> {
        let ((ba, ia), (bb, ib)) = (a, b);
        if ba == bb && ia.abs_diff(ib) == 1 {
            return Some(BorderRef::Cut { band: ba, cut: ia.min(ib) });
        }
        if ba.abs_diff(bb) == 1 {
            let [a0, a1] = self.cell_span(ba, ia);
            let [b0, b1] = self.cell_span(bb, ib);
            if a0.max(b0) < a1.min(b1) - SAME {
                return Some(BorderRef::Edge(ba.min(bb)));
            }
        }
        None
    }

    /// Bubble-mode weights: the owning cell, crossfaded with the one that
    /// would own the point without it, across their seam, when the border
    /// they share is a gradient. The seam is the nearest of: the owner's own
    /// shape edge, the runner-up's, or where their pressure-scaled gauges
    /// meet — each distance a gauge difference over its slope (exact for
    /// straight seams, close along curved ones).
    fn bubble_weights(&self, x: f32, y: f32, shape: &impl Fn(&Gradient, f32) -> f32) -> Vec<CellWeight> {
        let id = |b: usize, i: usize| self.bands[b].cells[i];
        let Some((bi, ii, gi)) = self.bubble_owner(x, y, None) else { return Vec::new() };
        let alone = vec![CellWeight { band: bi, index: ii, id: id(bi, ii), weight: 1.0, governor: None }];
        let Some((bj, ij, gj)) = self.bubble_owner(x, y, Some((bi, ii))) else { return alone };
        let Some(r) = self.border_between((bi, ii), (bj, ij)) else { return alone };
        let Some(g) = self.border(r).and_then(|b| b.gradient.as_ref()) else { return alone };
        const H: f32 = 1e-3;
        let slope = |b: usize, i: usize| {
            let dx = self.cell_gauge(b, i, x + H, y) - self.cell_gauge(b, i, x - H, y);
            let dy = self.cell_gauge(b, i, x, y + H) - self.cell_gauge(b, i, x, y - H);
            ((dx * dx + dy * dy).sqrt() / (2.0 * H)).max(1e-6)
        };
        let (si, sj) = (slope(bi, ii), slope(bj, ij));
        let mut delta = f32::INFINITY;
        if gi <= 1.0 {
            delta = delta.min((1.0 - gi) / si);
        }
        if gj > 1.0 {
            delta = delta.min((gj - 1.0) / sj);
        }
        if gi <= 1.0 && gj <= 1.0 {
            let (fi, fj) = (1.0 - 0.9 * self.cell_pressure_of(id(bi, ii)), 1.0 - 0.9 * self.cell_pressure_of(id(bj, ij)));
            delta = delta.min((gj * fj - gi * fi) / (si * fi + sj * fj));
        }
        let delta = delta.max(0.0);
        if delta >= g.width * 0.5 {
            return alone;
        }
        // Which side of the border is low (band below / cell before the cut).
        let i_low = match r {
            BorderRef::Edge(e) => bi == e,
            BorderRef::Cut { cut, .. } => ii == cut,
        };
        let t = if i_low { 0.5 - delta / g.width } else { 0.5 + delta / g.width };
        let f = shape(g, t).clamp(0.0, 1.0);
        let (low, high) = if i_low { ((bi, ii), (bj, ij)) } else { ((bj, ij), (bi, ii)) };
        let mut out = vec![
            CellWeight { band: low.0, index: low.1, id: id(low.0, low.1), weight: 1.0 - f, governor: Some((r, 1.0 - f)) },
            CellWeight { band: high.0, index: high.1, id: id(high.0, high.1), weight: f, governor: Some((r, f)) },
        ];
        out.retain(|c| c.weight > 0.0);
        out.sort_by_key(|c| (c.band, c.index));
        out
    }

    /// Shares for an ANALOG layer, ignoring borders and gradients: the stick's
    /// deflection past the centre — the centre disc's radius is the deadzone —
    /// projected onto each direction cell of the band the point is in, so a
    /// 4-way ring gives up / right / down / left exactly the stick's axes
    /// (W at 0.71 and D at 0.71 on a full diagonal). Inside the deadzone the
    /// centre cell takes it all. Meant for one key per direction cell.
    pub fn analog_shares(&self, x: f32, y: f32, tune: &AnalogTune) -> Vec<CellWeight> {
        let f = match self.analog_frame(x, y, tune) {
            Ok(f) => f,
            Err((band, index)) => return vec![self.whole_weight(band, index)],
        };
        let (ux, uy) = (x / f.r, y / f.r);
        let out: Vec<CellWeight> = f.dirs.iter().filter_map(|&(i, dx, dy)| {
            let share = (dx * ux + dy * uy).clamp(0.0, 1.0) * f.m;
            (share > 1e-4).then(|| CellWeight {
                band: f.band, index: i, id: self.bands[f.band].cells[i], weight: share, governor: None,
            })
        }).collect();
        if out.is_empty() {
            let (band, index) = self.locate(x, y);
            return vec![self.whole_weight(band, index)];
        }
        out
    }

    fn whole_weight(&self, band: usize, index: usize) -> CellWeight {
        CellWeight { band, index, id: self.bands[band].cells[index], weight: 1.0, governor: None }
    }

    /// What an analog layer reads off a point: its band's direction cells
    /// (index and unit direction) and the deflection past the deadzone. `Err`
    /// holds the cell that takes the point whole: the deadzone's centre disc,
    /// or a band without directions.
    fn analog_frame(&self, x: f32, y: f32, tune: &AnalogTune) -> Result<AnalogFrame, (usize, usize)> {
        let (band, index) = self.locate(x, y);
        let r = (x * x + y * y).sqrt();
        let centre_disc = self.has_centre_disc();
        if (centre_disc && band == 0) || r < 1e-6 {
            return Err((band, index));
        }
        let dz = if centre_disc { self.edges[0].pos } else { 0.0 };
        let m = tune.push(r, dz);
        let dirs: Vec<(usize, f32, f32)> = (0..self.bands[band].cells.len()).filter_map(|i| {
            self.analog_dir(band, i).map(|(dx, dy)| (i, dx, dy))
        }).collect();
        if dirs.is_empty() {
            return Err((band, index));
        }
        Ok(AnalogFrame { band, r, m, dirs })
    }

    /// Is band 0 a whole centre disc (an analog layer's deadzone)?
    pub fn has_centre_disc(&self) -> bool {
        self.shape == Shape::Circle && !self.edges.is_empty() && self.bands[0].cuts.len() <= 1
    }

    /// The unit direction a cell stands for in an analog layer: a sector's
    /// middle, a rectangle cell's centre. `None` for a whole ring or the
    /// rectangle's middle cell, which point nowhere.
    pub fn analog_dir(&self, band: usize, index: usize) -> Option<(f32, f32)> {
        match self.shape {
            Shape::Circle => {
                let [v0, v1] = self.cell_span(band, index);
                (v1 - v0 < 0.999).then(|| dir_of((v0 + v1) * 0.5))
            }
            Shape::Rect => {
                let [x0, x1, y0, y1] = self.cell_bounds(band, index);
                let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
                let len = (cx * cx + cy * cy).sqrt();
                (len >= 1e-6).then(|| (cx / len, cy / len))
            }
        }
    }

    /// An analog layer's target for a point when it moves by DIRECTION, for
    /// games that walk the same speed every way: the stick's angle falls
    /// between two of the moves its band offers — each direction cell alone,
    /// and two neighbouring cells about a right angle apart together (W + D is
    /// the diagonal) — split between them by how close it is to each; the
    /// deflection past the deadzone is how much of the time either holds, and
    /// the rest is nothing pressed. So a full push never stops the walk, only
    /// steers it. A cell that takes the point whole holds alone. Returns the
    /// band and its moves, nothing (slot 0) last when it has a share.
    pub fn analog_moves(&self, x: f32, y: f32, tune: &AnalogTune) -> (usize, Vec<AnalogMove>) {
        let f = match self.analog_frame(x, y, tune) {
            Ok(f) => f,
            Err((band, index)) => {
                let whole = AnalogMove { slot: 1 + index, ids: vec![self.bands[band].cells[index]], share: 1.0 };
                return (band, vec![whole]);
            }
        };
        let cells = &self.bands[f.band].cells;
        let n = cells.len();
        let turn = |dx: f32, dy: f32| dx.atan2(dy).rem_euclid(std::f32::consts::TAU);
        let mut dirs: Vec<(usize, f32, f32, f32)> = f.dirs.iter().map(|&(i, dx, dy)| (i, dx, dy, turn(dx, dy))).collect();
        dirs.sort_by(|a, b| a.3.total_cmp(&b.3));
        // Each candidate: (slot, ids, angle).
        let mut cands: Vec<(usize, Vec<u32>, f32)> = dirs.iter().map(|d| (1 + d.0, vec![cells[d.0]], d.3)).collect();
        if dirs.len() >= 2 {
            for k in 0..dirs.len() {
                let (a, b) = (dirs[k], dirs[(k + 1) % dirs.len()]);
                let gap = (b.3 - a.3).rem_euclid(std::f32::consts::TAU).to_degrees();
                if (70.0..=110.0).contains(&gap) {
                    cands.push((1 + n + a.0, vec![cells[a.0], cells[b.0]], turn(a.1 + b.1, a.2 + b.2)));
                }
            }
        }
        cands.sort_by(|a, b| a.2.total_cmp(&b.2));
        let phi = turn(x, y);
        // The candidates either side of the stick's angle.
        let k = cands.iter().rposition(|c| c.2 <= phi).unwrap_or(cands.len() - 1);
        let (a, b) = (&cands[k], &cands[(k + 1) % cands.len()]);
        let gap = (b.2 - a.2).rem_euclid(std::f32::consts::TAU);
        let from_a = (phi - a.2).rem_euclid(std::f32::consts::TAU);
        let mut moves = Vec::new();
        if cands.len() >= 2 && gap.to_degrees() <= 135.0 {
            let t = (from_a / gap.max(1e-6)).clamp(0.0, 1.0);
            moves.push(AnalogMove { slot: a.0, ids: a.1.clone(), share: f.m * (1.0 - t) });
            moves.push(AnalogMove { slot: b.0, ids: b.1.clone(), share: f.m * t });
        } else {
            // Too few directions to steer between: the nearer one, by how
            // much the stick points along it.
            let from_b = (b.2 - phi).rem_euclid(std::f32::consts::TAU);
            let (c, off) = if from_a <= from_b { (a, from_a) } else { (b, from_b) };
            moves.push(AnalogMove { slot: c.0, ids: c.1.clone(), share: f.m * off.cos().max(0.0) });
        }
        moves.retain(|m| m.share > 1e-4);
        let held: f32 = moves.iter().map(|m| m.share).sum();
        if held < 1.0 - 1e-4 {
            moves.push(AnalogMove { slot: 0, ids: Vec::new(), share: 1.0 - held });
        }
        (f.band, moves)
    }

    /// The most move slots [`Self::analog_moves`] can use for one band.
    pub fn analog_slot_count(&self) -> usize {
        1 + 2 * self.bands.iter().map(|b| b.cells.len()).max().unwrap_or(1)
    }

    /// The border two cells (by id) of a rectangle meet across, if they share one.
    pub fn border_between_ids(&self, a: u32, b: u32) -> Option<BorderRef> {
        self.border_between(self.find_cell(a)?, self.find_cell(b)?)
    }

    /// Cell `id` and, when `symmetric`, the cells holding its mirrored centre.
    fn cell_and_mirrors(&self, id: u32, symmetric: bool) -> Vec<u32> {
        let Some((band, index)) = self.find_cell(id) else { return Vec::new() };
        let mut ids = vec![id];
        if symmetric {
            let [x0, x1, y0, y1] = self.cell_bounds(band, index);
            let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
            for (mx, my) in [(-cx, cy), (cx, -cy), (-cx, -cy)] {
                let b = self.band_at(my);
                let m = self.bands[b].cells[self.cell_index_in(b, mx)];
                if !ids.contains(&m) { ids.push(m); }
            }
        }
        ids
    }

    /// Set a cell's pressure (0 … 1), and its mirror images' when `symmetric`.
    pub fn set_cell_pressure(&mut self, id: u32, pressure: f32, symmetric: bool) {
        if self.shape != Shape::Rect {
            return;
        }
        let ids = self.cell_and_mirrors(id, symmetric);
        set_per_cell(&mut self.cell_pressure, &ids, pressure);
    }

    /// Round rectangle cell `id` into a bubble by `round` (0 … 1), and its
    /// mirror images' when `symmetric`.
    pub fn set_cell_round(&mut self, id: u32, round: f32, symmetric: bool) {
        if self.shape != Shape::Rect {
            return;
        }
        let ids = self.cell_and_mirrors(id, symmetric);
        set_per_cell(&mut self.cell_round, &ids, round);
    }

    /// Does a squared ring bend the plain wedges? (A rectangle's rounded
    /// cells are drawn over the plain grid.)
    pub fn is_curved(&self) -> bool {
        self.shape == Shape::Circle && self.edges.iter().any(|e| e.square > 1e-4)
    }

    /// Where a model-space point lies relative to edge `e`, in the edge's own
    /// units — compared against its `pos`.
    fn edge_coord(&self, e: usize, mx: f32, my: f32) -> f32 {
        match self.shape {
            Shape::Circle => self.edges[e].ring().norm(mx, my),
            Shape::Rect => my.clamp(-1.0, 1.0),
        }
    }

    fn band_of_model(&self, mx: f32, my: f32) -> usize {
        self.edges.iter().enumerate().filter(|(e, b)| self.edge_coord(*e, mx, my) >= b.pos).count()
    }

    /// The band a centred point falls in, each ring measured in its own norm.
    pub fn band_of(&self, x: f32, y: f32) -> usize {
        self.band_of_model(x, y)
    }

    /// The radius of circle edge `e` along angle fraction `v`.
    pub fn edge_radius_at(&self, e: usize, v: f32) -> f32 {
        let b = &self.edges[e];
        b.pos / b.ring().norm_dir(v).max(1e-6)
    }

    /// The span of `band` along angle fraction `v` (circle; a rectangle's band
    /// is the same everywhere — [`band_span`](Self::band_span)). The outermost
    /// ring is drawn out to the unit circle, or a little past its inner ring
    /// where a squared ring's corner reaches beyond it.
    pub fn band_span_at(&self, band: usize, v: f32) -> [f32; 2] {
        if self.shape == Shape::Rect {
            return self.band_span(band);
        }
        let lo = band.checked_sub(1).map(|e| self.edge_radius_at(e, v)).unwrap_or(0.0);
        let hi = if band < self.edges.len() { self.edge_radius_at(band, v) } else { 1.0f32.max(lo + 0.08) };
        [lo, hi]
    }

    /// A point of a region, on the FIELD (display space): `s` runs along the
    /// cut coordinate (angle / x), `t` along the band coordinate (radius / y).
    pub fn region_point(&self, region: Region, s: f32, t: f32) -> [f32; 2] {
        match self.shape {
            Shape::Circle => {
                let (v, lo, hi) = match region {
                    Region::Cell { band, index } => {
                        let [v0, v1] = self.cell_span(band, index);
                        let v = lerp(v0, v1, s);
                        let [lo, hi] = self.band_span_at(band, v);
                        (v, lo, hi)
                    }
                    Region::EdgeBand { edge, half } => {
                        let b = &self.edges[edge];
                        let n = b.ring().norm_dir(s).max(1e-6);
                        (s, (b.pos - half).max(0.0) / n, (b.pos + half) / n)
                    }
                    Region::CutBand { band, cut, half } => {
                        let pos = self.bands[band].cuts[cut].pos;
                        let v = lerp(pos - half, pos + half, s);
                        let [lo, hi] = self.band_span_at(band, v);
                        (v, lo, hi)
                    }
                };
                let (dx, dy) = dir_of(v);
                let r = lerp(lo, hi, t);
                [dx * r, dy * r]
            }
            Shape::Rect => {
                let (mx, my) = match region {
                    Region::Cell { band, index } => {
                        let [v0, v1] = self.cell_span(band, index);
                        let [lo, hi] = self.band_span(band);
                        (lerp(v0, v1, s), lerp(lo, hi, t))
                    }
                    Region::EdgeBand { edge, half } => {
                        let pos = self.edges[edge].pos;
                        (lerp(-1.0, 1.0, s), lerp((pos - half).max(-1.0), (pos + half).min(1.0), t))
                    }
                    Region::CutBand { band, cut, half } => {
                        let pos = self.bands[band].cuts[cut].pos;
                        let [lo, hi] = self.band_span(band);
                        (lerp((pos - half).max(-1.0), (pos + half).min(1.0), s), lerp(lo, hi, t))
                    }
                };
                [mx, my]
            }
        }
    }

    /// A point along a border, on the field: `s` runs the border's length —
    /// once round a ring (from 12 o'clock), across a row (left to right), along
    /// a cut (inner to outer, bottom to top).
    pub fn border_point(&self, r: BorderRef, s: f32) -> [f32; 2] {
        match (self.shape, r) {
            (Shape::Circle, BorderRef::Edge(e)) => {
                let (dx, dy) = dir_of(s);
                let rad = self.edge_radius_at(e, s);
                [dx * rad, dy * rad]
            }
            (Shape::Circle, BorderRef::Cut { band, cut }) => {
                let v = self.bands[band].cuts[cut].pos;
                let [lo, hi] = self.band_span_at(band, v);
                let (dx, dy) = dir_of(v);
                let rad = lerp(lo, hi, s);
                [dx * rad, dy * rad]
            }
            (Shape::Rect, BorderRef::Edge(e)) => [lerp(-1.0, 1.0, s), self.edges[e].pos],
            (Shape::Rect, BorderRef::Cut { band, cut }) => {
                let [lo, hi] = self.band_span(band);
                [self.bands[band].cuts[cut].pos, lerp(lo, hi, s)]
            }
        }
    }

    /// The position border `r` would take to pass through a field point — what
    /// a drag moves it to.
    pub fn border_coord_of(&self, r: BorderRef, x: f32, y: f32) -> f32 {
        match r {
            BorderRef::Edge(e) => self.edge_coord(e, x, y),
            BorderRef::Cut { .. } => self.cut_coord(x, y),
        }
    }

    /// The band holding band coordinate `u`.
    pub fn band_at(&self, u: f32) -> usize {
        self.edges.iter().take_while(|e| e.pos <= u).count()
    }

    /// The index of the cell of `band` holding cut coordinate `v`.
    pub fn cell_index_in(&self, band: usize, v: f32) -> usize {
        let cuts = &self.bands[band].cuts;
        let below = cuts.iter().take_while(|c| c.pos <= v).count();
        match self.shape {
            Shape::Circle if cuts.len() <= 1 => 0,
            // Before the first cut is the wrap cell, which starts at the last.
            Shape::Circle => if below == 0 { cuts.len() - 1 } else { below - 1 },
            Shape::Rect => below,
        }
    }

    /// The cell a centred point falls in, ignoring gradients: `(band, index)`.
    pub fn locate(&self, x: f32, y: f32) -> (usize, usize) {
        if self.is_bubbly() {
            if let Some((b, i, _)) = self.bubble_owner(x, y, None) {
                return (b, i);
            }
        }
        let band = self.band_of(x, y);
        (band, self.cell_index_in(band, self.cut_coord(x, y)))
    }

    /// The id of the cell a centred point falls in, ignoring gradients.
    pub fn locate_id(&self, x: f32, y: f32) -> u32 {
        let (band, idx) = self.locate(x, y);
        self.bands[band].cells[idx]
    }

    /// Every cell sharing the point, with its weight. `shape` turns a
    /// gradient's linear position across its band (0 = low side, 1 = high
    /// side) into the high side's share — it is where the gradient's curve
    /// goes; pass `|_, t| t` for linear. Weights sum to 1; cells with no share
    /// are left out. Ordered by band, then cell index.
    pub fn weights(&self, x: f32, y: f32, shape: impl Fn(&Gradient, f32) -> f32) -> Vec<CellWeight> {
        if self.is_bubbly() {
            return self.bubble_weights(x, y, &shape);
        }
        let (mx, my) = (x, y);
        let v = self.cut_coord(mx, my);
        let mut out = Vec::new();
        for (band, bw, bgov) in self.band_weights(mx, my, &shape) {
            for (index, cw, cgov) in self.cut_weights(band, v, &shape) {
                let weight = bw * cw;
                if weight <= 0.0 {
                    continue;
                }
                let governor = match (bgov, cgov) {
                    (Some(a), Some(b)) => Some(if a.1 <= b.1 { a } else { b }),
                    (a, b) => a.or(b),
                };
                out.push(CellWeight { band, index, id: self.bands[band].cells[index], weight, governor });
            }
        }
        out
    }

    /// The bands sharing a model-space point: `(band, weight, governor)`.
    /// Each edge measures the point in its own units, so a squared ring's
    /// crossfade follows its shape.
    fn band_weights(
        &self,
        mx: f32,
        my: f32,
        shape: &impl Fn(&Gradient, f32) -> f32,
    ) -> Vec<(usize, f32, Option<(BorderRef, f32)>)> {
        let k = self.band_of_model(mx, my);
        let near = [k.checked_sub(1), (k < self.edges.len()).then_some(k)];
        let hit = near.into_iter().flatten()
            .filter_map(|e| {
                let g = self.edges[e].gradient.as_ref()?;
                let d = self.edge_coord(e, mx, my) - self.edges[e].pos;
                (d.abs() < g.width * 0.5).then_some((e, d, g))
            })
            .min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()));
        match hit {
            None => vec![(k, 1.0, None)],
            Some((e, d, g)) => {
                let f = shape(g, (d + g.width * 0.5) / g.width).clamp(0.0, 1.0);
                let r = BorderRef::Edge(e);
                vec![(e, 1.0 - f, Some((r, 1.0 - f))), (e + 1, f, Some((r, f)))]
            }
        }
    }

    /// The cells of `band` sharing cut coordinate `v`: `(index, weight, governor)`.
    fn cut_weights(
        &self,
        band: usize,
        v: f32,
        shape: &impl Fn(&Gradient, f32) -> f32,
    ) -> Vec<(usize, f32, Option<(BorderRef, f32)>)> {
        let cuts = &self.bands[band].cuts;
        let n = cuts.len();
        let i = self.cell_index_in(band, v);
        // The cuts bounding cell `i`, and for each the cells on its low and
        // high side.
        let candidates: Vec<(usize, usize, usize)> = match self.shape {
            Shape::Circle if n <= 1 => Vec::new(),
            Shape::Circle => vec![(i, (i + n - 1) % n, i), ((i + 1) % n, i, (i + 1) % n)],
            Shape::Rect => {
                let mut c = Vec::new();
                if i > 0 { c.push((i - 1, i - 1, i)); }
                if i < n { c.push((i, i, i + 1)); }
                c
            }
        };
        let hit = candidates.into_iter()
            .filter_map(|(j, lo, hi)| {
                let g = cuts[j].gradient.as_ref()?;
                let d = match self.shape {
                    Shape::Circle => wrap_diff(v, cuts[j].pos),
                    Shape::Rect => v - cuts[j].pos,
                };
                (d.abs() < g.width * 0.5).then_some((j, lo, hi, d, g))
            })
            .min_by(|a, b| a.3.abs().total_cmp(&b.3.abs()));
        match hit {
            None => vec![(i, 1.0, None)],
            Some((j, lo, hi, d, g)) => {
                let f = shape(g, (d + g.width * 0.5) / g.width).clamp(0.0, 1.0);
                let r = BorderRef::Cut { band, cut: j };
                vec![(lo, 1.0 - f, Some((r, 1.0 - f))), (hi, f, Some((r, f)))]
            }
        }
    }

    /// The border a [`BorderRef`] names, if it exists.
    pub fn border(&self, r: BorderRef) -> Option<&Border> {
        match r {
            BorderRef::Edge(e) => self.edges.get(e),
            BorderRef::Cut { band, cut } => self.bands.get(band)?.cuts.get(cut),
        }
    }

    fn border_mut(&mut self, r: BorderRef) -> Option<&mut Border> {
        match r {
            BorderRef::Edge(e) => self.edges.get_mut(e),
            BorderRef::Cut { band, cut } => self.bands.get_mut(band)?.cuts.get_mut(cut),
        }
    }

    /// Every cell id, band by band.
    pub fn cell_ids(&self) -> Vec<u32> {
        self.bands.iter().flat_map(|b| b.cells.iter().copied()).collect()
    }

    /// Where a cell id sits: `(band, index)`.
    pub fn find_cell(&self, id: u32) -> Option<(usize, usize)> {
        self.bands.iter().enumerate()
            .find_map(|(b, band)| band.cells.iter().position(|c| *c == id).map(|i| (b, i)))
    }

    /// Next free cell id.
    pub fn next_id(&self) -> u32 {
        self.cell_ids().into_iter().max().map(|m| m + 1).unwrap_or(0)
    }

    /// The span of band `band` along the band coordinate: `[lo, hi]`. The
    /// outermost circle ring has no outer edge; it reports 1.
    pub fn band_span(&self, band: usize) -> [f32; 2] {
        let (lo_default, hi_default) = match self.shape {
            Shape::Circle => (0.0, 1.0),
            Shape::Rect => (-1.0, 1.0),
        };
        let lo = band.checked_sub(1).map(|e| self.edges[e].pos).unwrap_or(lo_default);
        let hi = self.edges.get(band).map(|e| e.pos).unwrap_or(hi_default);
        [lo, hi]
    }

    /// The span of cell `index` of `band` along the cut coordinate: `[lo, hi]`.
    /// A circle cell may wrap, so `hi` can exceed 1; a full ring is `[0, 1]`.
    pub fn cell_span(&self, band: usize, index: usize) -> [f32; 2] {
        let cuts = &self.bands[band].cuts;
        match self.shape {
            Shape::Circle => {
                let n = cuts.len();
                if n <= 1 {
                    let a = cuts.first().map(|c| c.pos).unwrap_or(0.0);
                    return [a, a + 1.0];
                }
                let lo = cuts[index].pos;
                let mut hi = cuts[(index + 1) % n].pos;
                if hi <= lo { hi += 1.0; }
                [lo, hi]
            }
            Shape::Rect => {
                let lo = index.checked_sub(1).map(|j| cuts[j].pos).unwrap_or(-1.0);
                let hi = cuts.get(index).map(|c| c.pos).unwrap_or(1.0);
                [lo, hi]
            }
        }
    }

    // ── Symmetry ─────────────────────────────────────────────────────────────

    /// The band mirrored up/down: a rectangle's rows mirror about the middle;
    /// a circle's rings are their own mirror images.
    pub fn mirror_band(&self, band: usize) -> usize {
        match self.shape {
            Shape::Circle => band,
            Shape::Rect => self.bands.len() - 1 - band,
        }
    }

    /// Positions (and bands) a cut at `pos` in `band` mirrors to, itself
    /// first, without duplicates.
    pub fn cut_mirror_targets(&self, band: usize, pos: f32) -> Vec<(usize, f32)> {
        let mut out: Vec<(usize, f32)> = Vec::new();
        for (f, other_band) in cut_transforms(self.shape) {
            let b = if other_band { self.mirror_band(band) } else { band };
            let p = f(pos);
            if !out.iter().any(|(ob, op)| *ob == b && self.same_cut_pos(*op, p)) {
                out.push((b, p));
            }
        }
        out
    }

    fn same_cut_pos(&self, a: f32, b: f32) -> bool {
        match self.shape {
            Shape::Circle => wrap_diff(a, b).abs() < SAME,
            Shape::Rect => (a - b).abs() < SAME,
        }
    }

    /// The cut of `band` at `pos`, if one sits there.
    pub fn find_cut(&self, band: usize, pos: f32) -> Option<usize> {
        self.bands.get(band)?.cuts.iter().position(|c| self.same_cut_pos(c.pos, pos))
    }

    /// Is `band` a circle ring split by one diameter (two opposite cuts)?
    /// Symmetric editing keeps such a split a diameter and turns it whole.
    pub fn is_diameter(&self, band: usize) -> bool {
        self.shape == Shape::Circle
            && self.bands.get(band).is_some_and(|b| {
                b.cuts.len() == 2 && (wrap_diff(b.cuts[0].pos, b.cuts[1].pos).abs() - 0.5).abs() < SAME
            })
    }

    /// Every border mirroring `r` (itself first) that exists in the layout.
    pub fn mirror_set(&self, r: BorderRef) -> Vec<BorderRef> {
        if let BorderRef::Cut { band, cut } = r {
            if self.is_diameter(band) {
                return vec![r, BorderRef::Cut { band, cut: 1 - cut.min(1) }];
            }
        }
        match r {
            BorderRef::Edge(e) => {
                let mut out = vec![r];
                if self.shape == Shape::Rect {
                    let m = self.edges.len() - 1 - e;
                    if m != e && (self.edges[m].pos + self.edges[e].pos).abs() < SAME {
                        out.push(BorderRef::Edge(m));
                    }
                }
                out
            }
            BorderRef::Cut { band, cut } => {
                let Some(pos) = self.bands.get(band).and_then(|b| b.cuts.get(cut)).map(|c| c.pos) else {
                    return vec![r];
                };
                let mut out = Vec::new();
                for (b, p) in self.cut_mirror_targets(band, pos) {
                    if let Some(j) = self.find_cut(b, p) {
                        let m = BorderRef::Cut { band: b, cut: j };
                        if !out.contains(&m) { out.push(m); }
                    }
                }
                out
            }
        }
    }

    /// Does a border sit on a mirror axis (its own mirror image under some
    /// non-identity reflection)? Such a border can't move without changing how
    /// many borders its mirror set has, so symmetric editing pins it.
    pub fn on_axis(&self, r: BorderRef) -> bool {
        let Some(pos) = self.border(r).map(|b| b.pos) else { return false };
        if matches!(r, BorderRef::Cut { band, .. } if self.is_diameter(band)) {
            return false; // a diameter turns freely
        }
        match r {
            BorderRef::Edge(_) => self.shape == Shape::Rect && pos.abs() < SAME,
            BorderRef::Cut { .. } => match self.shape {
                Shape::Circle => [0.0f32, 0.25, 0.5, 0.75].iter().any(|ax| wrap_diff(pos, *ax).abs() < SAME),
                Shape::Rect => pos.abs() < SAME,
            },
        }
    }

    /// Is every border's mirror image present? (Symmetric editing assumes it;
    /// a layout edited with symmetry off may not be.)
    pub fn is_symmetric(&self) -> bool {
        let edges_ok = self.shape == Shape::Circle
            || self.edges.iter().all(|e| self.edges.iter().any(|o| (o.pos + e.pos).abs() < SAME));
        let cuts_ok = self.bands.iter().enumerate().all(|(b, band)| {
            self.is_diameter(b) || band.cuts.iter().all(|c| {
                self.cut_mirror_targets(b, c.pos).into_iter().all(|(mb, mp)| self.find_cut(mb, mp).is_some())
            })
        });
        let rows_ok = self.shape == Shape::Circle
            || (0..self.bands.len()).all(|b| {
                self.bands[b].cuts.len() == self.bands[self.mirror_band(b)].cuts.len()
            });
        edges_ok && cuts_ok && rows_ok
    }

    // ── Editing ──────────────────────────────────────────────────────────────

    /// Add a cut to `band` at `pos` (and its mirror images when `symmetric`).
    /// The cell it lands in keeps its id on the low side; each new cell gets
    /// a fresh id. Positions too close to an existing cut are skipped. Returns
    /// the new cell ids.
    ///
    /// A whole circle ring has nothing to cut between, so its first cut is a
    /// diameter, halving it. Under symmetry the next one makes four: the
    /// diameter's mirror image (or, on an axis, the diameter across it).
    pub fn add_cut(&mut self, band: usize, pos: f32, symmetric: bool) -> Vec<u32> {
        if band >= self.bands.len() {
            return Vec::new();
        }
        let pos = match self.shape {
            Shape::Circle => wrap01(pos),
            Shape::Rect => pos.clamp(-1.0 + MIN_GAP, 1.0 - MIN_GAP),
        };
        let targets = if self.shape == Shape::Circle && self.bands[band].cuts.len() <= 1 {
            // A lone cut on a whole ring (an old layout's) divides nothing.
            self.bands[band].cuts.clear();
            vec![(band, pos), (band, wrap01(pos + 0.5))]
        } else if symmetric && self.is_diameter(band) {
            let at = self.bands[band].cuts[0].pos;
            let fresh: Vec<(usize, f32)> = self.cut_mirror_targets(band, at).into_iter()
                .filter(|(b, p)| self.find_cut(*b, *p).is_none())
                .collect();
            if fresh.is_empty() {
                vec![(band, wrap01(at + 0.25)), (band, wrap01(at + 0.75))]
            } else {
                fresh
            }
        } else if symmetric {
            self.cut_mirror_targets(band, pos)
        } else {
            vec![(band, pos)]
        };
        let mut new_ids = Vec::new();
        for (b, p) in targets {
            if let Some(id) = self.insert_cut(b, p) {
                new_ids.push(id);
            }
        }
        new_ids
    }

    /// Insert one cut. `None` when it lands on an existing cut, or when it is a
    /// circle band's first cut (which makes no new cell).
    fn insert_cut(&mut self, band: usize, pos: f32) -> Option<u32> {
        let too_close = self.bands[band].cuts.iter().any(|c| match self.shape {
            Shape::Circle => wrap_diff(c.pos, pos).abs() < MIN_GAP,
            Shape::Rect => (c.pos - pos).abs() < MIN_GAP,
        });
        if too_close {
            return None;
        }
        let new_id = self.next_id();
        let shape = self.shape;
        let b = &mut self.bands[band];
        // Each cell is keyed by the cut it starts at (a rectangle's first cell
        // starts at the left side and stays first). The new cut starts the new
        // cell, so the cell it lands in keeps the part below it.
        let lead = match shape {
            Shape::Circle => None,
            Shape::Rect => Some(b.cells[0]),
        };
        let first_circle_cut = shape == Shape::Circle && b.cuts.is_empty();
        let mut starts: Vec<(Border, u32)> = match shape {
            Shape::Circle => b.cuts.drain(..).zip(b.cells.iter().copied()).collect(),
            Shape::Rect => b.cuts.drain(..).zip(b.cells.iter().skip(1).copied()).collect(),
        };
        let id = if first_circle_cut { b.cells[0] } else { new_id };
        starts.push((Border::hard(pos), id));
        starts.sort_by(|a, b| a.0.pos.total_cmp(&b.0.pos));
        b.cells = lead.into_iter().chain(starts.iter().map(|s| s.1)).collect();
        b.cuts = starts.into_iter().map(|s| s.0).collect();
        (!first_circle_cut).then_some(new_id)
    }

    /// Move a cut (and its mirror images when `symmetric`). The move is kept
    /// between the cut's neighbours, and under symmetry inside its quadrant —
    /// crossing an axis would collide it with its own mirror image. A cut ON an
    /// axis doesn't move under symmetry. Returns whether anything moved.
    pub fn move_cut(&mut self, band: usize, cut: usize, pos: f32, symmetric: bool) -> bool {
        let r = BorderRef::Cut { band, cut };
        let Some(old) = self.border(r).map(|b| b.pos) else { return false };
        if symmetric && self.is_diameter(band) {
            // The diameter turns as one.
            let d = wrap_diff(pos, old);
            for c in &mut self.bands[band].cuts {
                c.pos = wrap01(c.pos + d);
            }
            self.normalize_circle_band(band);
            return d.abs() > f32::EPSILON;
        }
        if symmetric && self.on_axis(r) {
            return false;
        }
        let mut new = self.clamp_cut_between_neighbours(band, cut, pos);
        if symmetric {
            new = match self.shape {
                Shape::Circle => {
                    // Unwrapped so the quadrant test is a plain interval.
                    let q0 = (old / 0.25).floor() * 0.25;
                    let unwrapped = old + wrap_diff(new, old);
                    wrap01(unwrapped.clamp(q0 + MIN_GAP * 0.5, q0 + 0.25 - MIN_GAP * 0.5))
                }
                Shape::Rect => {
                    if old > 0.0 { new.max(MIN_GAP * 0.5) } else { new.min(-MIN_GAP * 0.5) }
                }
            };
        }
        // Each mirror image moves by the reflection that made it.
        let mut moves: Vec<(usize, usize, f32)> = vec![(band, cut, new)];
        if symmetric {
            for (f, other_band) in cut_transforms(self.shape).into_iter().skip(1) {
                let b = if other_band { self.mirror_band(band) } else { band };
                if let Some(j) = self.find_cut(b, f(old)) {
                    if !moves.iter().any(|m| m.0 == b && m.1 == j) {
                        moves.push((b, j, f(new)));
                    }
                }
            }
        }
        let mut moved = false;
        for (b, j, p) in moves {
            if let Some(c) = self.bands[b].cuts.get_mut(j) {
                moved |= (c.pos - p).abs() > f32::EPSILON;
                c.pos = p;
            }
        }
        if self.shape == Shape::Circle {
            for b in 0..self.bands.len() {
                self.normalize_circle_band(b);
            }
        }
        moved
    }

    /// `pos` kept at least [`MIN_GAP`] from the cut's neighbours (and, for a
    /// rectangle, the sides).
    fn clamp_cut_between_neighbours(&self, band: usize, cut: usize, pos: f32) -> f32 {
        let cuts = &self.bands[band].cuts;
        match self.shape {
            Shape::Circle => {
                let n = cuts.len();
                if n <= 1 {
                    return wrap01(pos);
                }
                let me = cuts[cut].pos;
                let mut prev = cuts[(cut + n - 1) % n].pos;
                let mut next = cuts[(cut + 1) % n].pos;
                if prev >= me { prev -= 1.0; }
                if next <= me { next += 1.0; }
                let unwrapped = me + wrap_diff(pos, me);
                wrap01(unwrapped.clamp(prev + MIN_GAP, next - MIN_GAP))
            }
            Shape::Rect => {
                let lo = cut.checked_sub(1).map(|j| cuts[j].pos).unwrap_or(-1.0) + MIN_GAP;
                let hi = cuts.get(cut + 1).map(|c| c.pos).unwrap_or(1.0) - MIN_GAP;
                pos.clamp(lo, hi.max(lo))
            }
        }
    }

    /// Keep a circle band's cuts in ascending order after a move wrapped one
    /// past 12 o'clock — rotating cuts and cells together, so each cell still
    /// starts at its own cut.
    fn normalize_circle_band(&mut self, band: usize) {
        let b = &mut self.bands[band];
        if b.cuts.len() <= 1 {
            return;
        }
        let k = b.cuts.iter().enumerate()
            .min_by(|x, y| x.1.pos.total_cmp(&y.1.pos)).map(|(i, _)| i).unwrap_or(0);
        b.cuts.rotate_left(k);
        b.cells.rotate_left(k);
    }

    /// Remove a cut (and its mirror images when `symmetric`), merging the two
    /// cells it separated. Of the two, the one `keep` prefers survives (e.g. the
    /// one with mappings); if both or neither, the lower id. Returns
    /// `(removed, kept)` per merge, so the caller can move the removed cell's
    /// mappings onto the kept one.
    pub fn remove_cut(
        &mut self,
        band: usize,
        cut: usize,
        symmetric: bool,
        keep: impl Fn(u32) -> bool,
    ) -> Vec<(u32, u32)> {
        let r = BorderRef::Cut { band, cut };
        if self.border(r).is_none() {
            return Vec::new();
        }
        let set = if symmetric { self.mirror_set(r) } else { vec![r] };
        // By position, since indices shift as cuts go.
        let targets: Vec<(usize, f32)> = set.iter().filter_map(|m| match *m {
            BorderRef::Cut { band, cut } => Some((band, self.bands[band].cuts[cut].pos)),
            BorderRef::Edge(_) => None,
        }).collect();
        let mut merges = Vec::new();
        for (b, p) in targets {
            if let Some(j) = self.find_cut(b, p) {
                if let Some(m) = self.delete_cut(b, j, &keep) {
                    merges.push(m);
                }
            }
        }
        self.cell_round.retain(|(id, _)| !merges.iter().any(|(removed, _)| removed == id));
        self.cell_pressure.retain(|(id, _)| !merges.iter().any(|(removed, _)| removed == id));
        merges
    }

    fn delete_cut(&mut self, band: usize, cut: usize, keep: &impl Fn(u32) -> bool) -> Option<(u32, u32)> {
        let shape = self.shape;
        let b = &mut self.bands[band];
        let n = b.cuts.len();
        if shape == Shape::Circle && n == 1 {
            b.cuts.clear();
            return None;
        }
        let (lo, hi) = match shape {
            Shape::Circle => ((cut + n - 1) % n, cut),
            Shape::Rect => (cut, cut + 1),
        };
        let (lo_id, hi_id) = (b.cells[lo], b.cells[hi]);
        let kept = match (keep(lo_id), keep(hi_id)) {
            (true, false) => lo_id,
            (false, true) => hi_id,
            _ => lo_id.min(hi_id),
        };
        let removed = if kept == lo_id { hi_id } else { lo_id };
        b.cuts.remove(cut);
        b.cells.remove(hi);
        let lo_after = if lo > hi { lo - 1 } else { lo };
        b.cells[lo_after] = kept;
        // A ring left with one cut is whole: that cut divides nothing.
        if shape == Shape::Circle && b.cuts.len() == 1 {
            b.cuts.clear();
        }
        Some((removed, kept))
    }

    /// Add an edge at band coordinate `pos` (and its mirror for a symmetric
    /// rectangle), splitting the band it lands in. The inner/lower part keeps
    /// the band's cells; the new part copies its cuts with fresh cell ids.
    /// Returns the new cell ids.
    pub fn add_edge(&mut self, pos: f32, symmetric: bool) -> Vec<u32> {
        let (lo, hi) = match self.shape {
            Shape::Circle => (MIN_GAP, 1.0 - MIN_GAP),
            Shape::Rect => (-1.0 + MIN_GAP, 1.0 - MIN_GAP),
        };
        let pos = pos.clamp(lo, hi);
        let mut targets = vec![pos];
        if symmetric && self.shape == Shape::Rect && pos.abs() >= SAME {
            targets.push(-pos);
        }
        let mut new_ids = Vec::new();
        for p in targets {
            let k = self.edges.iter().filter(|e| e.pos <= p).count();
            let shape = RingShape { square: self.inherited_square(k), corners: self.inherited_corners(k) };
            new_ids.extend(self.insert_edge(k, p, shape).unwrap_or_default());
        }
        new_ids
    }

    /// Add an edge through a field point (and its mirror for a symmetric
    /// rectangle). A circle's new ring takes the squareness of the ring inside
    /// it (else outside it), so it nests the way its neighbours already do.
    pub fn add_edge_at(&mut self, x: f32, y: f32, symmetric: bool) -> Vec<u32> {
        match self.shape {
            Shape::Rect => self.add_edge(y, symmetric),
            Shape::Circle => {
                let k = self.band_of(x, y);
                let square = self.inherited_square(k);
                let corners = self.inherited_corners(k);
                let pos = RingShape { square, corners }.norm(x, y);
                self.insert_edge(k, pos, RingShape { square, corners }).unwrap_or_default()
            }
        }
    }

    /// Whether a new ring between edges `k - 1` and `k` squares by its corners.
    fn inherited_corners(&self, k: usize) -> bool {
        k.checked_sub(1).and_then(|i| self.edges.get(i)).or_else(|| self.edges.get(k))
            .is_some_and(|b| b.corners)
    }

    /// The squareness a new ring between edges `k - 1` and `k` takes.
    fn inherited_square(&self, k: usize) -> f32 {
        if self.shape != Shape::Circle {
            return 0.0;
        }
        k.checked_sub(1).and_then(|i| self.edges.get(i)).or_else(|| self.edges.get(k))
            .map(|b| b.square).unwrap_or(0.0)
    }

    /// Insert an edge as index `k` (splitting band `k`), kept clear of its
    /// neighbours in every direction. The inner/lower part keeps the band's
    /// cells; the new part copies its cuts with fresh ids. `None` when there is
    /// no room for it.
    fn insert_edge(&mut self, k: usize, pos: f32, shape: RingShape) -> Option<Vec<u32>> {
        let inner = k.checked_sub(1).and_then(|i| self.edges.get(i));
        let outer = self.edges.get(k);
        let (lo, hi) = match self.shape {
            Shape::Circle => ring_bounds(inner, outer, shape),
            Shape::Rect => (
                inner.map(|b| b.pos).unwrap_or(-1.0) + MIN_GAP,
                outer.map(|b| b.pos).unwrap_or(1.0) - MIN_GAP,
            ),
        };
        if lo > hi {
            return None;
        }
        let pos = pos.clamp(lo, hi);
        let base = self.next_id();
        let mut copy = self.bands[k].clone();
        let mut new_ids = Vec::new();
        for (i, c) in copy.cells.iter_mut().enumerate() {
            *c = base + i as u32;
            new_ids.push(*c);
        }
        let circle = self.shape == Shape::Circle;
        self.edges.insert(k, Border {
            square: if circle { shape.square } else { 0.0 },
            corners: circle && shape.corners,
            ..Border::hard(pos)
        });
        self.bands.insert(k + 1, copy);
        Some(new_ids)
    }

    /// Square (or round) circle edge `e` toward `square`, as far as it can go
    /// while staying clear of its neighbours in every direction. Returns
    /// whether it changed.
    pub fn set_edge_square(&mut self, e: usize, square: f32) -> bool {
        if self.shape != Shape::Circle || e >= self.edges.len() {
            return false;
        }
        let corners = self.edges[e].corners;
        let cur = self.edges[e].square;
        let next = self.furthest_fitting(e, corners, cur, square.clamp(0.0, 1.0));
        self.edges[e].square = next;
        (next - cur).abs() > 1e-6
    }

    /// Switch how circle edge `e` squares (see [`Border::corners`]). The ring
    /// keeps its squareness where it still fits, else squares only as far as
    /// it can. Returns whether it changed.
    pub fn set_edge_corners(&mut self, e: usize, corners: bool) -> bool {
        if self.shape != Shape::Circle || e >= self.edges.len() || self.edges[e].corners == corners {
            return false;
        }
        let want = self.edges[e].square;
        // A circle is the same in both modes, so 0 is where the switch can
        // always start from.
        let square = self.furthest_fitting(e, corners, 0.0, want);
        self.edges[e].corners = corners;
        self.edges[e].square = square;
        true
    }

    /// The squareness nearest `target`, going from `from`, at which edge `e`
    /// (squaring by `corners`) still clears its neighbours.
    fn furthest_fitting(&self, e: usize, corners: bool, from: f32, target: f32) -> f32 {
        let pos = self.edges[e].pos;
        let inner = e.checked_sub(1).map(|i| &self.edges[i]);
        let outer = self.edges.get(e + 1);
        let fits = |s: f32| {
            let (lo, hi) = ring_bounds(inner, outer, RingShape { square: s, corners });
            pos >= lo - 1e-5 && pos <= hi + 1e-5
        };
        if fits(target) || !fits(from) {
            return target;
        }
        let (mut ok, mut bad) = (from, target);
        for _ in 0..24 {
            let mid = (ok + bad) * 0.5;
            if fits(mid) { ok = mid } else { bad = mid }
        }
        ok
    }

    /// Move an edge (and its mirror for a symmetric rectangle), kept between
    /// its neighbours and, under symmetry, on its own side of the middle. An
    /// edge ON the middle doesn't move under symmetry. Returns whether it moved.
    pub fn move_edge(&mut self, edge: usize, pos: f32, symmetric: bool) -> bool {
        let r = BorderRef::Edge(edge);
        let Some(old) = self.border(r).map(|b| b.pos) else { return false };
        if symmetric && self.on_axis(r) {
            return false;
        }
        let (lo, hi) = match self.shape {
            Shape::Circle => ring_bounds(
                edge.checked_sub(1).map(|e| &self.edges[e]),
                self.edges.get(edge + 1),
                self.edges[edge].ring(),
            ),
            Shape::Rect => (
                edge.checked_sub(1).map(|e| self.edges[e].pos).unwrap_or(-1.0) + MIN_GAP,
                self.edges.get(edge + 1).map(|e| e.pos).unwrap_or(1.0) - MIN_GAP,
            ),
        };
        let mut new = pos.clamp(lo, hi.max(lo));
        let mirror = (symmetric && self.shape == Shape::Rect)
            .then(|| self.mirror_set(r).get(1).copied()).flatten();
        if mirror.is_some() {
            new = if old > 0.0 { new.max(MIN_GAP * 0.5) } else { new.min(-MIN_GAP * 0.5) };
        }
        let moved = (self.edges[edge].pos - new).abs() > f32::EPSILON;
        self.edges[edge].pos = new;
        if let Some(BorderRef::Edge(m)) = mirror {
            self.edges[m].pos = -new;
        }
        moved
    }

    /// Remove an edge (and its mirror for a symmetric rectangle), merging the
    /// two bands it separated. The band with more cells `keep` prefers survives
    /// with its cuts; on a tie, the one nearer the centre. Each cell of the
    /// dropped band is reported as `(removed, kept)` against the surviving
    /// cell at its middle, so its mappings can follow.
    pub fn remove_edge(&mut self, edge: usize, symmetric: bool, keep: impl Fn(u32) -> bool) -> Vec<(u32, u32)> {
        let r = BorderRef::Edge(edge);
        if self.border(r).is_none() {
            return Vec::new();
        }
        let set = if symmetric { self.mirror_set(r) } else { vec![r] };
        let positions: Vec<f32> = set.iter().filter_map(|m| match *m {
            BorderRef::Edge(e) => Some(self.edges[e].pos),
            BorderRef::Cut { .. } => None,
        }).collect();
        let mut merges = Vec::new();
        for p in positions {
            let Some(e) = self.edges.iter().position(|x| (x.pos - p).abs() < SAME) else { continue };
            merges.extend(self.delete_edge(e, &keep));
        }
        self.cell_round.retain(|(id, _)| !merges.iter().any(|(removed, _)| removed == id));
        self.cell_pressure.retain(|(id, _)| !merges.iter().any(|(removed, _)| removed == id));
        merges
    }

    fn delete_edge(&mut self, edge: usize, keep: &impl Fn(u32) -> bool) -> Vec<(u32, u32)> {
        let (lo, hi) = (edge, edge + 1);
        let score = |b: &Band| b.cells.iter().filter(|c| keep(**c)).count();
        let centre_dist = |b: usize| {
            let [a, z] = self.band_span(b);
            ((a + z) * 0.5).abs()
        };
        let keep_lo = match score(&self.bands[lo]).cmp(&score(&self.bands[hi])) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => centre_dist(lo) <= centre_dist(hi),
        };
        let (kept_band, dropped_band) = if keep_lo { (lo, hi) } else { (hi, lo) };
        let merges: Vec<(u32, u32)> = (0..self.bands[dropped_band].cells.len()).map(|i| {
            let [a, z] = self.cell_span(dropped_band, i);
            let mid = match self.shape {
                Shape::Circle => wrap01((a + z) * 0.5),
                Shape::Rect => (a + z) * 0.5,
            };
            let kept = self.bands[kept_band].cells[self.cell_index_in(kept_band, mid)];
            (self.bands[dropped_band].cells[i], kept)
        }).collect();
        self.edges.remove(edge);
        self.bands.remove(dropped_band);
        merges
    }

    /// The position that centres a border between its neighbours, evening out
    /// the two cells (or bands) it separates — what a double-click does.
    pub fn centered_pos(&self, r: BorderRef) -> Option<f32> {
        match r {
            BorderRef::Edge(e) => {
                self.edges.get(e)?;
                let (min, max) = match self.shape {
                    Shape::Circle => (0.0, 1.0),
                    Shape::Rect => (-1.0, 1.0),
                };
                let lo = e.checked_sub(1).map(|i| self.edges[i].pos).unwrap_or(min);
                let hi = self.edges.get(e + 1).map(|b| b.pos).unwrap_or(max);
                Some((lo + hi) * 0.5)
            }
            BorderRef::Cut { band, cut } => {
                let cuts = &self.bands.get(band)?.cuts;
                let me = cuts.get(cut)?.pos;
                match self.shape {
                    Shape::Circle => {
                        let n = cuts.len();
                        if n < 2 {
                            return Some(me);
                        }
                        let mut prev = cuts[(cut + n - 1) % n].pos;
                        let mut next = cuts[(cut + 1) % n].pos;
                        if prev >= me { prev -= 1.0; }
                        if next <= me { next += 1.0; }
                        Some(wrap01((prev + next) * 0.5))
                    }
                    Shape::Rect => {
                        let lo = cut.checked_sub(1).map(|j| cuts[j].pos).unwrap_or(-1.0);
                        let hi = cuts.get(cut + 1).map(|c| c.pos).unwrap_or(1.0);
                        Some((lo + hi) * 0.5)
                    }
                }
            }
        }
    }

    /// [`Self::centered_pos`] under symmetric editing. A neighbour that is the
    /// border's own mirror image moves along with it, so the border evens out
    /// the cells with that counted. A lone cut in a quarter goes to its middle
    /// (45°). A cut beside its own mirror across an axis splits the cell the
    /// two share into three equal parts.
    pub fn centered_pos_symmetric(&self, r: BorderRef) -> Option<f32> {
        let plain = self.centered_pos(r)?;
        let x = self.border(r)?.pos;
        if let BorderRef::Cut { band, .. } = r {
            if self.is_diameter(band) {
                // A diameter has nothing to centre between: square it up to
                // the nearest eighth (upright, level or diagonal).
                return Some(wrap01((x * 8.0).round() / 8.0));
            }
        }
        if self.on_axis(r) {
            return Some(x);
        }
        let (lo, hi, a, b) = match (self.shape, r) {
            (Shape::Circle, BorderRef::Edge(_)) => return Some(plain),
            (Shape::Circle, BorderRef::Cut { band, cut }) => {
                let cuts = &self.bands[band].cuts;
                let n = cuts.len();
                if n < 2 {
                    return Some(plain);
                }
                let mut prev = cuts[(cut + n - 1) % n].pos;
                let mut next = cuts[(cut + 1) % n].pos;
                if prev >= x { prev -= 1.0; }
                if next <= x { next += 1.0; }
                let a = (x / 0.25).floor() * 0.25;
                (prev, next, Some(a), Some(a + 0.25))
            }
            (Shape::Rect, _) => {
                let (list, i): (Vec<f32>, usize) = match r {
                    BorderRef::Edge(e) => (self.edges.iter().map(|b| b.pos).collect(), e),
                    BorderRef::Cut { band, cut } => (self.bands[band].cuts.iter().map(|c| c.pos).collect(), cut),
                };
                let prev = i.checked_sub(1).map(|j| list[j]).unwrap_or(-1.0);
                let next = list.get(i + 1).copied().unwrap_or(1.0);
                if x > 0.0 { (prev, next, Some(0.0), None) } else { (prev, next, None, Some(0.0)) }
            }
        };
        let mirror_lo = a.filter(|a| (lo - (2.0 * a - x)).abs() < 2.0 * SAME);
        let mirror_hi = b.filter(|b| (hi - (2.0 * b - x)).abs() < 2.0 * SAME);
        let pos = match (mirror_lo, mirror_hi) {
            (None, None) => (lo + hi) * 0.5,
            (Some(a), None) => (hi + 2.0 * a) / 3.0,
            (None, Some(b)) => (lo + 2.0 * b) / 3.0,
            (Some(a), Some(b)) => (a + b) * 0.5,
        };
        Some(match self.shape {
            Shape::Circle => wrap01(pos),
            Shape::Rect => pos,
        })
    }

    /// Set or clear a border's gradient (and its mirror images' when
    /// `symmetric`). Returns whether a border was found.
    pub fn set_gradient(&mut self, r: BorderRef, gradient: Option<Gradient>, symmetric: bool) -> bool {
        if self.border(r).is_none() {
            return false;
        }
        let set = if symmetric { self.mirror_set(r) } else { vec![r] };
        for m in set {
            if let Some(b) = self.border_mut(m) {
                b.gradient = gradient.clone();
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear(_: &Gradient, t: f32) -> f32 {
        t
    }

    fn dir(deg: f32, r: f32) -> (f32, f32) {
        let a = deg.to_radians();
        (a.sin() * r, a.cos() * r)
    }

    #[test]
    fn the_default_circle_is_a_symmetric_four_way_numbered_by_direction() {
        let l = AreaLayout::default_for(Shape::Circle);
        assert_eq!(l.bands[1].cells.len(), 4);
        for (deg, id) in [(0.0f32, 1), (90.0, 3), (180.0, 5), (270.0, 7)] {
            let (x, y) = dir(deg, 0.9);
            assert_eq!(l.locate_id(x, y), id, "{deg}°");
        }
        assert!(l.is_symmetric());
        assert_eq!(l.locate_id(0.0, 0.1), 0);
    }

    #[test]
    fn analog_shares_project_the_stick_onto_the_direction_cells() {
        let l = AreaLayout::default_for(Shape::Circle);
        // Full diagonal up-right: W and D at 0.71 each, nothing else.
        let (x, y) = dir(45.0, 1.0);
        let w = l.analog_shares(x, y, &AnalogTune::default());
        let share = |id| w.iter().find(|c| c.id == id).map(|c| c.weight).unwrap_or(0.0);
        assert!((share(1) - 0.7071).abs() < 1e-3 && (share(3) - 0.7071).abs() < 1e-3, "{w:?}");
        assert_eq!(share(5) + share(7), 0.0);
        // Half way out past the deadzone, straight up: 0.5.
        let w = l.analog_shares(0.0, 0.65, &AnalogTune::default());
        assert!((w[0].weight - 0.5).abs() < 1e-3 && w[0].id == 1, "{w:?}");
        // Inside the deadzone: the centre.
        assert_eq!(l.analog_shares(0.1, 0.1, &AnalogTune::default())[0].id, 0);
    }

    #[test]
    fn default_circle_is_a_centre_and_eight_way_with_up_as_cell_1() {
        let l = AreaLayout::circle_ways(8);
        assert_eq!(l.locate_id(0.0, 0.1), 0, "inside the centre");
        let (x, y) = dir(0.0, 0.9);
        assert_eq!(l.locate_id(x, y), 1, "up");
        let (x, y) = dir(45.0, 0.9);
        assert_eq!(l.locate_id(x, y), 2, "up-right");
        let (x, y) = dir(90.0, 0.9);
        assert_eq!(l.locate_id(x, y), 3, "right");
        let (x, y) = dir(-45.0, 0.9);
        assert_eq!(l.locate_id(x, y), 8, "up-left");
        // Past the rim is still the outer ring.
        assert_eq!(l.locate_id(0.0, 1.4), 1);
    }

    #[test]
    fn default_rect_is_a_three_by_three_grid_numbered_like_the_circle() {
        let l = AreaLayout::default_for(Shape::Rect);
        let c = AreaLayout::circle_ways(8);
        for deg in [0.0f32, 45.0, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0] {
            let (x, y) = dir(deg, 0.9);
            assert_eq!(l.locate_id(x, y), c.locate_id(x, y), "{deg}°");
        }
        assert_eq!(l.locate_id(0.0, 0.0), 0, "middle");
        assert!(l.is_symmetric());
    }

    #[test]
    fn layouts_round_trip_through_json() {
        let mut l = AreaLayout::circle_ways(8);
        l.set_gradient(BorderRef::Cut { band: 1, cut: 0 }, Some(Gradient::with_width(0.05)), true);
        let back = AreaLayout::from_value(&l.to_value()).expect("parses");
        assert_eq!(back, l);
        let r = AreaLayout::default_for(Shape::Rect);
        assert_eq!(AreaLayout::from_value(&r.to_value()), Some(r));
    }

    #[test]
    fn a_broken_layout_does_not_parse() {
        let mut v = AreaLayout::circle_ways(8).to_value();
        v["bands"][1]["cells"] = json!([1, 2]);
        assert!(AreaLayout::from_value(&v).is_none());
    }

    #[test]
    fn without_gradients_the_one_cell_takes_everything() {
        let l = AreaLayout::circle_ways(8);
        let (x, y) = dir(10.0, 0.8);
        let w = l.weights(x, y, linear);
        assert_eq!(w.len(), 1);
        assert_eq!((w[0].id, w[0].weight, w[0].governor), (1, 1.0, None));
    }

    #[test]
    fn a_gradient_cut_crossfades_its_two_cells() {
        let mut l = AreaLayout::circle_ways(8);
        // The cut between up (1) and up-right (2) sits at 22.5°.
        let r = BorderRef::Cut { band: 1, cut: 0 };
        l.set_gradient(r, Some(Gradient::with_width(0.1)), false);
        let (x, y) = dir(22.5, 0.8);
        let w = l.weights(x, y, linear);
        let share = |id| w.iter().find(|c| c.id == id).map(|c| c.weight).unwrap_or(0.0);
        assert!((share(1) - 0.5).abs() < 1e-3 && (share(2) - 0.5).abs() < 1e-3, "{w:?}");
        // A quarter of the band toward up-right.
        let (x, y) = dir(22.5 + 0.025 * 360.0, 0.8);
        let w = l.weights(x, y, linear);
        let share = |id| w.iter().find(|c| c.id == id).map(|c| c.weight).unwrap_or(0.0);
        assert!((share(2) - 0.75).abs() < 1e-3, "{w:?}");
        assert!(w.iter().all(|c| c.governor.map(|g| g.0) == Some(r)));
    }

    #[test]
    fn a_gradient_edge_and_cut_split_a_corner_four_ways_summing_to_one() {
        let mut l = AreaLayout::circle_ways(8);
        l.set_gradient(BorderRef::Edge(0), Some(Gradient::with_width(0.2)), false);
        l.set_gradient(BorderRef::Cut { band: 1, cut: 0 }, Some(Gradient::with_width(0.1)), false);
        let (x, y) = dir(25.0, 0.32);
        let w = l.weights(x, y, linear);
        let total: f32 = w.iter().map(|c| c.weight).sum();
        assert!((total - 1.0).abs() < 1e-4, "{w:?}");
        let ids: Vec<u32> = w.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![0, 1, 2], "centre, up, up-right");
        // The centre has no cuts, so only the ring edge governs it.
        assert_eq!(w[0].governor.map(|g| g.0), Some(BorderRef::Edge(0)));
    }

    #[test]
    fn the_curve_hook_shapes_the_fade() {
        let mut l = AreaLayout::default_for(Shape::Rect);
        l.set_gradient(BorderRef::Cut { band: 1, cut: 1 }, Some(Gradient::with_width(0.2)), false);
        let x = 1.0 / 3.0 + 0.05; // 3/4 of the way across
        let w = l.weights(x, 0.0, |_, t| t * t);
        let right = w.iter().find(|c| c.id == 3).unwrap().weight;
        assert!((right - 0.5625).abs() < 1e-3, "{w:?}");
    }

    #[test]
    fn a_symmetric_cut_appears_in_all_four_quarters() {
        let mut l = AreaLayout::circle_ways(8);
        let before = l.bands[1].cuts.len();
        let new = l.add_cut(1, 0.03, true);
        assert_eq!(new.len(), 4);
        assert_eq!(l.bands[1].cuts.len(), before + 4);
        for p in [0.03, 0.97, 0.47, 0.53] {
            assert!(l.find_cut(1, p).is_some(), "cut at {p}");
        }
        assert!(l.is_symmetric());
    }

    #[test]
    fn a_cut_on_an_axis_mirrors_to_two_not_four() {
        let mut l = AreaLayout::circle_ways(8);
        l.bands[1] = Band { cuts: Vec::new(), cells: vec![1] };
        l.add_cut(1, 0.0, true);
        let cuts: Vec<f32> = l.bands[1].cuts.iter().map(|c| c.pos).collect();
        assert_eq!(cuts, vec![0.0, 0.5]);
        assert_eq!(l.bands[1].cells.len(), 2, "two halves: right and left");
        // A lone diameter turns as one under symmetry (it is not pinned).
        assert!(l.move_cut(1, 0, 0.1, true));
        let mut at: Vec<f32> = l.bands[1].cuts.iter().map(|c| c.pos).collect();
        at.sort_by(f32::total_cmp);
        assert!((at[0] - 0.1).abs() < 1e-5 && (at[1] - 0.6).abs() < 1e-5, "{at:?}");
    }

    #[test]
    fn moving_a_symmetric_cut_moves_its_mirrors_and_stays_in_its_quarter() {
        let mut l = AreaLayout::circle_ways(8);
        // Cut 0 is at 22.5°; move it to 30°.
        assert!(l.move_cut(1, 0, 30.0 / 360.0, true));
        for deg in [30.0f32, 330.0, 150.0, 210.0] {
            assert!(l.find_cut(1, deg / 360.0).is_some(), "cut at {deg}°");
        }
        assert!(l.is_symmetric());
        // Dragging past the 3 o'clock axis stops short of it (and of 67.5°).
        l.move_cut(1, 0, 100.0 / 360.0, true);
        let p = l.bands[1].cuts[0].pos * 360.0;
        assert!(p < 67.5, "kept under its neighbour, got {p}°");
    }

    #[test]
    fn a_circle_cut_moved_past_twelve_keeps_its_cells_in_order() {
        let mut l = AreaLayout::circle_ways(8);
        let up = 1;
        // Free move: the 337.5° cut (start of up) to 5°.
        let j = l.find_cut(1, 337.5 / 360.0).unwrap();
        l.move_cut(1, j, 5.0 / 360.0, false);
        let (x, y) = dir(10.0, 0.9);
        assert_eq!(l.locate_id(x, y), up);
        let (x, y) = dir(0.0, 0.9);
        assert_eq!(l.locate_id(x, y), 8, "up-left grew past 12 o'clock");
        assert!(l.bands[1].cuts.windows(2).all(|w| w[0].pos < w[1].pos));
    }

    #[test]
    fn removing_a_cut_keeps_the_cell_with_mappings() {
        let mut l = AreaLayout::circle_ways(8);
        // Cut 0 (22.5°) separates up (1) and up-right (2).
        let merges = l.remove_cut(1, 0, false, |id| id == 2);
        assert_eq!(merges, vec![(1, 2)]);
        let (x, y) = dir(0.0, 0.9);
        assert_eq!(l.locate_id(x, y), 2);
        assert_eq!(l.bands[1].cells.len(), 7);
    }

    #[test]
    fn removing_a_symmetric_cut_removes_its_mirrors() {
        let mut l = AreaLayout::circle_ways(8);
        let merges = l.remove_cut(1, 0, true, |_| false);
        assert_eq!(merges.len(), 4);
        assert_eq!(l.bands[1].cuts.len(), 4);
        assert!(l.is_symmetric());
    }

    #[test]
    fn rect_symmetry_mirrors_rows_and_columns() {
        let mut l = AreaLayout::default_for(Shape::Rect);
        // A cut in the top row at x = 0.6 mirrors to −0.6 and to the bottom row.
        let new = l.add_cut(2, 0.6, true);
        assert_eq!(new.len(), 4);
        for (b, p) in [(2, 0.6), (2, -0.6), (0, 0.6), (0, -0.6)] {
            assert!(l.find_cut(b, p).is_some(), "row {b} at {p}");
        }
        assert_eq!(l.bands[1].cuts.len(), 2, "the middle row is untouched");
        assert!(l.is_symmetric());
        // Moving it moves all four.
        let j = l.find_cut(2, 0.6).unwrap();
        l.move_cut(2, j, 0.7, true);
        for (b, p) in [(2, 0.7), (2, -0.7), (0, 0.7), (0, -0.7)] {
            assert!(l.find_cut(b, p).is_some(), "row {b} at {p}");
        }
    }

    #[test]
    fn a_symmetric_rect_edge_adds_and_moves_in_pairs() {
        let mut l = AreaLayout::default_for(Shape::Rect);
        let new = l.add_edge(0.7, true);
        assert_eq!(l.edges.len(), 4);
        assert_eq!(new.len(), 6, "two new rows of three");
        assert!(l.is_symmetric());
        let e = l.edges.iter().position(|e| (e.pos - 0.7).abs() < 1e-4).unwrap();
        l.move_edge(e, 0.8, true);
        assert!(l.edges.iter().any(|e| (e.pos + 0.8).abs() < 1e-4));
        assert!(l.is_symmetric());
    }

    #[test]
    fn new_ids_never_collide() {
        let mut l = AreaLayout::circle_ways(8);
        l.add_edge(0.6, true);
        l.add_cut(0, 0.1, true);
        let mut ids = l.cell_ids();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "{:?}", l.cell_ids());
    }

    #[test]
    fn removing_an_edge_maps_the_dropped_cells_onto_the_kept_ones() {
        let mut l = AreaLayout::circle_ways(8);
        // The centre (0) has no mappings, the ring does: the ring survives and
        // the centre reports onto the ring cell at its middle.
        let merges = l.remove_edge(0, true, |id| id != 0);
        assert_eq!(l.bands.len(), 1);
        assert_eq!(l.bands[0].cells.len(), 8);
        assert_eq!(merges.len(), 1);
        assert_eq!(merges[0].0, 0);
    }

    #[test]
    fn centring_evens_out_the_neighbours() {
        let mut l = AreaLayout::circle_ways(8);
        l.move_cut(1, 0, 30.0 / 360.0, false);
        let mid = l.centered_pos(BorderRef::Cut { band: 1, cut: 0 }).unwrap();
        assert!((mid * 360.0 - 22.5).abs() < 1e-3, "between 337.5° and 67.5°: {}", mid * 360.0);
        // The wrap cut (337.5°) centres across 12 o'clock.
        let j = l.find_cut(1, 292.5 / 360.0).unwrap();
        l.move_cut(1, j, 300.0 / 360.0, false);
        let k = l.find_cut(1, 337.5 / 360.0).unwrap();
        let mid = l.centered_pos(BorderRef::Cut { band: 1, cut: k }).unwrap();
        assert!((mid * 360.0 - 345.0).abs() < 1e-3, "{}", mid * 360.0);
        assert_eq!(l.centered_pos(BorderRef::Edge(0)), Some(0.5));
        let r = AreaLayout::default_for(Shape::Rect);
        // The top row edge sits between the bottom one (−1/3) and the top side.
        let top = r.centered_pos(BorderRef::Edge(1)).unwrap();
        assert!((top - 1.0 / 3.0).abs() < 1e-5, "{top}");
    }

    #[test]
    fn a_whole_ring_halves_then_quarters() {
        let mut l = AreaLayout::circle_rings(0.5);
        // First cut: a diameter, two halves.
        l.add_cut(1, 30.0 / 360.0, true);
        assert_eq!(l.bands[1].cells.len(), 2);
        assert!(l.is_diameter(1) && l.is_symmetric());
        // It turns as one, past any axis.
        assert!(l.move_cut(1, 0, 100.0 / 360.0, true));
        let mut at: Vec<f32> = l.bands[1].cuts.iter().map(|c| (c.pos * 360.0).round()).collect();
        at.sort_by(f32::total_cmp);
        assert_eq!(at, vec![100.0, 280.0]);
        // Double-click squares it to the nearest eighth.
        let k = l.find_cut(1, 100.0 / 360.0).unwrap();
        let p = l.centered_pos_symmetric(BorderRef::Cut { band: 1, cut: k }).unwrap();
        assert!((p * 360.0 - 90.0).abs() < 1e-3);
        // The next symmetric cut: the diameter's mirror image, four cells.
        l.add_cut(1, 0.0, true);
        assert_eq!(l.bands[1].cells.len(), 4, "{:?}", l.bands[1].cuts);
        assert!(l.is_symmetric());
        // Removing back down to one cut leaves the ring whole, no seam.
        let mut l = AreaLayout::circle_rings(0.5);
        l.add_cut(1, 0.0, true);
        l.add_cut(1, 0.0, true);
        assert_eq!(l.bands[1].cells.len(), 4, "on an axis: the cross");
        l.remove_cut(1, 0, false, |_| false);
        l.remove_cut(1, 0, false, |_| false);
        l.remove_cut(1, 0, false, |_| false);
        assert!(l.bands[1].cuts.is_empty() && l.bands[1].cells.len() == 1);
    }

    #[test]
    fn analog_tune_sets_the_full_zone_and_the_midpoint() {
        let t = AnalogTune { full: 0.8, mid: 0.5 };
        assert_eq!(t.push(0.9, 0.3), 1.0);
        assert!((t.push(0.55, 0.3) - 0.5).abs() < 1e-5);
        let t = AnalogTune { full: 1.0, mid: 0.25 };
        assert!((t.push(0.25, 0.0) - 0.5).abs() < 1e-4, "half at a quarter push");
        assert_eq!(t.push(1.0, 0.0), 1.0);
    }

    #[test]
    fn symmetric_centring_counts_the_mirror_images() {
        // A ring cut once per quarter at 20°: centring puts it at 45°.
        let mut l = AreaLayout::circle_ways(4);
        l.remove_cut(1, 0, false, |_| false);
        l.remove_cut(1, 0, false, |_| false);
        l.remove_cut(1, 0, false, |_| false);
        assert!(l.bands[1].cuts.is_empty());
        let mut l2 = l.clone();
        for p in [20.0f32, 160.0, 200.0, 340.0] {
            l2.add_cut(1, p / 360.0, false);
        }
        let l = l2;
        assert_eq!(l.bands[1].cuts.len(), 4, "{:?}", l.bands[1].cuts);
        let k = l.find_cut(1, 20.0 / 360.0).unwrap();
        let p = l.centered_pos_symmetric(BorderRef::Cut { band: 1, cut: k }).unwrap();
        assert!((p * 360.0 - 45.0).abs() < 1e-3, "{}", p * 360.0);
        // 4-way plus ±15°: the up cell splits in three equal parts.
        let mut l = AreaLayout::circle_ways(4);
        l.add_cut(1, 10.0 / 360.0, true);
        let k = l.find_cut(1, 10.0 / 360.0).unwrap();
        let p = l.centered_pos_symmetric(BorderRef::Cut { band: 1, cut: k }).unwrap();
        assert!((p * 360.0 - 15.0).abs() < 1e-3, "{}", p * 360.0);
        // The 45° cut beside it evens the cell up to its mirror at 135° (and
        // its own neighbour at 10°): 10…63⅓ and 63⅓…116⅔.
        let k = l.find_cut(1, 45.0 / 360.0).unwrap();
        let p = l.centered_pos_symmetric(BorderRef::Cut { band: 1, cut: k }).unwrap();
        assert!((p * 360.0 - 190.0 / 3.0).abs() < 1e-3, "{}", p * 360.0);
        // A rectangle row cut once each side: thirds.
        let mut r = AreaLayout::default_for(Shape::Rect);
        r.remove_cut(1, 0, true, |_| false);
        assert!(r.bands[1].cuts.is_empty());
        r.add_cut(1, 0.2, true);
        let k = r.find_cut(1, 0.2).unwrap();
        let p = r.centered_pos_symmetric(BorderRef::Cut { band: 1, cut: k }).unwrap();
        assert!((p - 1.0 / 3.0).abs() < 1e-4, "{p}");
    }

    #[test]
    fn analog_moves_steer_between_a_key_and_the_diagonal() {
        let l = AreaLayout::circle_ways(4);
        // Full push at 30°: between W (0°) and W + D (45°), never nothing.
        let (x, y) = dir(30.0, 1.0);
        let (band, moves) = l.analog_moves(x, y, &AnalogTune::default());
        assert_eq!(band, 1);
        let share = |ids: &[u32]| moves.iter().find(|m| m.ids == ids).map(|m| m.share).unwrap_or(0.0);
        assert!((share(&[1]) - 1.0 / 3.0).abs() < 1e-3, "{moves:?}");
        assert!((share(&[1, 3]) - 2.0 / 3.0).abs() < 1e-3, "{moves:?}");
        assert_eq!(share(&[]), 0.0);
        // Half way past the deadzone, straight right: D half the time.
        let (band, moves) = l.analog_moves(0.65, 0.0, &AnalogTune::default());
        assert_eq!(band, 1);
        let d = moves.iter().find(|m| m.ids == [3]).unwrap();
        let none = moves.iter().find(|m| m.slot == 0).unwrap();
        assert!((d.share - 0.5).abs() < 1e-3 && (none.share - 0.5).abs() < 1e-3, "{moves:?}");
        // Up-left wraps across 12 o'clock: W + A.
        let (x, y) = dir(-45.0, 1.0);
        let (_, moves) = l.analog_moves(x, y, &AnalogTune::default());
        assert!(moves.iter().any(|m| m.share > 0.99 && m.ids.len() == 2 && m.ids.contains(&1) && m.ids.contains(&7)), "{moves:?}");
        // An 8-way ring has its own diagonals: no pairs.
        let l8 = AreaLayout::circle_ways(8);
        let (x, y) = dir(30.0, 1.0);
        let (_, moves) = l8.analog_moves(x, y, &AnalogTune::default());
        assert!(moves.iter().all(|m| m.ids.len() <= 1), "{moves:?}");
        // The deadzone: the centre, whole.
        let (band, moves) = l.analog_moves(0.1, 0.0, &AnalogTune::default());
        assert_eq!((band, moves[0].ids.clone(), moves[0].share), (0, vec![0], 1.0));
    }

    #[test]
    fn a_squared_ring_measures_square() {
        let mut l = AreaLayout::circle_ways(8);
        // Diagonal at radius 0.35: outside the round 0.3 deadzone ...
        let (x, y) = dir(45.0, 0.35);
        assert_eq!(l.locate_id(x, y), 2);
        // ... inside it once squared (its corner reaches 0.42).
        assert!(l.set_edge_square(0, 1.0));
        assert_eq!(l.locate_id(x, y), 0);
        // Along the axis nothing changed.
        assert_eq!(l.locate_id(0.0, 0.31), 1);
        assert!(l.is_curved());
    }

    #[test]
    fn squaring_stops_where_the_rings_would_touch() {
        let mut l = AreaLayout::circle_ways(8);
        l.add_edge(0.35, false);
        assert!(l.set_edge_square(0, 1.0));
        let s = l.edges[0].square;
        assert!(s > 0.0 && s < 1.0, "partly squared: {s}");
        for k in 0..64 {
            let v = k as f32 / 64.0;
            assert!(l.edge_radius_at(0, v) < l.edge_radius_at(1, v), "nested at {v}");
        }
        // And a squared ring can't be dragged into its neighbour.
        l.move_edge(0, 0.34, false);
        for k in 0..64 {
            let v = k as f32 / 64.0;
            assert!(l.edge_radius_at(0, v) < l.edge_radius_at(1, v), "still nested at {v}");
        }
    }

    #[test]
    fn a_new_ring_takes_its_inner_rings_shape() {
        let mut l = AreaLayout::circle_ways(8);
        l.set_edge_square(0, 1.0);
        let (x, y) = dir(45.0, 0.8);
        l.add_edge_at(x, y, true);
        assert_eq!(l.edges.len(), 2);
        assert_eq!(l.edges[1].square, 1.0);
        // It passes through the point: the point sits on its border.
        assert!((l.border_coord_of(BorderRef::Edge(1), x, y) - l.edges[1].pos).abs() < 1e-5);
    }

    #[test]
    fn a_squared_ring_crossfades_along_its_shape() {
        let mut l = AreaLayout::circle_ways(8);
        l.set_edge_square(0, 1.0);
        l.set_gradient(BorderRef::Edge(0), Some(Gradient::with_width(0.2)), false);
        // On the square's corner the share is even, at radius 0.3·√2.
        let (x, y) = dir(45.0, 0.3 * std::f32::consts::SQRT_2);
        let w = l.weights(x, y, linear);
        let centre = w.iter().find(|c| c.id == 0).unwrap().weight;
        assert!((centre - 0.5).abs() < 1e-3, "{w:?}");
    }

    #[test]
    fn corner_rounding_squares_a_ring_differently() {
        let mut blend = AreaLayout::circle_ways(8);
        blend.set_edge_square(0, 0.5);
        let mut corners = blend.clone();
        assert!(corners.set_edge_corners(0, true));
        assert!((corners.edges[0].square - 0.5).abs() < 1e-6);
        // Along the axes both stay at 0.3; on the diagonal the rounded square
        // reaches further than the blend (its sides run flat up to the arc).
        let diag = 1.0 / 8.0;
        assert!((corners.edge_radius_at(0, 0.0) - 0.3).abs() < 1e-5);
        assert!(corners.edge_radius_at(0, diag) > blend.edge_radius_at(0, diag));
        // Square 0 is a circle, square 1 a square, in either mode.
        corners.set_edge_square(0, 0.0);
        assert!((corners.edge_radius_at(0, diag) - 0.3).abs() < 1e-4);
        corners.set_edge_square(0, 1.0);
        assert!((corners.edge_radius_at(0, diag) - 0.3 * std::f32::consts::SQRT_2).abs() < 1e-4);
        assert_eq!(AreaLayout::from_value(&corners.to_value()), Some(corners));
    }

    #[test]
    fn a_rounded_cell_shares_with_equal_pressure_and_takes_its_circle_with_more() {
        let mut l = AreaLayout::default_for(Shape::Rect);
        assert!(!l.is_bubbly());
        l.set_cell_round(0, 1.0, false); // the middle: a circle of the square's area
        assert!(l.is_bubbly());
        assert_eq!(l.locate_id(0.1, 0.1), 0);
        // Equal pressure: the overlap with a neighbour is shared (≈ 0.354) ...
        assert_eq!(l.locate_id(0.345, 0.0), 0);
        assert_eq!(l.locate_id(0.365, 0.0), 3);
        // ... its circle reaches the diagonal ...
        assert_eq!(l.locate_id(0.26, 0.26), 0);
        // ... and the corners it gives up go to the plain cells, not back to it.
        assert_ne!(l.locate_id(0.3, 0.3), 0);
        assert_ne!(l.locate_id(0.29, 0.31), 0);
        // Under pressure it takes its whole circle, r = (1/3)·2/√π ≈ 0.376.
        l.set_cell_pressure(0, 1.0, false);
        assert_eq!(l.locate_id(0.37, 0.0), 0);
        assert_eq!(l.locate_id(0.0, -0.37), 0);
        assert_eq!(l.locate_id(0.38, 0.0), 3);
        assert_ne!(l.locate_id(0.3, 0.3), 0);
        // Square cells away from it keep their plain borders.
        assert_eq!(l.locate_id(0.9, 0.34), 2);
        assert_eq!(l.locate_id(0.9, 0.32), 3);
    }

    #[test]
    fn rounded_neighbours_meet_in_flat_seams() {
        let mut l = AreaLayout::default_for(Shape::Rect);
        for id in 0..9 {
            l.set_cell_round(id, 1.0, false);
        }
        // Equal bubbles: the seam between the middle and its right neighbour
        // is straight, back on x = 1/3.
        for y in [-0.2f32, 0.0, 0.2] {
            assert_eq!(l.locate_id(0.32, y), 0, "y {y}");
            assert_eq!(l.locate_id(0.35, y), 3, "y {y}");
        }
    }

    #[test]
    fn a_gradient_crossfades_across_a_bubble_seam() {
        let mut l = AreaLayout::default_for(Shape::Rect);
        l.set_cell_round(0, 1.0, false);
        l.set_cell_pressure(0, 1.0, false);
        l.set_gradient(BorderRef::Cut { band: 1, cut: 1 }, Some(Gradient::with_width(0.1)), false);
        // The bubble's edge on the axis: its radius.
        let seam = (1.0 / 3.0) * 2.0 / std::f32::consts::PI.sqrt();
        let w = l.weights(seam, 0.0, linear);
        let mid = w.iter().find(|c| c.id == 0).map(|c| c.weight).unwrap_or(0.0);
        assert!((mid - 0.5).abs() < 1e-2, "{w:?}");
        let w = l.weights(seam - 0.025, 0.0, linear);
        let mid = w.iter().find(|c| c.id == 0).map(|c| c.weight).unwrap_or(0.0);
        assert!((mid - 0.75).abs() < 2e-2, "{w:?}");
        assert!((w.iter().map(|c| c.weight).sum::<f32>() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn symmetric_rounding_rounds_the_mirror_cells_and_survives_json() {
        let mut l = AreaLayout::default_for(Shape::Rect);
        l.set_cell_round(1, 0.5, true); // top-middle → also bottom-middle
        l.set_cell_pressure(1, 0.25, true);
        assert_eq!(l.cell_round_of(5), 0.5);
        assert_eq!(l.cell_pressure_of(5), 0.25);
        assert_eq!(AreaLayout::from_value(&l.to_value()), Some(l.clone()));
        // Removing the cell forgets its rounding.
        let j = l.find_cut(0, -1.0 / 3.0).unwrap();
        l.remove_cut(0, j, false, |_| false);
        assert!(l.cell_round.iter().all(|(id, _)| l.find_cell(*id).is_some()));
        assert!(l.cell_pressure.iter().all(|(id, _)| l.find_cell(*id).is_some()));
    }

    #[test]
    fn region_points_sit_where_the_cell_is() {
        let l = AreaLayout::circle_ways(8);
        let (band, index) = l.find_cell(1).unwrap();
        let [x, y] = l.region_point(Region::Cell { band, index }, 0.5, 0.5);
        assert!(x.abs() < 1e-4 && (y - 0.65).abs() < 1e-4, "{x} {y}");
        assert_eq!(l.locate_id(x, y), 1);
    }

    #[test]
    fn a_symmetric_gradient_covers_the_mirror_set() {
        let mut l = AreaLayout::circle_ways(8);
        l.set_gradient(BorderRef::Cut { band: 1, cut: 0 }, Some(Gradient::with_width(0.05)), true);
        let graded = l.bands[1].cuts.iter().filter(|c| c.gradient.is_some()).count();
        assert_eq!(graded, 4, "22.5°, 157.5°, 202.5°, 337.5°");
    }
}
