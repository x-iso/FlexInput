//! Auto-arrange: lay a whole patch out so its signal reads left to right.
//!
//! A layered layout (after Sugiyama): each module goes in a column by how far down the
//! signal path it is - a patch's inputs (devices, a sub-patch's inlets) in the first, its
//! outputs (devices, outlets) in the last, a knob or other source of its own just before the
//! module it feeds - the columns are ordered top to bottom so wires cross as little as they
//! can, and each module is put at the height its wires come in at, so a wire from one pin to
//! the next runs straight. Separate groups of modules stack one under another; modules wired
//! to nothing go in a row at the bottom. On the layout grid everything lands on blocks.
//!
//! [`arrange`] is the whole of it, on frames and pin heights alone: the canvas gathers those
//! (`Canvas::auto_arrange`) and moves the nodes to what it returns.

use std::collections::HashMap;

use egui_snarl::NodeId;

/// A module as the layout sees it.
#[derive(Clone, Debug)]
pub(crate) struct ArrangeNode {
    pub id: NodeId,
    /// Its frame where it is now; the layout keeps its size.
    pub frame: egui::Rect,
    /// Where the signal comes into the patch (a device's inputs, a sub-patch's inlet): the
    /// first column, never pulled along to what it feeds.
    pub entry: bool,
    /// Where the signal leaves it (a device's outputs, an outlet): the last column.
    pub exit: bool,
}

/// A wire between two modules, by the heights of its pins below the tops of their frames.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ArrangeWire {
    pub from: NodeId,
    pub from_y: f32,
    pub to: NodeId,
    pub to_y: f32,
}

/// How far apart things go.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Spacing {
    /// Between columns: the room the wires between them run in.
    pub column: f32,
    /// Between modules in a column.
    pub row: f32,
    /// Between separate groups, and above the row of loose modules.
    pub group: f32,
    /// The grid's block, when modules snap to it.
    pub block: Option<f32>,
}

/// Where each module's frame goes (its new top-left), the patch as a whole kept where its
/// top-left corner was.
pub(crate) fn arrange(nodes: &[ArrangeNode], wires: &[ArrangeWire], sp: &Spacing) -> HashMap<NodeId, egui::Pos2> {
    let n = nodes.len();
    let mut out = HashMap::new();
    if n == 0 {
        return out;
    }
    let index: HashMap<NodeId, usize> = nodes.iter().enumerate().map(|(i, nd)| (nd.id, i)).collect();
    let edges: Vec<Edge> = wires
        .iter()
        .filter_map(|w| {
            let (a, b) = (*index.get(&w.from)?, *index.get(&w.to)?);
            (a != b).then_some(Edge { a, b, ay: w.from_y, by: w.to_y })
        })
        .collect();

    // Groups of modules wired together.
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for e in &edges {
        let (ra, rb) = (find(&mut parent, e.a), find(&mut parent, e.b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    let mut by_root: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        by_root.entry(r).or_default().push(i);
    }
    let top_left = |i: usize| (nodes[i].frame.min.y, nodes[i].frame.min.x);
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut loose: Vec<usize> = Vec::new();
    for members in by_root.into_values() {
        if members.len() == 1 {
            loose.push(members[0]);
        } else {
            groups.push(members);
        }
    }
    // Top to bottom as they were: the group that was highest stays on top.
    let first = |g: &[usize]| g.iter().map(|&i| top_left(i)).fold((f32::INFINITY, f32::INFINITY), |a, b| if b < a { b } else { a });
    groups.sort_by(|a, b| first(a).partial_cmp(&first(b)).unwrap_or(std::cmp::Ordering::Equal));
    loose.sort_by(|&a, &b| top_left(a).partial_cmp(&top_left(b)).unwrap_or(std::cmp::Ordering::Equal));

    let snap = |v: f32| sp.block.map_or(v, |b| (v / b).round() * b);
    let mut rel: Vec<egui::Pos2> = vec![egui::Pos2::ZERO; n];
    let mut y = 0.0f32;
    let mut width = 0.0f32;
    for g in &groups {
        let (placed, size) = layout_group(g, nodes, &edges, sp);
        for (i, p) in placed {
            rel[i] = egui::pos2(p.x, p.y + y);
        }
        y = snap(y + size.y + sp.group);
        width = width.max(size.x);
    }
    // The loose ones in rows, as wide as the rest (or a few modules across on their own).
    if !loose.is_empty() {
        let row_w = width.max(loose.iter().map(|&i| nodes[i].frame.width()).sum::<f32>().min(900.0));
        let (mut x, mut row_h) = (0.0f32, 0.0f32);
        for &i in &loose {
            let size = nodes[i].frame.size();
            if x > 0.0 && x + size.x > row_w {
                y = snap(y + row_h + sp.row);
                x = 0.0;
                row_h = 0.0;
            }
            rel[i] = egui::pos2(x, y);
            x = snap(x + size.x + sp.row);
            row_h = row_h.max(size.y);
        }
    }

    // Where the patch's top-left corner was, on a block centre on the grid.
    let origin = nodes.iter().fold(egui::pos2(f32::INFINITY, f32::INFINITY), |o, nd| o.min(nd.frame.min));
    let origin = match sp.block {
        Some(b) => egui::pos2(((origin.x / b - 0.5).round() + 0.5) * b, ((origin.y / b - 0.5).round() + 0.5) * b),
        None => origin,
    };
    for (i, nd) in nodes.iter().enumerate() {
        out.insert(nd.id, origin + rel[i].to_vec2());
    }
    out
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    a: usize,
    b: usize,
    /// The pin heights on `a` and `b`.
    ay: f32,
    by: f32,
}

/// One group of modules wired together, laid out from (0, 0): where each goes, and how big
/// the group is.
fn layout_group(members: &[usize], nodes: &[ArrangeNode], edges: &[Edge], sp: &Spacing) -> (Vec<(usize, egui::Pos2)>, egui::Vec2) {
    let m = members.len();
    let local: HashMap<usize, usize> = members.iter().enumerate().map(|(k, &i)| (i, k)).collect();
    let frame = |k: usize| nodes[members[k]].frame;
    // This group's wires, in its own numbering.
    let mut group_edges: Vec<Edge> = edges
        .iter()
        .filter_map(|e| Some(Edge { a: *local.get(&e.a)?, b: *local.get(&e.b)?, ay: e.ay, by: e.by }))
        .collect();

    // A wire that runs back against the flow (a loop) is laid out as if it ran forward:
    // a depth-first walk from the left of the patch finds them.
    let mut out_of: Vec<Vec<usize>> = vec![Vec::new(); m];
    for (ix, e) in group_edges.iter().enumerate() {
        out_of[e.a].push(ix);
    }
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|&a, &b| {
        let (fa, fb) = (frame(a).min, frame(b).min);
        (fa.x, fa.y).partial_cmp(&(fb.x, fb.y)).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut state = vec![0u8; m]; // 0 unseen, 1 on the walk, 2 done
    let mut reversed = vec![false; group_edges.len()];
    for &start in &order {
        if state[start] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        state[start] = 1;
        while let Some(&mut (v, ref mut next)) = stack.last_mut() {
            if let Some(&ix) = out_of[v].get(*next) {
                *next += 1;
                let w = group_edges[ix].b;
                match state[w] {
                    0 => {
                        state[w] = 1;
                        stack.push((w, 0));
                    }
                    1 => reversed[ix] = true,
                    _ => {}
                }
            } else {
                state[v] = 2;
                stack.pop();
            }
        }
    }
    for (e, &rev) in group_edges.iter_mut().zip(&reversed) {
        if rev {
            *e = Edge { a: e.b, b: e.a, ay: e.by, by: e.ay };
        }
    }
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); m];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); m];
    for (ix, e) in group_edges.iter().enumerate() {
        succs[e.a].push(ix);
        preds[e.b].push(ix);
    }

    // Columns: the longest way in from the patch's inputs.
    let mut indeg: Vec<usize> = preds.iter().map(Vec::len).collect();
    let mut topo: Vec<usize> = (0..m).filter(|&k| indeg[k] == 0).collect();
    let mut at = 0;
    while at < topo.len() {
        let v = topo[at];
        at += 1;
        for &ix in &succs[v] {
            let w = group_edges[ix].b;
            indeg[w] -= 1;
            if indeg[w] == 0 {
                topo.push(w);
            }
        }
    }
    let mut layer = vec![0usize; m];
    for &v in &topo {
        for &ix in &succs[v] {
            let w = group_edges[ix].b;
            layer[w] = layer[w].max(layer[v] + 1);
        }
    }
    // A source of the patch's own (a knob, a constant) sits just before what it feeds,
    // not in the first column with the devices.
    for &v in topo.iter().rev() {
        if preds[v].is_empty() && !nodes[members[v]].entry && !succs[v].is_empty() {
            let nearest = succs[v].iter().map(|&ix| layer[group_edges[ix].b]).min().unwrap_or(1);
            layer[v] = nearest.saturating_sub(1);
        }
    }
    let last = layer.iter().copied().max().unwrap_or(0);
    // The patch's outputs together in the last column.
    let last = if (0..m).any(|k| nodes[members[k]].exit && succs[k].is_empty() && layer[k] < last) || last == 0 {
        last.max(1)
    } else {
        last
    };
    for k in 0..m {
        if nodes[members[k]].exit && succs[k].is_empty() {
            layer[k] = last;
        }
    }
    let layers_n = layer.iter().copied().max().unwrap_or(0) + 1;
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); layers_n];
    for k in 0..m {
        layers[layer[k]].push(k);
    }
    for l in &mut layers {
        l.sort_by(|&a, &b| frame(a).center().y.partial_cmp(&frame(b).center().y).unwrap_or(std::cmp::Ordering::Equal));
    }

    // Order within each column: by where what it is wired to sits (barycentres, swept
    // forward and back), a pin's own height in its module counting a little.
    let mut pos = vec![0.0f32; m];
    let renumber = |layers: &Vec<Vec<usize>>, pos: &mut Vec<f32>| {
        for l in layers {
            for (i, &k) in l.iter().enumerate() {
                pos[k] = i as f32;
            }
        }
    };
    renumber(&layers, &mut pos);
    let frac = |k: usize, y: f32| (y / frame(k).height().max(1.0)).clamp(0.0, 0.99);
    for sweep in 0..8 {
        let forward = sweep % 2 == 0;
        let order: Vec<usize> = if forward { (1..layers_n).collect() } else { (0..layers_n.saturating_sub(1)).rev().collect() };
        for l in order {
            let mut keyed: Vec<(f32, usize)> = layers[l]
                .iter()
                .map(|&k| {
                    let near: Vec<f32> = if forward {
                        preds[k].iter().map(|&ix| { let e = group_edges[ix]; pos[e.a] + frac(e.a, e.ay) }).collect()
                    } else {
                        succs[k].iter().map(|&ix| { let e = group_edges[ix]; pos[e.b] + frac(e.b, e.by) }).collect()
                    };
                    let key = if near.is_empty() { pos[k] } else { near.iter().sum::<f32>() / near.len() as f32 };
                    (key, k)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            layers[l] = keyed.into_iter().map(|(_, k)| k).collect();
            for (i, &k) in layers[l].iter().enumerate() {
                pos[k] = i as f32;
            }
        }
    }

    // Columns side by side, each as wide as its widest module.
    let snap_up = |v: f32| sp.block.map_or(v, |b| (v / b - 1e-3).ceil() * b);
    let mut col_x = vec![0.0f32; layers_n];
    for l in 1..layers_n {
        let widest = layers[l - 1].iter().map(|&k| frame(k).width()).fold(0.0f32, f32::max);
        col_x[l] = snap_up(col_x[l - 1] + widest + sp.column);
    }

    // Heights: stacked to start with, then each module moved to where its wires come in
    // (the middle one where there are several), keeping the column's order and gaps -
    // forward from the inputs and back from the outputs, a few times over.
    let mut y = vec![0.0f32; m];
    for l in &layers {
        let mut top = 0.0f32;
        for &k in l {
            y[k] = top;
            top += frame(k).height() + sp.row;
        }
    }
    let place = |l: &[usize], want: &[f32], y: &mut Vec<f32>| {
        let fitted = fit_in_order(want, &l.iter().map(|&k| frame(k).height()).collect::<Vec<_>>(), sp.row);
        for (&k, v) in l.iter().zip(fitted) {
            y[k] = v;
        }
    };
    // Odd, so the last pass runs forward: each module ends level with what feeds it, which
    // is the wire a reader follows.
    for round in 0..9 {
        let forward = round % 2 == 0;
        let order: Vec<usize> = if forward { (1..layers_n).collect() } else { (0..layers_n.saturating_sub(1)).rev().collect() };
        for l in order {
            let want: Vec<f32> = layers[l]
                .iter()
                .map(|&k| {
                    let mut tops: Vec<f32> = if forward {
                        preds[k].iter().map(|&ix| { let e = group_edges[ix]; y[e.a] + e.ay - e.by }).collect()
                    } else {
                        succs[k].iter().map(|&ix| { let e = group_edges[ix]; y[e.b] + e.by - e.ay }).collect()
                    };
                    if tops.is_empty() {
                        return y[k];
                    }
                    tops.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    tops[(tops.len() - 1) / 2]
                })
                .collect();
            place(&layers[l], &want, &mut y);
        }
    }
    // From the top, on blocks.
    let top = y.iter().copied().fold(f32::INFINITY, f32::min);
    for v in &mut y {
        *v -= top;
    }
    if let Some(b) = sp.block {
        for l in &layers {
            let mut floor = 0.0f32;
            for &k in l {
                let v = ((y[k] / b).round() * b).max(floor);
                y[k] = v;
                floor = snap_up(v + frame(k).height() + sp.row);
            }
        }
    }
    let mut size = egui::Vec2::ZERO;
    let placed = (0..m)
        .map(|k| {
            let p = egui::pos2(col_x[layer[k]], y[k]);
            size = size.max(p.to_vec2() + frame(k).size());
            (members[k], p)
        })
        .collect();
    (placed, size)
}

/// Tops for a column of boxes `heights` tall, in this order, at least `gap` apart, as near
/// the tops `want` as they can be (least squares): pool-adjacent-violators on the tops less
/// the room the boxes above them need.
fn fit_in_order(want: &[f32], heights: &[f32], gap: f32) -> Vec<f32> {
    let mut offset = 0.0f32;
    let offsets: Vec<f32> = heights
        .iter()
        .map(|h| {
            let o = offset;
            offset += h + gap;
            o
        })
        .collect();
    // Blocks of (sum, count) whose means rise.
    let mut blocks: Vec<(f32, usize)> = Vec::new();
    for (w, o) in want.iter().zip(&offsets) {
        blocks.push((w - o, 1));
        while blocks.len() > 1 {
            let (s1, c1) = blocks[blocks.len() - 1];
            let (s0, c0) = blocks[blocks.len() - 2];
            if s0 / c0 as f32 <= s1 / c1 as f32 {
                break;
            }
            blocks.pop();
            *blocks.last_mut().expect("two blocks") = (s0 + s1, c0 + c1);
        }
    }
    let mut out = Vec::with_capacity(want.len());
    let mut i = 0;
    for (s, c) in blocks {
        let v = s / c as f32;
        for _ in 0..c {
            out.push(v + offsets[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: usize, x: f32, y: f32, w: f32, h: f32) -> ArrangeNode {
        ArrangeNode { id: NodeId(id), frame: egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)), entry: false, exit: false }
    }
    fn wire(a: usize, ay: f32, b: usize, by: f32) -> ArrangeWire {
        ArrangeWire { from: NodeId(a), from_y: ay, to: NodeId(b), to_y: by }
    }
    const SP: Spacing = Spacing { column: 112.0, row: 56.0, group: 112.0, block: None };

    fn overlaps(nodes: &[ArrangeNode], at: &HashMap<NodeId, egui::Pos2>) -> Vec<(usize, usize)> {
        let rect = |nd: &ArrangeNode| egui::Rect::from_min_size(at[&nd.id], nd.frame.size());
        let mut bad = Vec::new();
        for (i, a) in nodes.iter().enumerate() {
            for b in &nodes[i + 1..] {
                if rect(a).shrink(0.5).intersects(rect(b).shrink(0.5)) {
                    bad.push((a.id.0, b.id.0));
                }
            }
        }
        bad
    }

    // A chain reads left to right whatever muddle it started in, and a wire from one pin to
    // the next runs level.
    #[test]
    fn a_chain_reads_left_to_right_with_level_wires() {
        let mut nodes = vec![node(0, 500.0, 300.0, 120.0, 80.0), node(1, 0.0, 0.0, 140.0, 120.0), node(2, 250.0, 600.0, 100.0, 60.0)];
        nodes[0].entry = true;
        nodes[2].exit = true;
        let wires = [wire(0, 40.0, 1, 70.0), wire(1, 30.0, 2, 30.0)];
        let at = arrange(&nodes, &wires, &SP);
        assert!(at[&NodeId(0)].x < at[&NodeId(1)].x && at[&NodeId(1)].x < at[&NodeId(2)].x, "{at:?}");
        assert!(((at[&NodeId(0)].y + 40.0) - (at[&NodeId(1)].y + 70.0)).abs() < 1e-3, "level 0→1: {at:?}");
        assert!(((at[&NodeId(1)].y + 30.0) - (at[&NodeId(2)].y + 30.0)).abs() < 1e-3, "level 1→2: {at:?}");
        assert_eq!(at[&NodeId(1)].x - at[&NodeId(0)].x, 120.0 + 112.0, "one gap between columns");
        assert_eq!(egui::pos2(0.0, 0.0), at.values().fold(egui::pos2(f32::INFINITY, f32::INFINITY), |a, b| a.min(*b)), "kept where it was");
    }

    // Modules in one column never overlap, in the order that crosses fewest wires: a fan out
    // keeps its targets in the order of the pins they hang off.
    #[test]
    fn a_fan_out_keeps_its_pins_order_and_nothing_overlaps() {
        let mut nodes = vec![node(0, 0.0, 0.0, 120.0, 200.0)];
        nodes[0].entry = true;
        for k in 1..=3 {
            nodes.push(node(k, 300.0, (4 - k) as f32 * 50.0, 100.0, 90.0));
        }
        let wires = [wire(0, 30.0, 1, 20.0), wire(0, 90.0, 2, 20.0), wire(0, 150.0, 3, 20.0)];
        let at = arrange(&nodes, &wires, &SP);
        assert!(at[&NodeId(1)].y < at[&NodeId(2)].y && at[&NodeId(2)].y < at[&NodeId(3)].y, "{at:?}");
        assert!(overlaps(&nodes, &at).is_empty(), "{:?}", overlaps(&nodes, &at));
    }

    // A knob sits just before the module it turns, not back with the devices; the patch's
    // outputs line up in the last column; a feedback loop doesn't stop the layout.
    #[test]
    fn knobs_sit_by_what_they_turn_outputs_go_last_and_loops_lay_out() {
        let mut nodes = vec![
            node(0, 0.0, 0.0, 100.0, 100.0),   // device in
            node(1, 0.0, 0.0, 100.0, 100.0),   // curve
            node(2, 0.0, 0.0, 100.0, 100.0),   // gain
            node(3, 0.0, 0.0, 60.0, 60.0),     // knob on the gain
            node(4, 0.0, 0.0, 100.0, 100.0),   // device out
            node(5, 0.0, 0.0, 100.0, 100.0),   // readout off the curve
        ];
        nodes[0].entry = true;
        nodes[4].exit = true;
        let wires = [
            wire(0, 30.0, 1, 30.0),
            wire(1, 30.0, 2, 30.0),
            wire(3, 30.0, 2, 60.0),
            wire(2, 30.0, 4, 30.0),
            wire(1, 30.0, 5, 30.0),
            wire(2, 60.0, 1, 60.0), // a loop back
        ];
        let at = arrange(&nodes, &wires, &SP);
        let col = |k: usize| at[&NodeId(k)].x;
        assert!(col(3) > col(0) && col(3) < col(2), "the knob between: {at:?}");
        assert!(col(4) >= col(2) && col(4) > col(5) - 1.0, "the output last: {at:?}");
        assert!(overlaps(&nodes, &at).is_empty(), "{:?}", overlaps(&nodes, &at));
    }

    // Separate groups stack, loose modules go in a row underneath, and on the grid every
    // frame lands on a block centre.
    #[test]
    fn groups_stack_loose_ones_go_below_and_everything_lands_on_blocks() {
        let nodes = vec![
            node(0, 10.0, 400.0, 112.0, 84.0),
            node(1, 20.0, 30.0, 112.0, 56.0),
            node(2, 300.0, 500.0, 84.0, 84.0),
            node(3, 300.0, 20.0, 84.0, 56.0),
            node(4, 900.0, 900.0, 56.0, 56.0),
        ];
        let wires = [wire(0, 20.0, 2, 30.0), wire(1, 20.0, 3, 20.0)];
        let sp = Spacing { column: 112.0, row: 28.0, group: 84.0, block: Some(28.0) };
        let at = arrange(&nodes, &wires, &sp);
        assert!(at[&NodeId(1)].y < at[&NodeId(0)].y, "the higher group stays on top: {at:?}");
        let lowest_group = [0, 1, 2, 3].iter().map(|&k| at[&NodeId(k)].y + nodes[k].frame.height()).fold(0.0f32, f32::max);
        assert!(at[&NodeId(4)].y >= lowest_group, "the loose module below: {at:?}");
        for p in at.values() {
            for v in [p.x, p.y] {
                assert!(((v / 28.0 - 0.5).round() + 0.5) * 28.0 - v == 0.0, "{v} isn't on a block centre: {at:?}");
            }
        }
        assert!(overlaps(&nodes, &at).is_empty(), "{:?}", overlaps(&nodes, &at));
    }

    #[test]
    fn fitting_in_order_keeps_the_gaps_and_stays_near() {
        assert_eq!(fit_in_order(&[0.0, 100.0], &[50.0, 50.0], 10.0), vec![0.0, 100.0]);
        // Both want 0: they share the push.
        assert_eq!(fit_in_order(&[0.0, 0.0], &[50.0, 50.0], 10.0), vec![-30.0, 30.0]);
    }
}
