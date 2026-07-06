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
