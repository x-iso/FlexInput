//! Shared geometry + dynamic-port naming for `module.touch_zones`.
//!
//! Both the engine (`eval.rs`) and the UI (`viewer.rs`) depend on this so zone
//! math and the port id scheme stay in lock-step — the UI generates the pins and
//! draws the field, the engine resolves the same ids back to zone rects.
//!
//! Coordinate spaces:
//!   * Pad space is normalized to `[-1, 1]` on the AutoMap bus (see
//!     `sdl_backend.rs` touch pins). Use [`pad_to_unit`] to convert.
//!   * Zone math here is all in unit space `[0, 1]`, y-down (0 = top edge),
//!     which matches how the field widget is drawn.
//!
//! Divider edges (`col_edges` / `row_edges`) are the INTERIOR split positions in
//! `(0, 1)`, kept sorted ascending. `cols = col_edges.len() + 1`, likewise rows.

/// Component carried by a per-zone output port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneComp {
    /// Local X within the zone, `0..1` (left→right).
    X,
    /// Local Y within the zone, `0..1` (top→bottom).
    Y,
    /// True while a touch point is inside the zone.
    Active,
}

/// Sentinel pin id for the AutoMap passthrough output (slot 0), matching the
/// AutoMap Splitter convention.
pub const PASS_PIN_ID: &str = "automap_pass";

pub fn cols(col_edges: &[f32]) -> usize {
    col_edges.len() + 1
}

pub fn rows(row_edges: &[f32]) -> usize {
    row_edges.len() + 1
}

/// Total zone count for the given divider edges.
pub fn zone_count(col_edges: &[f32], row_edges: &[f32]) -> usize {
    cols(col_edges) * rows(row_edges)
}

/// `(col, row)` for a zone index in row-major order.
pub fn zone_col_row(idx: usize, col_edges: &[f32]) -> (usize, usize) {
    let c = cols(col_edges);
    (idx % c, idx / c)
}

/// Normalized rect `(x0, y0, x1, y1)` in unit space for a zone index.
pub fn zone_rect(idx: usize, col_edges: &[f32], row_edges: &[f32]) -> (f32, f32, f32, f32) {
    let (col, row) = zone_col_row(idx, col_edges);
    let x0 = if col == 0 { 0.0 } else { col_edges[col - 1] };
    let x1 = if col == col_edges.len() { 1.0 } else { col_edges[col] };
    let y0 = if row == 0 { 0.0 } else { row_edges[row - 1] };
    let y1 = if row == row_edges.len() { 1.0 } else { row_edges[row] };
    (x0, y0, x1, y1)
}

/// Map a pad coordinate in `[-1, 1]` to unit space `[0, 1]`.
pub fn pad_to_unit(v: f32) -> f32 {
    ((v + 1.0) * 0.5).clamp(0.0, 1.0)
}

/// Convert a pad-space touch point to field unit space.
///
/// The bus convention is `[-1, 1]` with **+Y = up** (established by the HID touch
/// parser in `gyro.rs`, which inverts the raw top-down sensor). The field is
/// drawn y-down (0 = top edge), so X passes straight through and Y is flipped —
/// touching the top of the pad then lands at the top of the field. Both the
/// engine and UI call this so zone hit-testing and the drawn dot stay in sync.
pub fn pad_point_to_unit(px: f32, py: f32) -> (f32, f32) {
    (pad_to_unit(px), 1.0 - pad_to_unit(py))
}

/// Locate a unit-space point to a zone, returning `(zone_idx, local_x, local_y)`
/// with local coordinates in `0..1` relative to the zone's own rect.
pub fn locate_unit(x: f32, y: f32, col_edges: &[f32], row_edges: &[f32]) -> (usize, f32, f32) {
    let col = bucket(x, col_edges);
    let row = bucket(y, row_edges);
    let idx = row * cols(col_edges) + col;
    let (x0, y0, x1, y1) = zone_rect(idx, col_edges, row_edges);
    let lx = if x1 > x0 { ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) } else { 0.0 };
    let ly = if y1 > y0 { ((y - y0) / (y1 - y0)).clamp(0.0, 1.0) } else { 0.0 };
    (idx, lx, ly)
}

/// Bucket a coordinate against sorted interior edges → slot index `0..edges.len()`.
fn bucket(v: f32, edges: &[f32]) -> usize {
    let mut slot = 0;
    for &e in edges {
        if v >= e { slot += 1; } else { break; }
    }
    slot
}

// ─────────────────────────────────────────────────────────────────────────────
// Hierarchical zone tree (BSP)
//
// The legacy grid (`col_edges`/`row_edges`) only supports full-width/height cuts.
// The tree supports PARTIAL dividers: a split node divides only its OWN sub-rect,
// so a divider line spans just that subtree — you can subdivide one zone without
// cutting across the whole pad, and removing a divider merges only the zones under
// it. Each leaf carries a STABLE id so mapping-card `(field, zone)` bindings
// survive edits. A pure grid migrates losslessly (`ZoneNode::from_grid`) with leaf
// ids == the old row-major indices, so existing patches keep working.
// ─────────────────────────────────────────────────────────────────────────────

/// Split orientation. `V` = a vertical divider line (splits the X extent into
/// left/right); `H` = a horizontal divider (splits the Y extent into top/bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis { V, H }

/// A binary space-partition of the pad into zones. `t` is the ABSOLUTE unit-space
/// position of the divider along its axis (kept within the node's sub-rect).
#[derive(Debug, Clone, PartialEq)]
pub enum ZoneNode {
    /// A zone with a stable id.
    Leaf(u32),
    /// A divider at `t`; child `a` is the left/top side, `b` the right/bottom.
    Split { axis: Axis, t: f32, a: Box<ZoneNode>, b: Box<ZoneNode> },
}

/// A divider line materialized for drawing / hit-testing. For a `V` split the line
/// is vertical at `x = pos`, spanning `y ∈ [span_lo, span_hi]`; for `H` it's
/// horizontal at `y = pos`. `[lo, hi]` is the draggable range of `pos` along the
/// split axis (the subtree's extent). `path` navigates to the split node
/// (0 = a, 1 = b).
#[derive(Debug, Clone, PartialEq)]
pub struct Divider {
    pub axis: Axis,
    pub pos: f32,
    pub span_lo: f32,
    pub span_hi: f32,
    pub lo: f32,
    pub hi: f32,
    pub path: Vec<u8>,
}

impl ZoneNode {
    /// Build a tree from the legacy grid, assigning each leaf the row-major grid
    /// index (`row * cols + col`) so migrated cards stay bound to the same zones.
    pub fn from_grid(col_edges: &[f32], row_edges: &[f32]) -> ZoneNode {
        let ncol = cols(col_edges);
        // One row band split into its columns (peel one column per V split).
        fn band(row: usize, ncol: usize, c0: usize, col_edges: &[f32]) -> ZoneNode {
            if c0 + 1 == ncol {
                return ZoneNode::Leaf((row * ncol + c0) as u32);
            }
            ZoneNode::Split {
                axis: Axis::V,
                t: col_edges[c0],
                a: Box::new(ZoneNode::Leaf((row * ncol + c0) as u32)),
                b: Box::new(band(row, ncol, c0 + 1, col_edges)),
            }
        }
        fn rows_rec(r0: usize, nrow: usize, ncol: usize, col_edges: &[f32], row_edges: &[f32]) -> ZoneNode {
            if r0 + 1 == nrow {
                return band(r0, ncol, 0, col_edges);
            }
            ZoneNode::Split {
                axis: Axis::H,
                t: row_edges[r0],
                a: Box::new(band(r0, ncol, 0, col_edges)),
                b: Box::new(rows_rec(r0 + 1, nrow, ncol, col_edges, row_edges)),
            }
        }
        rows_rec(0, rows(row_edges), ncol, col_edges, row_edges)
    }

    /// Every zone as `(id, [x0, y0, x1, y1])` in unit space, in traversal order.
    pub fn zones(&self) -> Vec<(u32, [f32; 4])> {
        let mut out = Vec::new();
        self.collect(0.0, 0.0, 1.0, 1.0, &mut out);
        out
    }

    fn collect(&self, x0: f32, y0: f32, x1: f32, y1: f32, out: &mut Vec<(u32, [f32; 4])>) {
        match self {
            ZoneNode::Leaf(id) => out.push((*id, [x0, y0, x1, y1])),
            ZoneNode::Split { axis: Axis::V, t, a, b } => {
                let xt = t.clamp(x0, x1);
                a.collect(x0, y0, xt, y1, out);
                b.collect(xt, y0, x1, y1, out);
            }
            ZoneNode::Split { axis: Axis::H, t, a, b } => {
                let yt = t.clamp(y0, y1);
                a.collect(x0, y0, x1, yt, out);
                b.collect(x0, yt, x1, y1, out);
            }
        }
    }

    /// Locate a unit-space point → `(zone_id, local_x, local_y)` within its rect.
    pub fn locate(&self, x: f32, y: f32) -> (u32, f32, f32) {
        self.locate_rec(x, y, 0.0, 0.0, 1.0, 1.0)
    }

    fn locate_rec(&self, x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> (u32, f32, f32) {
        match self {
            ZoneNode::Leaf(id) => {
                let lx = if x1 > x0 { ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) } else { 0.0 };
                let ly = if y1 > y0 { ((y - y0) / (y1 - y0)).clamp(0.0, 1.0) } else { 0.0 };
                (*id, lx, ly)
            }
            ZoneNode::Split { axis: Axis::V, t, a, b } => {
                let xt = t.clamp(x0, x1);
                if x < xt { a.locate_rec(x, y, x0, y0, xt, y1) }
                else { b.locate_rec(x, y, xt, y0, x1, y1) }
            }
            ZoneNode::Split { axis: Axis::H, t, a, b } => {
                let yt = t.clamp(y0, y1);
                if y < yt { a.locate_rec(x, y, x0, y0, x1, yt) }
                else { b.locate_rec(x, y, x0, yt, x1, y1) }
            }
        }
    }

    /// Unit rect of a zone by id, if present.
    pub fn zone_rect(&self, id: u32) -> Option<[f32; 4]> {
        self.zones().into_iter().find(|(zid, _)| *zid == id).map(|(_, r)| r)
    }

    /// All zone ids in traversal order.
    pub fn ids(&self) -> Vec<u32> {
        self.zones().into_iter().map(|(id, _)| id).collect()
    }

    pub fn zone_count(&self) -> usize {
        match self {
            ZoneNode::Leaf(_) => 1,
            ZoneNode::Split { a, b, .. } => a.zone_count() + b.zone_count(),
        }
    }

    /// Next free id (max existing + 1).
    pub fn next_id(&self) -> u32 {
        self.ids().into_iter().max().map(|m| m + 1).unwrap_or(0)
    }

    /// All divider lines with their spans + path, for drawing / hit-testing.
    pub fn dividers(&self) -> Vec<Divider> {
        let mut out = Vec::new();
        self.dividers_rec(0.0, 0.0, 1.0, 1.0, &mut Vec::new(), &mut out);
        out
    }

    fn dividers_rec(&self, x0: f32, y0: f32, x1: f32, y1: f32, path: &mut Vec<u8>, out: &mut Vec<Divider>) {
        if let ZoneNode::Split { axis, t, a, b } = self {
            match axis {
                Axis::V => {
                    let xt = t.clamp(x0, x1);
                    out.push(Divider { axis: Axis::V, pos: xt, span_lo: y0, span_hi: y1,
                        lo: x0, hi: x1, path: path.clone() });
                    path.push(0); a.dividers_rec(x0, y0, xt, y1, path, out); path.pop();
                    path.push(1); b.dividers_rec(xt, y0, x1, y1, path, out); path.pop();
                }
                Axis::H => {
                    let yt = t.clamp(y0, y1);
                    out.push(Divider { axis: Axis::H, pos: yt, span_lo: x0, span_hi: x1,
                        lo: y0, hi: y1, path: path.clone() });
                    path.push(0); a.dividers_rec(x0, y0, x1, yt, path, out); path.pop();
                    path.push(1); b.dividers_rec(x0, yt, x1, y1, path, out); path.pop();
                }
            }
        }
    }

    /// Subdivide the leaf `id`, keeping `id` on the a-side (left/top) and giving the
    /// b-side (right/bottom) a fresh id. Returns the new id, or None if not a leaf.
    pub fn subdivide(&mut self, id: u32, axis: Axis, t: f32) -> Option<u32> {
        self.subdivide_side(id, axis, t, false)
    }

    /// Subdivide the leaf `id`. When `new_low` the NEW (empty) zone takes the
    /// low side (left/top) and `id` keeps the high side (right/bottom) — used so a
    /// "+" on a zone's left/top edge adds the empty cell on THAT side, pushing the
    /// mapped content the other way. Returns the new id, or None if not a leaf.
    pub fn subdivide_side(&mut self, id: u32, axis: Axis, t: f32, new_low: bool) -> Option<u32> {
        let new_id = self.next_id();
        if self.subdivide_rec(id, axis, t, new_id, new_low) { Some(new_id) } else { None }
    }

    fn subdivide_rec(&mut self, id: u32, axis: Axis, t: f32, new_id: u32, new_low: bool) -> bool {
        match self {
            ZoneNode::Leaf(lid) if *lid == id => {
                let (lo, hi) = if new_low { (new_id, id) } else { (id, new_id) };
                *self = ZoneNode::Split {
                    axis, t,
                    a: Box::new(ZoneNode::Leaf(lo)),
                    b: Box::new(ZoneNode::Leaf(hi)),
                };
                true
            }
            ZoneNode::Leaf(_) => false,
            ZoneNode::Split { a, b, .. } =>
                a.subdivide_rec(id, axis, t, new_id, new_low) || b.subdivide_rec(id, axis, t, new_id, new_low),
        }
    }

    /// Navigate to the split at `path` and set its `t`. Returns whether it applied.
    pub fn set_divider_t(&mut self, path: &[u8], t: f32) -> bool {
        match self {
            ZoneNode::Split { t: cur, a, b, .. } => match path.split_first() {
                None => { *cur = t; true }
                Some((0, rest)) => a.set_divider_t(rest, t),
                Some((_, rest)) => b.set_divider_t(rest, t),
            },
            ZoneNode::Leaf(_) => false,
        }
    }

    /// The position that recentres the divider at `path` between its immediate
    /// same-axis neighbours — the borders that actually bound the two zones it
    /// separates — so a double-click equalises those two zones (what the flat
    /// `col_edges`/`row_edges` editor does with `(edge[i-1]+edge[i+1])/2`).
    ///
    /// A divider's own `[lo, hi]` draggable range is its *parent node's* span,
    /// which in this right-leaning tree is often far wider than the gap between
    /// its visible neighbours — so the naïve `(lo+hi)/2` overshoots and squashes
    /// the next zone. This walks the flattened dividers, keeps only same-axis
    /// ones whose perpendicular span overlaps, and centres between the nearest
    /// on each side (falling back to the field edge / parent span). `None` if
    /// `path` isn't a split.
    pub fn centered_divider_pos(&self, path: &[u8]) -> Option<f32> {
        let all = self.dividers();
        let me = all.iter().find(|d| d.path == path)?;
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for d in &all {
            if d.path == me.path || d.axis != me.axis {
                continue;
            }
            // Only neighbours whose perpendicular span overlaps ours bound us
            // (a divider in another row/column can't squash this one).
            if d.span_lo >= me.span_hi - 1e-4 || d.span_hi <= me.span_lo + 1e-4 {
                continue;
            }
            if d.pos < me.pos {
                lo = lo.max(d.pos);
            } else if d.pos > me.pos {
                hi = hi.min(d.pos);
            }
        }
        // Never leave the divider's own parent span.
        Some(((lo + hi) * 0.5).clamp(me.lo, me.hi))
    }

    /// Collapse the split at `path` into a single leaf. `inheritor` (if it is one
    /// of the merged ids) is kept; otherwise the smallest merged id wins. Returns
    /// `(kept_id, removed_ids)` — the caller reassigns/drops the removed zones'
    /// mapping cards. No-op (None) if `path` doesn't point at a split.
    pub fn remove_split(&mut self, path: &[u8], inheritor: Option<u32>) -> Option<(u32, Vec<u32>)> {
        let target = self.node_at_mut(path)?;
        if let ZoneNode::Leaf(_) = target { return None; }
        let mut merged: Vec<u32> = target.ids();
        merged.sort_unstable();
        let kept = inheritor.filter(|i| merged.contains(i))
            .unwrap_or_else(|| *merged.first().unwrap());
        let removed: Vec<u32> = merged.into_iter().filter(|i| *i != kept).collect();
        *target = ZoneNode::Leaf(kept);
        Some((kept, removed))
    }

    fn node_at_mut(&mut self, path: &[u8]) -> Option<&mut ZoneNode> {
        match path.split_first() {
            None => Some(self),
            Some((step, rest)) => match self {
                ZoneNode::Split { a, b, .. } =>
                    if *step == 0 { a.node_at_mut(rest) } else { b.node_at_mut(rest) },
                ZoneNode::Leaf(_) => None,
            },
        }
    }

    /// Serialize to JSON: `{"id":N}` for a leaf, `{"axis":"v"|"h","t":..,"a":..,"b":..}`.
    pub fn to_value(&self) -> serde_json::Value {
        match self {
            ZoneNode::Leaf(id) => serde_json::json!({ "id": id }),
            ZoneNode::Split { axis, t, a, b } => serde_json::json!({
                "axis": if *axis == Axis::V { "v" } else { "h" },
                "t": t,
                "a": a.to_value(),
                "b": b.to_value(),
            }),
        }
    }

    /// Parse from the JSON produced by [`to_value`]. Returns None on malformed data.
    pub fn from_value(v: &serde_json::Value) -> Option<ZoneNode> {
        if let Some(id) = v.get("id").and_then(|x| x.as_u64()) {
            return Some(ZoneNode::Leaf(id as u32));
        }
        let axis = match v.get("axis")?.as_str()? {
            "v" => Axis::V, "h" => Axis::H, _ => return None,
        };
        let t = v.get("t")?.as_f64()? as f32;
        let a = ZoneNode::from_value(v.get("a")?)?;
        let b = ZoneNode::from_value(v.get("b")?)?;
        Some(ZoneNode::Split { axis, t, a: Box::new(a), b: Box::new(b) })
    }
}

/// A parsed dynamic output port. Ports are namespaced by FIELD so split mode
/// (two independent pads — one per touch point) can coexist on one node. In
/// single-field mode only field 0 exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// Per-zone X / Y / Active.
    Zone { field: usize, idx: usize, comp: ZoneComp },
    /// Per-field touchpad click (whole pad is a button on DS4/DualSense/Steam).
    Click { field: usize },
}

/// Port id for a zone component, e.g. `zone_pin_id(0, 2, ZoneComp::X) == "f0z2_x"`.
pub fn zone_pin_id(field: usize, idx: usize, comp: ZoneComp) -> String {
    let s = match comp {
        ZoneComp::X => "x",
        ZoneComp::Y => "y",
        ZoneComp::Active => "act",
    };
    format!("f{field}z{idx}_{s}")
}

/// Port id for a field's click, e.g. `click_pin_id(1) == "f1c"`.
pub fn click_pin_id(field: usize) -> String {
    format!("f{field}c")
}

/// Parse a dynamic port id into a [`Pin`]. Returns `None` for the passthrough
/// sentinel or any unrecognized id.
pub fn parse_pin(id: &str) -> Option<Pin> {
    let rest = id.strip_prefix('f')?;
    // Click: "f{field}c"
    if let Some(fnum) = rest.strip_suffix('c') {
        let field: usize = fnum.parse().ok()?;
        return Some(Pin::Click { field });
    }
    // Zone: "f{field}z{idx}_{comp}"
    let (fnum, zrest) = rest.split_once('z')?;
    let field: usize = fnum.parse().ok()?;
    let (num, comp) = zrest.split_once('_')?;
    let idx: usize = num.parse().ok()?;
    let comp = match comp {
        "x" => ZoneComp::X,
        "y" => ZoneComp::Y,
        "act" => ZoneComp::Active,
        _ => return None,
    };
    Some(Pin::Zone { field, idx, comp })
}

/// Human-readable zone pin label. In single-field mode (`field == 0` and
/// `single == true`) the field prefix is dropped: `"Z0 X"`; otherwise `"A0 X"` /
/// `"B0 X"` using a per-field letter.
pub fn zone_pin_label(field: usize, idx: usize, comp: ZoneComp, single: bool) -> String {
    let pre = field_prefix(field, single);
    match comp {
        ZoneComp::X => format!("{pre}{idx} X"),
        ZoneComp::Y => format!("{pre}{idx} Y"),
        ZoneComp::Active => format!("{pre}{idx} •"),
    }
}

/// Human-readable click pin label, e.g. `"A ⊙"` / `"B ⊙"` (or `"Click"` single).
pub fn click_pin_label(field: usize, single: bool) -> String {
    if single {
        "Click".to_string()
    } else {
        format!("{} ⊙", field_letter(field))
    }
}

/// Zone-label prefix: `"Z"` when single-field, else the field's letter (`A`, `B`…).
fn field_prefix(field: usize, single: bool) -> String {
    if single { "Z".to_string() } else { format!("{}", field_letter(field)) }
}

/// Field letter: 0→A, 1→B, … (falls back to the number past 'Z').
pub fn field_letter(field: usize) -> char {
    char::from_u32('A' as u32 + field as u32).filter(|c| *c <= 'Z').unwrap_or('?')
}

/// Insert a new interior divider at `pos`, keeping the list sorted and rejecting
/// near-duplicates (within `eps`). Returns true if inserted.
pub fn insert_edge(edges: &mut Vec<f32>, pos: f32, eps: f32) -> bool {
    let pos = pos.clamp(eps, 1.0 - eps);
    if edges.iter().any(|&e| (e - pos).abs() < eps) {
        return false;
    }
    edges.push(pos);
    edges.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    true
}

/// Evenly-spaced interior edges for `n` slots, e.g. `even_edges(3) == [1/3, 2/3]`.
pub fn even_edges(n: usize) -> Vec<f32> {
    if n <= 1 {
        return Vec::new();
    }
    (1..n).map(|i| i as f32 / n as f32).collect()
}

/// A relative grid edit on one field. `Insert*` splits the referenced column/row
/// at its midpoint (the original keeps its identity/wiring, the new half is
/// empty); `Remove*` deletes the referenced column/row, shifting the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridOp {
    InsertCol(usize),
    RemoveCol(usize),
    InsertRow(usize),
    RemoveRow(usize),
}

/// Apply a [`GridOp`] to a field's edges. Returns the new edges plus a map from
/// each SURVIVING old zone index to its new index (removed zones are absent), or
/// `None` if the op can't be applied (single row/col removal, or a split that
/// lands too close to an existing divider). Pure index/geometry math — the UI
/// layer uses this to remap port wiring so wires follow their zones.
pub fn apply_grid_op(
    op: GridOp,
    col: &[f32],
    row: &[f32],
) -> Option<(Vec<f32>, Vec<f32>, std::collections::HashMap<usize, usize>)> {
    let ncols_old = cols(col);
    let nrows_old = rows(row);
    let mut new_col = col.to_vec();
    let mut new_row = row.to_vec();
    let mut remap = std::collections::HashMap::new();
    match op {
        GridOp::InsertCol(c) => {
            let left = if c == 0 { 0.0 } else { col[c - 1] };
            let right = if c == ncols_old - 1 { 1.0 } else { col[c] };
            if !insert_edge(&mut new_col, (left + right) * 0.5, 0.04) { return None; }
            let ncols = ncols_old + 1;
            for idx in 0..ncols_old * nrows_old {
                let (oc, r) = (idx % ncols_old, idx / ncols_old);
                let nc = if oc > c { oc + 1 } else { oc };
                remap.insert(idx, r * ncols + nc);
            }
        }
        GridOp::RemoveCol(c) => {
            if ncols_old <= 1 { return None; }
            new_col.remove(if c < ncols_old - 1 { c } else { c - 1 });
            let ncols = ncols_old - 1;
            for idx in 0..ncols_old * nrows_old {
                let (oc, r) = (idx % ncols_old, idx / ncols_old);
                if oc == c { continue; }
                let nc = if oc > c { oc - 1 } else { oc };
                remap.insert(idx, r * ncols + nc);
            }
        }
        GridOp::InsertRow(rr) => {
            let top = if rr == 0 { 0.0 } else { row[rr - 1] };
            let bot = if rr == nrows_old - 1 { 1.0 } else { row[rr] };
            if !insert_edge(&mut new_row, (top + bot) * 0.5, 0.04) { return None; }
            for idx in 0..ncols_old * nrows_old {
                let (oc, r) = (idx % ncols_old, idx / ncols_old);
                let nr = if r > rr { r + 1 } else { r };
                remap.insert(idx, nr * ncols_old + oc);
            }
        }
        GridOp::RemoveRow(rr) => {
            if nrows_old <= 1 { return None; }
            new_row.remove(if rr < nrows_old - 1 { rr } else { rr - 1 });
            for idx in 0..ncols_old * nrows_old {
                let (oc, r) = (idx % ncols_old, idx / ncols_old);
                if r == rr { continue; }
                let nr = if r > rr { r - 1 } else { r };
                remap.insert(idx, nr * ncols_old + oc);
            }
        }
    }
    Some((new_col, new_row, remap))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_indexing() {
        let cols_e = vec![0.5];
        let rows_e = vec![0.5];
        assert_eq!(zone_count(&cols_e, &rows_e), 4);
        // Row-major: idx 0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right.
        assert_eq!(zone_col_row(0, &cols_e), (0, 0));
        assert_eq!(zone_col_row(3, &cols_e), (1, 1));
    }

    #[test]
    fn locate_maps_to_local() {
        let cols_e = vec![0.5];
        let rows_e = vec![0.5];
        // Point at (0.75, 0.25) → top-right zone (idx 1), local (0.5, 0.5).
        let (idx, lx, ly) = locate_unit(0.75, 0.25, &cols_e, &rows_e);
        assert_eq!(idx, 1);
        assert!((lx - 0.5).abs() < 1e-6);
        assert!((ly - 0.5).abs() < 1e-6);
    }

    #[test]
    fn tree_from_grid_matches_grid_indexing() {
        // 2×2 grid → tree must locate the same row-major ids as `locate_unit`.
        let cols_e = vec![0.5];
        let rows_e = vec![0.5];
        let tree = ZoneNode::from_grid(&cols_e, &rows_e);
        assert_eq!(tree.zone_count(), 4);
        let mut ids = tree.ids();
        ids.sort_unstable();
        assert_eq!(ids, vec![0, 1, 2, 3]);
        for (x, y) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
            let grid = locate_unit(x, y, &cols_e, &rows_e).0 as u32;
            let (tid, _, _) = tree.locate(x, y);
            assert_eq!(tid, grid, "tree id must match grid at ({x},{y})");
        }
        // A 3×1 row also matches (peels columns left→right).
        let t3 = ZoneNode::from_grid(&[0.33, 0.66], &[]);
        assert_eq!(t3.locate(0.1, 0.5).0, 0);
        assert_eq!(t3.locate(0.5, 0.5).0, 1);
        assert_eq!(t3.locate(0.9, 0.5).0, 2);
    }

    #[test]
    fn centered_divider_uses_immediate_neighbours() {
        // 3 columns at 0.2 / 0.8 (right-leaning tree). Centring the FIRST
        // divider must land between the left edge and the SECOND divider (0.4),
        // NOT the field midpoint 0.5 — that overshoot squashes the middle zone.
        let tree = ZoneNode::from_grid(&[0.2, 0.8], &[]);
        let d0 = tree.centered_divider_pos(&[]).unwrap();
        assert!((d0 - 0.4).abs() < 1e-4, "first divider centres at 0.4, got {d0}");
        // The second divider centres between the first divider and the right edge.
        let d1 = tree.centered_divider_pos(&[1]).unwrap();
        assert!((d1 - 0.6).abs() < 1e-4, "second divider centres at 0.6, got {d1}");
        // A path that isn't a split has no centred position.
        assert!(tree.centered_divider_pos(&[0]).is_none());

        // 2×2 grid: a column divider ignores the OTHER row's column divider
        // (non-overlapping perpendicular span) → centres at 0.5 in its band.
        let g = ZoneNode::from_grid(&[0.3], &[0.5]);
        let c = g.centered_divider_pos(&[0]).unwrap();
        assert!((c - 0.5).abs() < 1e-4, "column divider centres at 0.5, got {c}");
    }

    #[test]
    fn tree_subdivide_is_local_and_keeps_ids() {
        // Start with one zone, split it vertically at 0.5 → ids 0 (left) + 1 (right).
        let mut tree = ZoneNode::Leaf(0);
        let new = tree.subdivide(0, Axis::V, 0.5).unwrap();
        assert_eq!(new, 1);
        assert_eq!(tree.locate(0.25, 0.5).0, 0);
        assert_eq!(tree.locate(0.75, 0.5).0, 1);
        // Subdivide ONLY the right zone horizontally → the divider is partial
        // (spans only x∈[0.5,1]); the left zone is untouched.
        let new2 = tree.subdivide(1, Axis::H, 0.5).unwrap();
        assert_eq!(new2, 2);
        assert_eq!(tree.locate(0.25, 0.9).0, 0, "left zone unaffected by right-side split");
        assert_eq!(tree.locate(0.75, 0.25).0, 1);
        assert_eq!(tree.locate(0.75, 0.75).0, 2);
        // The horizontal divider spans only the right half.
        let hdiv = tree.dividers().into_iter().find(|d| d.axis == Axis::H).unwrap();
        assert!((hdiv.span_lo - 0.5).abs() < 1e-6 && (hdiv.span_hi - 1.0).abs() < 1e-6,
            "partial divider spans x∈[0.5,1], got [{},{}]", hdiv.span_lo, hdiv.span_hi);
    }

    #[test]
    fn tree_subdivide_side_puts_new_cell_low() {
        // new_low=true: the NEW empty cell takes the left/low side; the original
        // id (its mappings) keeps the right side.
        let mut tree = ZoneNode::Leaf(0);
        let new = tree.subdivide_side(0, Axis::V, 0.5, true).unwrap();
        assert_eq!(tree.locate(0.25, 0.5).0, new, "left half is the new empty cell");
        assert_eq!(tree.locate(0.75, 0.5).0, 0, "right half keeps the original id/mappings");
    }

    #[test]
    fn tree_remove_split_merges_and_reports_removed() {
        let mut tree = ZoneNode::from_grid(&[0.5], &[]); // ids 0 (left), 1 (right)
        // Remove the only divider → the two zones merge; id 0 kept, 1 removed.
        let div = tree.dividers()[0].clone();
        let (kept, removed) = tree.remove_split(&div.path, None).unwrap();
        assert_eq!(kept, 0);
        assert_eq!(removed, vec![1]);
        assert_eq!(tree.zone_count(), 1);
        assert_eq!(tree.locate(0.9, 0.5).0, 0, "merged zone covers the whole pad");
    }

    #[test]
    fn tree_remove_split_honors_inheritor() {
        let mut tree = ZoneNode::from_grid(&[0.5], &[]);
        let div = tree.dividers()[0].clone();
        // Keep zone 1 instead of the default-smallest 0.
        let (kept, removed) = tree.remove_split(&div.path, Some(1)).unwrap();
        assert_eq!(kept, 1);
        assert_eq!(removed, vec![0]);
    }

    #[test]
    fn tree_json_roundtrip() {
        let mut tree = ZoneNode::from_grid(&[0.4], &[0.6]);
        tree.subdivide(0, Axis::V, 0.2);
        let v = tree.to_value();
        let back = ZoneNode::from_value(&v).unwrap();
        assert_eq!(tree, back);
    }

    #[test]
    fn pin_id_roundtrip() {
        for field in [0usize, 1] {
            for comp in [ZoneComp::X, ZoneComp::Y, ZoneComp::Active] {
                let id = zone_pin_id(field, 7, comp);
                assert_eq!(parse_pin(&id), Some(Pin::Zone { field, idx: 7, comp }));
            }
            let cid = click_pin_id(field);
            assert_eq!(parse_pin(&cid), Some(Pin::Click { field }));
        }
        assert_eq!(parse_pin(PASS_PIN_ID), None);
        // Ensure zone vs click disambiguation is robust (zone id ends in _x/_y/_act,
        // click id ends in 'c' with no 'z').
        assert_eq!(parse_pin("f0z10_act"), Some(Pin::Zone { field: 0, idx: 10, comp: ZoneComp::Active }));
    }

    #[test]
    fn pad_conversion() {
        assert!((pad_to_unit(-1.0) - 0.0).abs() < 1e-6);
        assert!((pad_to_unit(0.0) - 0.5).abs() < 1e-6);
        assert!((pad_to_unit(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pad_point_flips_y_only() {
        // +Y = up on the bus → top of pad (py=+1) maps to top of field (uy=0).
        let (ux, uy) = pad_point_to_unit(1.0, 1.0);
        assert!((ux - 1.0).abs() < 1e-6, "x passes through");
        assert!((uy - 0.0).abs() < 1e-6, "y flips: pad-top → field-top");
        let (_, uy2) = pad_point_to_unit(0.0, -1.0);
        assert!((uy2 - 1.0).abs() < 1e-6, "pad-bottom → field-bottom");
    }

    #[test]
    fn insert_col_preserves_and_shifts() {
        // 2×2 grid (cols=[0.5], rows=[0.5]): indices 0 1 / 2 3.
        let col = vec![0.5];
        let row = vec![0.5];
        // Split column 0 → new col 1 is empty; old col-1 zones shift right.
        let (nc, nr, remap) = apply_grid_op(GridOp::InsertCol(0), &col, &row).unwrap();
        assert_eq!(cols(&nc), 3);
        assert_eq!(rows(&nr), 2);
        // Old (col0,row0)=0 → stays new col0 → 0. Old (col1,row0)=1 → new col2 → 2.
        assert_eq!(remap[&0], 0);
        assert_eq!(remap[&1], 2);
        // Old (col0,row1)=2 → new (col0,row1)=3. Old (col1,row1)=3 → new (col2,row1)=5.
        assert_eq!(remap[&2], 3);
        assert_eq!(remap[&3], 5);
    }

    #[test]
    fn remove_col_drops_and_shifts() {
        // 3 cols × 1 row: indices 0 1 2.
        let col = vec![1.0 / 3.0, 2.0 / 3.0];
        let row: Vec<f32> = vec![];
        let (nc, _nr, remap) = apply_grid_op(GridOp::RemoveCol(1), &col, &row).unwrap();
        assert_eq!(cols(&nc), 2);
        assert_eq!(remap.get(&1), None, "removed column's zone is dropped");
        assert_eq!(remap[&0], 0);
        assert_eq!(remap[&2], 1, "column after the removed one shifts left");
    }

    #[test]
    fn remove_last_col_and_single_guard() {
        let col = vec![0.5];
        let row: Vec<f32> = vec![];
        // Remove the last (rightmost) column of a 2×1 grid.
        let (nc, _nr, remap) = apply_grid_op(GridOp::RemoveCol(1), &col, &row).unwrap();
        assert_eq!(cols(&nc), 1);
        assert_eq!(remap[&0], 0);
        assert_eq!(remap.get(&1), None);
        // Cannot remove the only column.
        assert!(apply_grid_op(GridOp::RemoveCol(0), &[], &[]).is_none());
    }

    #[test]
    fn insert_row_shifts_down() {
        // 2 cols × 2 rows: 0 1 / 2 3. Insert row 0 → new empty row 1.
        let (nc, nr, remap) = apply_grid_op(GridOp::InsertRow(0), &[0.5], &[0.5]).unwrap();
        assert_eq!(cols(&nc), 2);
        assert_eq!(rows(&nr), 3);
        // Row-0 zones stay; row-1 zones move to row 2.
        assert_eq!(remap[&0], 0);
        assert_eq!(remap[&1], 1);
        assert_eq!(remap[&2], 4);
        assert_eq!(remap[&3], 5);
    }

    #[test]
    fn even_and_insert() {
        assert_eq!(even_edges(1), Vec::<f32>::new());
        let e = even_edges(3);
        assert!((e[0] - 1.0 / 3.0).abs() < 1e-6 && (e[1] - 2.0 / 3.0).abs() < 1e-6);
        let mut edges = vec![0.5];
        assert!(insert_edge(&mut edges, 0.25, 0.02));
        assert!(!insert_edge(&mut edges, 0.505, 0.02)); // too close to 0.5
        assert_eq!(edges, vec![0.25, 0.5]);
    }
}
