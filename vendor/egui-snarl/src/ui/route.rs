//! FLEXINPUT PATCH: wires routed through the gaps between nodes.
//!
//! With the layout grid on, node frames sit with their edges on block centers, so the
//! block lines always run through the gaps between nodes, never through a node. Wires
//! are routed along those lines: out of an output pin to the first line right of its
//! node, along the lines around every node in the way, and in from the line left of the
//! input pin's node. Nodes off the grid are walked around all the same, along their own
//! edges where no block line gets past them.
//!
//! Every wire gets a lane of its own where wires share a stretch of line, ordered so they
//! cross as little as they can; a wire that carries on along its pin's row steps into
//! its lane at the first gap. A wire takes a less crowded line when one costs about the
//! same. Nodes are walls that cost a great deal to go through rather than ones that
//! can't be, so every wire has a route, and only goes through a node when the patch
//! leaves it no way round.

use std::{cmp::Ordering, collections::BinaryHeap};

use egui::{Pos2, Rect, Vec2, ahash::HashMap, pos2, vec2};

/// One wire to route.
#[derive(Clone, Copy, Debug)]
pub struct RouteWire {
    /// The output pin's center.
    pub from: Pos2,
    /// The input pin's center.
    pub to: Pos2,
    /// Index of the output pin's node in the node list.
    pub from_node: usize,
    /// Index of the input pin's node in the node list.
    pub to_node: usize,
    /// Wires with the same net (the same output pin) don't mind following each other.
    pub net: u64,
    /// How thick the wire is drawn: lanes are spaced to keep wires apart edge to edge.
    pub width: f32,
}

/// How wires are routed.
#[derive(Clone, Copy, Debug)]
pub struct RouteParams {
    /// The grid's block size: the spacing of the lines wires run along.
    pub block: f32,
    /// How far from a node's frame, sideways, wires keep out (pins stick out there).
    pub side_clear: f32,
    /// How far from a node's frame, above and below, wires keep out.
    pub end_clear: f32,
    /// Clear space between the edges of wires in neighboring lanes.
    pub lane_gap: f32,
    /// Corner radius of a wire alone on its lines.
    pub radius: f32,
}

/// A stretch of block line where more wires run side by side than fit between the
/// nodes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crowding {
    /// Whether the line is vertical (wires run up and down it).
    pub vertical: bool,
    /// The line's x if vertical, else its y.
    pub coord: f32,
    /// Where the crowded stretch starts and ends along the line.
    pub lo: f32,
    pub hi: f32,
    /// How many lanes it has.
    pub lanes: usize,
    /// How many points wider than the gap the lanes are side by side.
    pub excess: f32,
}

/// Routed wires.
#[derive(Clone, Debug, Default)]
pub struct Routes {
    /// Each wire's route, spread into its lane. `None` only for a wire whose nodes
    /// weren't in the list.
    pub paths: Vec<Option<Vec<Pos2>>>,
    /// The radius to round each point of a path's corner to (the ends' are unused):
    /// inner lanes turn tighter than outer ones, so wires turning together stay apart.
    pub radii: Vec<Option<Vec<f32>>>,
    /// Each wire's route before lanes were spread: what [`route_some`] takes back to
    /// keep a wire where it was.
    pub raw: Vec<Option<Vec<Pos2>>>,
    pub crowding: Vec<Crowding>,
}

const EPS: f32 = 0.01;

/// Going through a node costs this many times going round.
const WALL_COST: f32 = 40.0;

/// Running along a node's edge rather than a block line costs this much more.
const OFF_GRID_COST: f32 = 0.15;

/// Routes `wires` around `nodes` (their frame rects).
#[must_use]
pub fn route_wires(nodes: &[Rect], wires: &[RouteWire], params: &RouteParams) -> Routes {
    route_some(nodes, wires, params, &vec![None; wires.len()])
}

/// Routes the wires `keep` has no route for, and keeps the routes it has: the routes
/// [`Routes::raw`] gave the same wires before. Lanes are spread over all of them.
///
/// Only the wires something happened to need routing again, which keeps a node being
/// dragged across a big patch from routing all of it every frame (and keeps wires that
/// nothing happened to from jumping between equally good routes).
#[must_use]
pub fn route_some(
    nodes: &[Rect],
    wires: &[RouteWire],
    params: &RouteParams,
    keep: &[Option<Vec<Pos2>>],
) -> Routes {
    let block = params.block.max(4.0);
    let walls: Vec<Rect> = nodes.iter().map(|r| wall(*r, params)).collect();

    let mut usage = Usage::default();
    let mut raw = vec![None; wires.len()];
    for (idx, kept) in keep.iter().enumerate().take(wires.len()) {
        if let Some(path) = kept {
            usage.add(path, wires[idx].net, idx, block);
            raw[idx] = Some(path.clone());
        }
    }

    // Short wires first: they have the fewest ways to go, and the long ones can go round.
    let mut order: Vec<usize> = (0..wires.len()).filter(|&i| raw[i].is_none()).collect();
    order.sort_by(|&a, &b| {
        let la = manhattan(wires[a].from, wires[a].to);
        let lb = manhattan(wires[b].from, wires[b].to);
        la.total_cmp(&lb).then(wires[a].net.cmp(&wires[b].net))
    });
    for idx in order {
        let wire = &wires[idx];
        if let Some(path) = route_one(wire, idx, &walls, block, &usage) {
            usage.add(&path, wire.net, idx, block);
            raw[idx] = Some(path);
        }
    }

    let widths: Vec<f32> = wires.iter().map(|w| w.width).collect();
    let (paths, radii, crowding) = spread_lanes(&raw, &widths, &walls, params, block);
    Routes { paths, radii, raw, crowding }
}

/// The area around a node's frame that wires keep out of.
#[must_use]
pub fn wall(frame: Rect, params: &RouteParams) -> Rect {
    Rect::from_min_max(
        pos2(frame.min.x - params.side_clear, frame.min.y - params.end_clear),
        pos2(frame.max.x + params.side_clear, frame.max.y + params.end_clear),
    )
}

/// Whether any segment of `path` passes into `area`.
#[must_use]
pub fn crosses(path: &[Pos2], area: Rect) -> bool {
    path.windows(2).any(|s| Rect::from_two_pos(s[0], s[1]).intersects(area))
}

fn manhattan(a: Pos2, b: Pos2) -> f32 {
    (a.x - b.x).abs() + (a.y - b.y).abs()
}

fn on_block(v: f32, block: f32) -> bool {
    let k = v / block;
    (k - k.round()).abs() * block < EPS
}

/// Which wires (and their nets) run along each block-long piece of each line.
#[derive(Default)]
struct Usage {
    wires: HashMap<(bool, i32, i32), Vec<(u64, usize)>>,
}

impl Usage {
    fn key(vertical: bool, coord: f32, cell: i32) -> (bool, i32, i32) {
        (vertical, (coord * 2.0).round() as i32, cell)
    }

    fn add(&mut self, path: &[Pos2], net: u64, wire: usize, block: f32) {
        for seg in path.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let vertical = (a.x - b.x).abs() < EPS;
            let (coord, lo, hi) = if vertical {
                (a.x, a.y.min(b.y), a.y.max(b.y))
            } else {
                (a.y, a.x.min(b.x), a.x.max(b.x))
            };
            let first = (lo / block).floor() as i32;
            let last = ((hi / block).ceil() as i32 - 1).max(first);
            for cell in first..=last {
                let users = self.wires.entry(Self::key(vertical, coord, cell)).or_default();
                if !users.iter().any(|&(_, w)| w == wire) {
                    users.push((net, wire));
                }
            }
        }
    }

    /// How much more than its length the piece of line between `lo` and `hi` costs: a
    /// quarter more for each other net on it, a bit less for each wire of the same net
    /// (enough that a big fan-out spreads over neighboring lines).
    fn crowd(&self, vertical: bool, coord: f32, lo: f32, hi: f32, net: u64, block: f32) -> f32 {
        let cell = ((lo + hi) * 0.5 / block).floor() as i32;
        self.wires.get(&Self::key(vertical, coord, cell)).map_or(0.0, |users| {
            users.iter().map(|&(n, _)| if n == net { 0.15 } else { 0.25 }).sum()
        })
    }
}

/// Whether a horizontal segment at `y` from `x_a` to `x_b` stays out of every wall but
/// `skip`'s.
fn h_clear(walls: &[Rect], y: f32, x_a: f32, x_b: f32, skip: &[usize]) -> bool {
    let (lo, hi) = (x_a.min(x_b), x_a.max(x_b));
    walls.iter().enumerate().all(|(i, w)| {
        skip.contains(&i)
            || !(y > w.min.y + EPS && y < w.max.y - EPS && lo < w.max.x - EPS && hi > w.min.x + EPS)
    })
}

/// Whether `p` is inside a wall other than `skip`'s.
fn buried(walls: &[Rect], p: Pos2, skip: usize) -> bool {
    walls.iter().enumerate().any(|(i, w)| {
        i != skip && p.x > w.min.x + EPS && p.x < w.max.x - EPS && p.y > w.min.y + EPS && p.y < w.max.y - EPS
    })
}

/// Directions a route moves in, as (dx, dy) on the candidate grid.
const DIRS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const EAST: usize = 0;
const WEST: usize = 1;

const fn opposite(dir: usize) -> usize {
    dir ^ 1
}

fn route_one(wire: &RouteWire, idx: usize, walls: &[Rect], block: f32, usage: &Usage) -> Option<Vec<Pos2>> {
    let src = *walls.get(wire.from_node)?;
    let dst = *walls.get(wire.to_node)?;
    let (p, q) = (wire.from, wire.to);

    // Straight across, when the pins face each other with nothing between them.
    if (p.y - q.y).abs() < 0.5 && p.x < q.x && h_clear(walls, p.y, p.x, q.x, &[wire.from_node, wire.to_node]) {
        return Some(vec![p, pos2(q.x, p.y)]);
    }

    // The lines the wire leaves and enters by: the first block line clear of its node,
    // or the node's own edge where that line is taken by another node.
    let mut x0 = (src.max.x / block).ceil() * block;
    if !h_clear(walls, p.y, p.x, x0, &[wire.from_node]) || buried(walls, pos2(x0, p.y), wire.from_node) {
        x0 = src.max.x;
    }
    let mut x1 = (dst.min.x / block).floor() * block;
    if !h_clear(walls, q.y, x1, q.x, &[wire.to_node]) || buried(walls, pos2(x1, q.y), wire.to_node) {
        x1 = dst.min.x;
    }

    // Round the nodes along the block lines, looking further out each time; then along
    // the nodes' edges too, for nodes off the grid that leave no block line between
    // them; through a node only when there's no way round at all. Each try is slower
    // than the one before, and on the grid the first nearly always does.
    let tries = [(3.0, false, false), (8.0, false, false), (8.0, true, false), (24.0, true, true)];
    for (margin, edges, soft) in tries {
        let found = search(wire, idx, walls, block, usage, (x0, x1), margin * block, edges, soft);
        let Some(mut corners) = found else {
            continue;
        };
        simplify(&mut corners);
        let mut path = Vec::with_capacity(corners.len() + 2);
        path.push(p);
        path.extend(corners);
        path.push(q);
        return Some(path);
    }
    None
}

#[derive(Clone, Copy, PartialEq)]
struct Open {
    f: f32,
    g: f32,
    state: u32,
}

impl Eq for Open {}

impl Ord for Open {
    fn cmp(&self, other: &Self) -> Ordering {
        // Lowest f first; among equals, the furthest along.
        other.f.total_cmp(&self.f).then(self.g.total_cmp(&other.g))
    }
}

impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Coordinates the search may stop at along one axis within `lo..hi`: the block lines,
/// the edges of the walls there, and the wire's own ends.
fn candidates(lo: f32, hi: f32, block: f32, extra: [f32; 2], edges: impl Iterator<Item = f32>) -> Vec<f32> {
    let first = (lo / block).floor() as i64;
    let last = (hi / block).ceil() as i64;
    let mut v: Vec<f32> = (first..=last).map(|k| k as f32 * block).collect();
    v.extend(extra);
    v.extend(edges.filter(|e| *e >= lo && *e <= hi));
    v.sort_by(f32::total_cmp);
    v.dedup_by(|a, b| (*a - *b).abs() < EPS);
    v
}

/// A* from where the wire leaves its node, `(x0, from.y)`, to where it enters the other,
/// `(x1, to.y)`. Returns the corners between them. `edges` lets the route run along the
/// walls' edges as well as the block lines; `soft` lets it through walls, at a cost.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn search(
    wire: &RouteWire,
    idx: usize,
    walls: &[Rect],
    block: f32,
    usage: &Usage,
    (x0, x1): (f32, f32),
    margin: f32,
    edges: bool,
    soft: bool,
) -> Option<Vec<Pos2>> {
    let _ = idx;
    let (py, qy) = (wire.from.y, wire.to.y);
    let (x_lo, x_hi) = (x0.min(x1) - margin, x0.max(x1) + margin);
    let (y_lo, y_hi) = (py.min(qy) - margin, py.max(qy) + margin);
    let near: Vec<&Rect> = walls
        .iter()
        .filter(|w| w.max.x >= x_lo && w.min.x <= x_hi && w.max.y >= y_lo && w.min.y <= y_hi)
        .collect();
    let edge_walls = if edges { near.as_slice() } else { &[] };
    let xs = candidates(x_lo, x_hi, block, [x0, x1], edge_walls.iter().flat_map(|w| [w.min.x, w.max.x]));
    let ys = candidates(y_lo, y_hi, block, [py, qy], edge_walls.iter().flat_map(|w| [w.min.y, w.max.y]));
    let (nx, ny) = (xs.len(), ys.len());
    if nx < 2 || ny < 2 || nx * ny > 400_000 {
        return None;
    }
    let find = |v: &[f32], x: f32| v.iter().position(|&c| (c - x).abs() < EPS);
    let start = (find(&xs, x0)?, find(&ys, py)?);
    let goal = (find(&xs, x1)?, find(&ys, qy)?);
    let at = |(i, j): (usize, usize)| j * nx + i;

    // Steps through a wall: the gaps between neighboring points that pass into one.
    let mut h_wall = vec![false; nx * ny]; // from (i, j) to (i + 1, j)
    let mut v_wall = vec![false; nx * ny]; // from (i, j) to (i, j + 1)
    let inside = |v: &[f32], lo: f32, hi: f32| {
        let a = v.partition_point(|&c| c <= lo + EPS);
        let b = v.partition_point(|&c| c < hi - EPS);
        a..b.max(a)
    };
    let spans = |v: &[f32], lo: f32, hi: f32| {
        // Gaps k (between v[k] and v[k + 1]) with v[k] < hi and v[k + 1] > lo.
        let a = v.partition_point(|&c| c <= lo + EPS).saturating_sub(1);
        let b = v.partition_point(|&c| c < hi - EPS).min(v.len() - 1);
        a..b.max(a)
    };
    for w in &near {
        let (ix, iy) = (inside(&xs, w.min.x, w.max.x), inside(&ys, w.min.y, w.max.y));
        let (sx, sy) = (spans(&xs, w.min.x, w.max.x), spans(&ys, w.min.y, w.max.y));
        for j in iy {
            for i in sx.clone() {
                h_wall[j * nx + i] = true;
            }
        }
        for j in sy {
            for i in ix.clone() {
                v_wall[j * nx + i] = true;
            }
        }
    }

    // What each step costs per point of length.
    let x_grid: Vec<bool> = xs.iter().map(|&x| on_block(x, block)).collect();
    let y_grid: Vec<bool> = ys.iter().map(|&y| on_block(y, block)).collect();
    let mut h_cost = vec![1.0_f32; nx * ny];
    let mut v_cost = vec![1.0_f32; nx * ny];
    for j in 0..ny {
        for i in 0..nx {
            let e = at((i, j));
            if i + 1 < nx {
                let mut c = 1.0 + usage.crowd(false, ys[j], xs[i], xs[i + 1], wire.net, block);
                // The wire's own pin rows are as good as block lines.
                if !y_grid[j] && (ys[j] - py).abs() > EPS && (ys[j] - qy).abs() > EPS {
                    c += OFF_GRID_COST;
                }
                h_cost[e] = if h_wall[e] { c * WALL_COST } else { c };
            }
            if j + 1 < ny {
                let mut c = 1.0 + usage.crowd(true, xs[i], ys[j], ys[j + 1], wire.net, block);
                if !x_grid[i] && (xs[i] - x0).abs() > EPS && (xs[i] - x1).abs() > EPS {
                    c += OFF_GRID_COST;
                }
                v_cost[e] = if v_wall[e] { c * WALL_COST } else { c };
            }
        }
    }

    let bend = block;
    let state = |v: usize, dir: usize| (v * 4 + dir) as u32;
    let mut best = vec![f32::INFINITY; nx * ny * 4];
    let mut came = vec![u32::MAX; nx * ny * 4];
    let mut open = BinaryHeap::new();
    let heuristic = |i: usize, j: usize| (xs[i] - x1).abs() + (ys[j] - qy).abs();

    let s0 = state(at(start), EAST);
    best[s0 as usize] = 0.0;
    open.push(Open { f: heuristic(start.0, start.1), g: 0.0, state: s0 });

    let mut found: Option<(f32, u32)> = None;
    while let Some(Open { f, g, state: s }) = open.pop() {
        if found.is_some_and(|(cost, _)| f >= cost) {
            break;
        }
        if g > best[s as usize] {
            continue;
        }
        let (v, dir) = (s as usize / 4, s as usize % 4);
        let (i, j) = (v % nx, v / nx);
        if (i, j) == goal && dir != WEST {
            // The wire goes on east into the pin from here (never back over itself).
            let total = g + if dir == EAST { 0.0 } else { bend };
            if found.is_none_or(|(cost, _)| total < cost) {
                found = Some((total, s));
            }
            continue;
        }
        for (d, &(dx, dy)) in DIRS.iter().enumerate() {
            if d == opposite(dir) {
                continue;
            }
            let (ni, nj) = (i as i64 + i64::from(dx), j as i64 + i64::from(dy));
            if ni < 0 || nj < 0 || ni >= nx as i64 || nj >= ny as i64 {
                continue;
            }
            let (ni, nj) = (ni as usize, nj as usize);
            if !soft {
                let walled = match d {
                    0 => h_wall[at((i, j))],
                    1 => h_wall[at((ni, nj))],
                    2 => v_wall[at((i, j))],
                    _ => v_wall[at((ni, nj))],
                };
                if walled {
                    continue;
                }
            }
            let step = match d {
                0 => (xs[ni] - xs[i]) * h_cost[at((i, j))],
                1 => (xs[i] - xs[ni]) * h_cost[at((ni, nj))],
                2 => (ys[nj] - ys[j]) * v_cost[at((i, j))],
                _ => (ys[j] - ys[nj]) * v_cost[at((ni, nj))],
            };
            let cost = g + step + if d == dir { 0.0 } else { bend };
            let ns = state(at((ni, nj)), d);
            if cost < best[ns as usize] {
                best[ns as usize] = cost;
                came[ns as usize] = s;
                open.push(Open { f: cost + heuristic(ni, nj), g: cost, state: ns });
            }
        }
    }

    let (_, mut s) = found?;
    let mut corners = Vec::new();
    loop {
        let v = s as usize / 4;
        corners.push(pos2(xs[v % nx], ys[v / nx]));
        if s == s0 {
            break;
        }
        s = came[s as usize];
        if s == u32::MAX {
            return None;
        }
    }
    corners.reverse();
    Some(corners)
}

/// Drops points that don't turn the path, but never its first and last.
fn simplify(path: &mut Vec<Pos2>) {
    path.dedup_by(|a, b| (a.x - b.x).abs() < EPS && (a.y - b.y).abs() < EPS);
    let mut i = 1;
    while i + 1 < path.len() {
        let (a, b, c) = (path[i - 1], path[i], path[i + 1]);
        let straight = ((a.x - b.x).abs() < EPS && (b.x - c.x).abs() < EPS)
            || ((a.y - b.y).abs() < EPS && (b.y - c.y).abs() < EPS);
        if straight {
            path.remove(i);
        } else {
            i += 1;
        }
    }
}

/// One wire's run along one line: where it is, and where it leaves the line and to
/// which side.
struct Track {
    wire: usize,
    width: f32,
    lo: f32,
    hi: f32,
    /// (position along the line, side: -1 toward lower coordinates, 1 toward higher).
    branches: Vec<(f32, f32)>,
    segs: Vec<usize>,
    lane: usize,
    offset: f32,
}

impl Track {
    fn overlaps(&self, other: &Track) -> bool {
        self.lo < other.hi - EPS && other.lo < self.hi - EPS
    }

    /// How strongly this track wants to be on the higher side of `other`: each branch of
    /// one leaving the line inside the other's stretch crosses the other unless it's on
    /// the side it leaves toward.
    fn vote(&self, other: &Track) -> f32 {
        let within = |t: &Track, pos: f32| pos > t.lo + EPS && pos < t.hi - EPS;
        let mine: f32 = self.branches.iter().filter(|b| within(other, b.0)).map(|b| b.1).sum();
        let theirs: f32 = other.branches.iter().filter(|b| within(self, b.0)).map(|b| b.1).sum();
        mine - theirs
    }
}

/// The room around a line (x = `coord` if vertical, else y) along `lo..hi`: how far the
/// nearest wall is on its lower side and on its higher side, at most a block each way.
/// Walls the line runs through (a route that had no way round) don't count.
fn corridor(walls: &[Rect], vertical: bool, coord: f32, lo: f32, hi: f32, block: f32) -> (f32, f32) {
    let (mut below, mut above) = (block, block);
    for w in walls {
        let (along_lo, along_hi, across_lo, across_hi) = if vertical {
            (w.min.y, w.max.y, w.min.x, w.max.x)
        } else {
            (w.min.x, w.max.x, w.min.y, w.max.y)
        };
        if along_hi <= lo + EPS || along_lo >= hi - EPS {
            continue;
        }
        if across_hi <= coord + EPS {
            below = below.min(coord - across_hi);
        } else if across_lo >= coord - EPS {
            above = above.min(across_lo - coord);
        }
    }
    (below.max(0.0), above.max(0.0))
}

fn is_vertical(a: Pos2, b: Pos2) -> bool {
    (a.x - b.x).abs() < EPS && (a.y - b.y).abs() >= EPS
}

/// Spreads wires sharing a line into lanes. The stubs into and out of pins stay where
/// they are; every other segment moves sideways by its lane's offset. Returns the
/// spread paths, each point's corner radius, and the crowded stretches of block line.
#[allow(clippy::type_complexity, clippy::too_many_lines)]
fn spread_lanes(
    raw: &[Option<Vec<Pos2>>],
    widths: &[f32],
    walls: &[Rect],
    params: &RouteParams,
    block: f32,
) -> (Vec<Option<Vec<Pos2>>>, Vec<Option<Vec<f32>>>, Vec<Crowding>) {
    let mut lines: HashMap<(bool, i32), Vec<Track>> = HashMap::default();
    for (w, path) in raw.iter().enumerate() {
        let Some(path) = path else { continue };
        let n = path.len();
        // Segments 1..n-2: all but the stubs at the pins.
        for k in 1..n.saturating_sub(2) {
            let (a, b) = (path[k], path[k + 1]);
            let vertical = is_vertical(a, b);
            let (coord, lo, hi) = if vertical {
                (a.x, a.y.min(b.y), a.y.max(b.y))
            } else {
                (a.y, a.x.min(b.x), a.x.max(b.x))
            };
            let side = |p: Pos2| {
                let d = if vertical { p.x - coord } else { p.y - coord };
                if d.abs() < EPS { 0.0 } else { d.signum() }
            };
            let along = |p: Pos2| if vertical { p.y } else { p.x };
            let branches = vec![(along(a), side(path[k - 1])), (along(b), side(path[k + 2]))];
            lines.entry((vertical, (coord * 2.0).round() as i32)).or_default().push(Track {
                wire: w,
                width: widths.get(w).copied().unwrap_or(1.0),
                lo,
                hi,
                branches,
                segs: vec![k],
                lane: 0,
                offset: 0.0,
            });
        }
    }

    let mut offsets: HashMap<(usize, usize), f32> = HashMap::default();
    let mut crowding = Vec::new();
    for ((vertical, coord2), mut tracks) in lines {
        // A wire that runs along a line twice in a row (rare) keeps one lane for it.
        tracks.sort_by(|a, b| a.wire.cmp(&b.wire).then(a.lo.total_cmp(&b.lo)));
        let mut merged: Vec<Track> = Vec::with_capacity(tracks.len());
        for t in tracks {
            match merged.last_mut() {
                Some(m) if m.wire == t.wire && t.lo <= m.hi + EPS => {
                    m.hi = m.hi.max(t.hi);
                    m.branches.extend(t.branches);
                    m.segs.extend(t.segs);
                }
                _ => merged.push(t),
            }
        }
        let mut tracks = merged;
        if tracks.len() < 2 {
            continue;
        }

        let n = tracks.len();
        let scores: Vec<f32> = (0..n)
            .map(|a| {
                (0..n)
                    .filter(|&b| b != a && tracks[a].overlaps(&tracks[b]))
                    .map(|b| tracks[a].vote(&tracks[b]))
                    .sum()
            })
            .collect();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| {
            scores[a]
                .total_cmp(&scores[b])
                .then(tracks[a].lo.total_cmp(&tracks[b].lo))
                .then(tracks[a].wire.cmp(&tracks[b].wire))
        });
        for (rank, &t) in order.iter().enumerate() {
            let lane = order[..rank]
                .iter()
                .filter(|&&u| tracks[u].overlaps(&tracks[t]))
                .map(|&u| tracks[u].lane + 1)
                .max()
                .unwrap_or(0);
            tracks[t].lane = lane;
        }

        // Tracks that overlap, directly or through others, center their lanes together.
        let mut group: Vec<usize> = (0..n).collect();
        fn root(group: &mut [usize], mut i: usize) -> usize {
            while group[i] != i {
                group[i] = group[group[i]];
                i = group[i];
            }
            i
        }
        for a in 0..n {
            for b in a + 1..n {
                if tracks[a].overlaps(&tracks[b]) {
                    let (ra, rb) = (root(&mut group, a), root(&mut group, b));
                    group[ra] = rb;
                }
            }
        }
        // Each cluster's lanes, packed edge to edge with clear space between (a lane as
        // wide as its widest wire), and centered on the line.
        let coord = coord2 as f32 * 0.5;
        let mut lanes_of: HashMap<usize, (Vec<f32>, f32, f32)> = HashMap::default();
        for t in 0..n {
            let r = root(&mut group, t);
            let e = lanes_of.entry(r).or_insert((Vec::new(), f32::INFINITY, f32::NEG_INFINITY));
            let lane = tracks[t].lane;
            if e.0.len() <= lane {
                e.0.resize(lane + 1, 0.0);
            }
            e.0[lane] = e.0[lane].max(tracks[t].width);
            e.1 = e.1.min(tracks[t].lo);
            e.2 = e.2.max(tracks[t].hi);
        }
        let mut centers: HashMap<usize, Vec<f32>> = HashMap::default();
        for (&r, (lane_w, lo, hi)) in &lanes_of {
            let mut at = Vec::with_capacity(lane_w.len());
            let mut x = 0.0;
            for (l, &w) in lane_w.iter().enumerate() {
                if l > 0 {
                    x += lane_w[l - 1] * 0.5 + params.lane_gap + w * 0.5;
                }
                at.push(x);
            }
            // Centered on the line, then moved off whichever side is nearer a wall, as
            // far as the room on the other side allows.
            let (below, above) = corridor(walls, vertical, coord, *lo, *hi, block);
            let (ext_lo, ext_hi) = (x * 0.5 + lane_w[0] * 0.5, x * 0.5 + lane_w[lane_w.len() - 1] * 0.5);
            let span = ext_lo + ext_hi;
            let shift = if span > below + above {
                (above - below) * 0.5
            } else if ext_lo > below {
                ext_lo - below
            } else if ext_hi > above {
                -(ext_hi - above)
            } else {
                0.0
            };
            for a in &mut at {
                *a += shift - x * 0.5;
            }
            if lane_w.len() > 1 && span > below + above + EPS && on_block(coord, block) {
                crowding.push(Crowding {
                    vertical,
                    coord,
                    lo: *lo,
                    hi: *hi,
                    lanes: lane_w.len(),
                    excess: span - (below + above),
                });
            }
            centers.insert(r, at);
        }
        for t in 0..n {
            let r = root(&mut group, t);
            tracks[t].offset = centers[&r][tracks[t].lane];
        }
        for t in &tracks {
            for &k in &t.segs {
                offsets.insert((t.wire, k), t.offset);
            }
        }
    }

    let mut paths = Vec::with_capacity(raw.len());
    let mut radii = Vec::with_capacity(raw.len());
    for (w, path) in raw.iter().enumerate() {
        let Some(old) = path else {
            paths.push(None);
            radii.push(None);
            continue;
        };
        let (p, r) = lay_out(old, |k| offsets.get(&(w, k)).copied().unwrap_or(0.0), params.radius);
        paths.push(Some(p));
        radii.push(Some(r));
    }
    (paths, radii, crowding)
}

/// A raw route moved into its lanes (`off(k)` is segment k's offset), with each point's
/// corner radius. Where two segments in a row run along the same line in different
/// lanes, the wire steps across between them.
fn lay_out(old: &[Pos2], off: impl Fn(usize) -> f32, radius: f32) -> (Vec<Pos2>, Vec<f32>) {
    let n = old.len();
    if n < 3 {
        return (old.to_vec(), vec![0.0; n]);
    }
    let dir = |k: usize| (old[k + 1] - old[k]).normalized();
    // Which way a segment's lanes go: down for a horizontal one, right for a vertical.
    let normal = |k: usize| if is_vertical(old[k], old[k + 1]) { vec2(1.0, 0.0) } else { vec2(0.0, 1.0) };
    let mut pts: Vec<(Pos2, f32)> = Vec::with_capacity(n + 4);
    pts.push((old[0], 0.0));
    for k in 1..n - 1 {
        let (oi, oo) = (off(k - 1), off(k));
        let (ni, no) = (normal(k - 1), normal(k));
        let (di, d_out) = (dir(k - 1), dir(k));
        if ni == no {
            // Straight on, maybe into another lane: step across (two small corners).
            if (oi - oo).abs() < EPS {
                continue;
            }
            pts.push((old[k] + ni * oi, radius));
            pts.push((old[k] + no * oo, radius));
        } else {
            // A turn: lanes on its inside turn tighter, so wires turning together keep
            // their distance all the way round.
            let corner = old[k] + ni * oi + no * oo;
            let inside = (oi * ni.dot(d_out) + oo * no.dot(-di)) * 0.5;
            pts.push((corner, (radius - inside).max(3.0)));
        }
    }
    pts.push((old[n - 1], 0.0));

    // Merge what offsetting made coincide or straight.
    pts.dedup_by(|a, b| (a.0 - b.0).length_sq() < EPS * EPS);
    let mut i = 1;
    while i + 1 < pts.len() {
        let (a, b, c) = (pts[i - 1].0, pts[i].0, pts[i + 1].0);
        let straight = ((a.x - b.x).abs() < EPS && (b.x - c.x).abs() < EPS)
            || ((a.y - b.y).abs() < EPS && (b.y - c.y).abs() < EPS);
        if straight {
            pts.remove(i);
        } else {
            i += 1;
        }
    }
    pts.into_iter().unzip()
}

/// How close the inside of a rounded corner may come to a node.
const CORNER_CLEARANCE: f32 = 10.0;

/// How far past a route's own extent a taut wire looks for nodes that might be in its
/// way.
const AROUND_REACH: f32 = 200.0;

/// The longest a taut wire's stub out of (or into) a pin is, before the shortest way round
/// takes over (never inside the pin's own node's wall).
const STUB_REACH: f32 = 24.0;

/// A routed wire pulled taut into a flowing curve, for wires that should go around
/// nodes without keeping to the grid's lines. `route` is the wire's route spread into
/// its lane (pins at the ends, the stubs out of and into them next), `ends` the
/// indices of the nodes it runs between, `frames` the nodes' frames, `walls` the areas
/// wires keep out of.
///
/// Between its stubs the wire takes the shortest way round the nodes in its way, any
/// angle, holding `clearance` off each node, plus `berth[i]`: what the wires routed
/// before it took going round node `i`. Every node it goes round widens by `pitch` (its
/// own width and the gap to the next) for the wires after it, so wires going round a
/// node together stack beside each other rather than on top of each other. It leaves
/// its pin level, straight on for as far as the way ahead allows, and reaches the other
/// the same way; each corner is rounded as much as the nodes near it allow (a rounded
/// corner cuts inside, toward the node it goes round). With nothing in the way it's a
/// plain S between the pins. A curve that would still clip a node gives way to the
/// route itself, rounded.
#[must_use]
pub fn taut(
    route: &[Pos2],
    ends: (usize, usize),
    frames: &[Rect],
    walls: &[Rect],
    clearance: f32,
    pitch: f32,
    berth: &mut [f32],
) -> Vec<Pos2> {
    let n = route.len();
    if n < 3 {
        return route.to_vec();
    }
    let (p, q) = (route[0], route[n - 1]);
    let (s, e) = (route[1], route[n - 2]);
    let region = Rect::from_points(route).expand(clearance.max(0.0) + AROUND_REACH);
    let berth_of = |i: usize| berth.get(i).copied().unwrap_or(0.0);

    // FLEXINPUT PATCH (2026-11-19): a route's stubs can be the whole of its first and last
    // runs - a route that keeps to its pin's row until it turns - and only what lies between
    // them is pulled taut, so such a wire stayed square: level, a hard corner, straight up, a
    // hard corner. Pulled back to a lead's length out of the pin (outside the node's own
    // wall), they leave the rest to the shortest way round.
    let s = if (s.y - p.y).abs() < EPS && s.x > p.x {
        let floor = walls.get(ends.0).map_or(p.x, |w| w.max.x + 2.0 * EPS);
        pos2(s.x.min((p.x + STUB_REACH).max(floor)), s.y)
    } else {
        s
    };
    let e = if (e.y - q.y).abs() < EPS && e.x < q.x {
        let ceil = walls.get(ends.1).map_or(q.x, |w| w.min.x - 2.0 * EPS);
        pos2(e.x.max((q.x - STUB_REACH).min(ceil)), e.y)
    } else {
        e
    };

    // What the wire goes round: the walls near it, held off by the clearance and by
    // the wires already round them. Its own two nodes are held off only by the wires
    // already round them - wires that loop back round the node they leave, or the one
    // they reach, go round it side by side too, where they used to lie on top of each
    // other - and the wire's stubs reach out past that, so it never starts inside its own
    // node's berth. A wall grown over one of its ends isn't gone round at all.
    // FLEXINPUT PATCH (2026-11-19): the ends' berth.
    let grown = |i: usize| walls.get(i).filter(|_| berth_of(i) > 0.0).map(|w| w.expand(berth_of(i)));
    let s = match (ends.0 != usize::MAX).then(|| grown(ends.0)).flatten() {
        Some(w) if s.y > w.min.y && s.y < w.max.y => pos2(s.x.max(w.max.x + 2.0 * EPS), s.y),
        _ => s,
    };
    let e = match (ends.1 != usize::MAX).then(|| grown(ends.1)).flatten() {
        Some(w) if e.y > w.min.y && e.y < w.max.y => pos2(e.x.min(w.min.x - 2.0 * EPS), e.y),
        _ => e,
    };
    let inside = |r: &Rect, at: Pos2| {
        at.x > r.min.x + EPS && at.x < r.max.x - EPS && at.y > r.min.y + EPS && at.y < r.max.y - EPS
    };
    let obstacles: Vec<(usize, Rect)> = walls
        .iter()
        .enumerate()
        .filter(|(_, w)| w.intersects(region))
        .map(|(i, w)| {
            let grow = if i == ends.0 || i == ends.1 { berth_of(i) } else { clearance.max(0.0) + berth_of(i) };
            (i, w.expand(grow))
        })
        .filter(|(_, o)| !inside(o, s) && !inside(o, e))
        .collect();
    let clear = |a: Pos2, b: Pos2| obstacles.iter().all(|(_, o)| !segment_enters(a, b, *o));

    let (path, gone_round) = shortest(s, e, &obstacles).unwrap_or_else(|| (vec![s, e], Vec::new()));
    for i in gone_round {
        if let Some(b) = berth.get_mut(i) {
            *b += pitch;
        }
    }

    let near_frames: Vec<Rect> = frames.iter().copied().filter(|f| f.intersects(region)).collect();
    let clips = |curve: &[Pos2]| {
        curve.windows(2).any(|w| near_frames.iter().any(|f| segment_enters(w[0], w[1], f.shrink(1.0))))
    };

    // Nothing in the way: the plain wire, one S from pin to pin.
    if path.len() == 2 && q.x > p.x {
        let curve = s_curve(p, q);
        if !clips(&curve) {
            return curve;
        }
    }

    // How far straight out of a pin the wire runs before it turns: some of the way to
    // where it's headed, as far as stays clear, never less than the stub. `dir` is 1
    // leaving the output pin, -1 coming into the input pin.
    let lead = |pin: Pos2, stub: Pos2, toward: Pos2, dir: f32| -> Pos2 {
        let ahead = (toward.x - pin.x) * dir;
        let stub_len = (stub.x - pin.x).abs();
        if ahead <= stub_len {
            return stub;
        }
        // Not `clamp`: a stub can be longer than the cap (a wire carrying on along its
        // pin's row), and then the lead is just the stub.
        let want = (ahead * 0.4).min(140.0).max(stub_len);
        if want <= stub_len {
            return stub;
        }
        for len in [want, (want + stub_len) * 0.5] {
            let at = pos2(pin.x + len * dir, pin.y);
            if clear(stub, at) && clear(at, toward) {
                return at;
            }
        }
        stub
    };
    let between = &path[1..path.len() - 1];
    let first = between.first().copied().unwrap_or(e);
    let last = between.last().copied().unwrap_or(s);
    let out_lead = lead(p, s, first, 1.0);
    let in_lead = lead(q, e, if between.is_empty() { out_lead } else { last }, -1.0);

    // Each corner as round as the nodes near it allow: a corner cut by a curve `r`
    // along each side comes `r * |v - u| / 4` in from the corner.
    let radii = |poly: &[Pos2], cap: f32| -> Vec<f32> {
        (0..poly.len())
            .map(|k| {
                if k == 0 || k + 1 == poly.len() {
                    return 0.0;
                }
                let b = poly[k];
                let (u, v) = ((b - poly[k - 1]).normalized(), (poly[k + 1] - b).normalized());
                let inset = (v - u).length() * 0.25;
                let room = near_frames
                    .iter()
                    .map(|f| {
                        let dx = (f.min.x - b.x).max(b.x - f.max.x).max(0.0);
                        let dy = (f.min.y - b.y).max(b.y - f.max.y).max(0.0);
                        (dx * dx + dy * dy).sqrt()
                    })
                    .fold(f32::INFINITY, f32::min);
                let allowed = (room - CORNER_CLEARANCE).max(2.0);
                if inset > 0.01 { cap.min(allowed / inset) } else { cap }
            })
            .collect()
    };

    let mut poly = Vec::with_capacity(between.len() + 4);
    poly.push(p);
    poly.push(out_lead);
    poly.extend(between.iter().copied());
    poly.push(in_lead);
    poly.push(q);
    simplify(&mut poly);
    let curve = rounded_any(&poly, &radii(&poly, 56.0));
    if !clips(&curve) {
        return curve;
    }
    let mut poly = route.to_vec();
    simplify(&mut poly);
    rounded_any(&poly, &radii(&poly, 24.0))
}

/// The shortest way from `a` to `b` that doesn't go into any of `obstacles`, at any
/// angle: through the obstacles' corners (the shortest way round a box always is), by
/// A* over which corners see which. Returns the path and the obstacles it goes round.
fn shortest(a: Pos2, b: Pos2, obstacles: &[(usize, Rect)]) -> Option<(Vec<Pos2>, Vec<usize>)> {
    let blocked = |x: Pos2, y: Pos2| obstacles.iter().any(|(_, o)| segment_enters(x, y, *o));
    if !blocked(a, b) {
        return Some((vec![a, b], Vec::new()));
    }
    let buried = |c: Pos2| {
        obstacles
            .iter()
            .any(|(_, r)| c.x > r.min.x + EPS && c.x < r.max.x - EPS && c.y > r.min.y + EPS && c.y < r.max.y - EPS)
    };
    let mut points: Vec<(Pos2, Option<usize>)> = vec![(a, None), (b, None)];
    for &(i, o) in obstacles {
        for c in [o.left_top(), o.right_top(), o.left_bottom(), o.right_bottom()] {
            if !buried(c) {
                points.push((c, Some(i)));
            }
        }
    }
    let m = points.len();
    let mut dist = vec![f32::INFINITY; m];
    let mut came = vec![usize::MAX; m];
    let mut done = vec![false; m];
    let mut open = BinaryHeap::new();
    dist[0] = 0.0;
    open.push(Open { f: (b - a).length(), g: 0.0, state: 0 });
    while let Some(Open { g, state, .. }) = open.pop() {
        let i = state as usize;
        if done[i] || g > dist[i] {
            continue;
        }
        done[i] = true;
        if i == 1 {
            break;
        }
        let from = points[i].0;
        for j in 0..m {
            if done[j] {
                continue;
            }
            let to = points[j].0;
            let d = g + (to - from).length();
            if d >= dist[j] || blocked(from, to) {
                continue;
            }
            dist[j] = d;
            came[j] = i;
            open.push(Open { f: d + (b - to).length(), g: d, state: j as u32 });
        }
    }
    if !done[1] {
        return None;
    }
    let mut path = Vec::new();
    let mut gone_round = Vec::new();
    let mut k = 1;
    loop {
        path.push(points[k].0);
        if let Some(i) = points[k].1 {
            if !gone_round.contains(&i) {
                gone_round.push(i);
            }
        }
        if k == 0 {
            break;
        }
        k = came[k];
        if k == usize::MAX {
            return None;
        }
    }
    path.reverse();
    Some((path, gone_round))
}

/// The plain wire from `p` to `q`: a curve leaving and reaching them level, reaching out
/// further the further apart they are.
fn s_curve(p: Pos2, q: Pos2) -> Vec<Pos2> {
    let reach = ((q.x - p.x) * 0.5).clamp(24.0, 160.0);
    let (c1, c2) = (p + vec2(reach, 0.0), q - vec2(reach, 0.0));
    let steps = (((q - p).length() / 10.0).ceil() as usize).clamp(8, 64);
    (0..=steps)
        .map(|s| {
            let t = s as f32 / steps as f32;
            let mt = 1.0 - t;
            (p.to_vec2() * (mt * mt * mt)
                + c1.to_vec2() * (3.0 * mt * mt * t)
                + c2.to_vec2() * (3.0 * mt * t * t)
                + q.to_vec2() * (t * t * t))
                .to_pos2()
        })
        .collect()
}

/// `path` with every corner, at whatever angle, cut by a curve its radius in `radii`
/// along each side (less where a side is too short for it).
fn rounded_any(path: &[Pos2], radii: &[f32]) -> Vec<Pos2> {
    let n = path.len();
    if n < 3 {
        return path.to_vec();
    }
    const STEPS: usize = 10;
    let mut out = vec![path[0]];
    for k in 1..n - 1 {
        let (a, b, c) = (path[k - 1], path[k], path[k + 1]);
        let (in_len, out_len) = ((b - a).length(), (c - b).length());
        if in_len < EPS || out_len < EPS {
            out.push(b);
            continue;
        }
        let (u, v) = ((b - a) / in_len, (c - b) / out_len);
        let r = radii.get(k).copied().unwrap_or(0.0).min(in_len * 0.5).min(out_len * 0.5);
        let (p0, p2) = (b - u * r, b + v * r);
        for s in 0..=STEPS {
            let t = s as f32 / STEPS as f32;
            let mt = 1.0 - t;
            out.push((p0.to_vec2() * (mt * mt) + b.to_vec2() * (2.0 * mt * t) + p2.to_vec2() * (t * t)).to_pos2());
        }
    }
    out.push(path[n - 1]);
    out
}

/// Whether the segment from `a` to `b` passes through the inside of `r` (running along
/// its edge doesn't count).
fn segment_enters(a: Pos2, b: Pos2, r: Rect) -> bool {
    let r = r.shrink(EPS);
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return false;
    }
    // Liang–Barsky: clip the segment's parameter range to the rect, one side at a time.
    let d = b - a;
    let (mut t0, mut t1) = (0.0_f32, 1.0_f32);
    for (p, q) in [(-d.x, a.x - r.min.x), (d.x, r.max.x - a.x), (-d.y, a.y - r.min.y), (d.y, r.max.y - a.y)] {
        if p.abs() < 1e-9 {
            if q < 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                if t > t1 {
                    return false;
                }
                t0 = t0.max(t);
            } else {
                if t < t0 {
                    return false;
                }
                t1 = t1.min(t);
            }
        }
    }
    t0 < t1
}

/// The squared distance from `p` to the polyline `path`.
#[must_use]
pub fn distance_sq(path: &[Pos2], p: Pos2) -> f32 {
    path.windows(2)
        .map(|s| {
            let (a, b) = (s[0], s[1]);
            let ab = b - a;
            let len_sq = ab.length_sq();
            let t = if len_sq > 0.0 { ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0) } else { 0.0 };
            (a + ab * t - p).length_sq()
        })
        .fold(f32::INFINITY, f32::min)
}

/// Jogs shorter than this between two runs going the same way are drawn as an eased S
/// rather than two corners: pins nearly in line, or a wire stepping into its lane.
pub const S_BEND_MAX: f32 = 24.0;

/// `path` with each corner rounded to its radius in `radii` (less where a segment is
/// too short for it), as a circular arc; and each short jog between two runs going the
/// same way (see [`S_BEND_MAX`]) eased into an S.
#[must_use]
pub fn rounded(path: &[Pos2], radii: &[f32]) -> Vec<Pos2> {
    let n = path.len();
    if n < 3 {
        return path.to_vec();
    }
    const STEPS: usize = 8;
    let mut out = Vec::with_capacity(n * (STEPS + 1));
    out.push(path[0]);
    let mut k = 1;
    while k < n - 1 {
        let (a, b, c) = (path[k - 1], path[k], path[k + 1]);
        let (in_len, out_len) = ((b - a).length(), (c - b).length());
        if in_len < EPS || out_len < EPS {
            out.push(b);
            k += 1;
            continue;
        }
        let (u, v) = ((b - a) / in_len, (c - b) / out_len);

        // A short jog (b to c) between runs going the same way: an S from partway along
        // the run in to partway along the run out, level at both ends.
        if k + 2 < n {
            let d = path[k + 2];
            let after = d - c;
            let after_len = after.length();
            let same_way = after_len > EPS && (after / after_len - u).length() < 0.01;
            if same_way && out_len < S_BEND_MAX && u.dot(v).abs() < 0.01 {
                // The run in shares its first half with the corner before it (if any),
                // the run out its far half with the corner after it.
                let reach = (out_len * 1.6).max(6.0).min(in_len * 0.5).min(after_len * 0.5);
                let (p0, p3) = (b - u * reach, c + u * reach);
                let (p1, p2) = (b, c);
                const S_STEPS: usize = 12;
                for s in 0..=S_STEPS {
                    let t = s as f32 / S_STEPS as f32;
                    let mt = 1.0 - t;
                    let p = p0.to_vec2() * (mt * mt * mt)
                        + p1.to_vec2() * (3.0 * mt * mt * t)
                        + p2.to_vec2() * (3.0 * mt * t * t)
                        + p3.to_vec2() * (t * t * t);
                    out.push(p.to_pos2());
                }
                k += 2;
                continue;
            }
        }

        let want = radii.get(k).copied().unwrap_or(0.0);
        let r = want.min(in_len * 0.5).min(out_len * 0.5);
        if r < 0.5 {
            out.push(b);
            k += 1;
            continue;
        }
        // A quarter circle (the turns are all right angles): from r before the corner
        // to r after it, around the center r in from both.
        let center = b - u * r + v * r;
        let from: Vec2 = -v * r;
        for s in 0..=STEPS {
            let t = s as f32 / STEPS as f32 * std::f32::consts::FRAC_PI_2;
            let (sin, cos) = t.sin_cos();
            out.push(center + from * cos + u * r * sin);
        }
        k += 1;
    }
    out.push(path[n - 1]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: f32 = 40.0;

    fn params() -> RouteParams {
        RouteParams { block: G, side_clear: 13.0, end_clear: 4.0, lane_gap: 2.0, radius: 10.0 }
    }

    /// A node's frame spanning blocks `(bx, by)` by `(w, h)` on the grid, edges on block
    /// centers.
    fn node(bx: f32, by: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2((bx + 0.5) * G, (by + 0.5) * G), egui::vec2(w * G, h * G))
    }

    fn out_pin(r: Rect, y: f32) -> Pos2 {
        pos2(r.max.x + 7.0, y)
    }

    fn in_pin(r: Rect, y: f32) -> Pos2 {
        pos2(r.min.x - 7.0, y)
    }

    fn wire(nodes: &[Rect], a: usize, ay: f32, b: usize, by: f32, net: u64) -> RouteWire {
        RouteWire { from: out_pin(nodes[a], ay), to: in_pin(nodes[b], by), from_node: a, to_node: b, net, width: 2.0 }
    }

    fn assert_orthogonal(path: &[Pos2]) {
        for s in path.windows(2) {
            assert!(
                (s[0].x - s[1].x).abs() < EPS || (s[0].y - s[1].y).abs() < EPS,
                "diagonal segment {s:?} in {path:?}"
            );
        }
    }

    fn assert_clear_of(path: &[Pos2], r: Rect) {
        for s in path.windows(2) {
            let seg = Rect::from_two_pos(s[0], s[1]);
            assert!(!seg.intersects(r.shrink(0.5)), "segment {s:?} crosses node {r:?}");
        }
    }

    fn verticals(p: &[Pos2]) -> Vec<f32> {
        p.windows(2).filter(|s| is_vertical(s[0], s[1]) && (s[0].y - s[1].y).abs() > 1.0).map(|s| s[0].x).collect()
    }

    #[test]
    fn facing_pins_connect_straight() {
        let nodes = [node(0.0, 0.0, 3.0, 2.0), node(5.0, 0.0, 3.0, 2.0)];
        let y = nodes[0].min.y + 30.0;
        let routes = route_wires(&nodes, &[wire(&nodes, 0, y, 1, y, 1)], &params());
        assert_eq!(routes.paths[0].as_ref().unwrap().len(), 2);
    }

    #[test]
    fn a_lower_target_drops_in_a_gap() {
        let nodes = [node(0.0, 0.0, 3.0, 2.0), node(5.0, 3.0, 3.0, 2.0)];
        let (ay, by) = (nodes[0].min.y + 30.0, nodes[1].min.y + 30.0);
        let routes = route_wires(&nodes, &[wire(&nodes, 0, ay, 1, by, 1)], &params());
        let path = routes.paths[0].as_ref().unwrap();
        assert_orthogonal(path);
        let v = verticals(path);
        assert_eq!(v.len(), 1, "{path:?}");
        assert!(on_block(v[0], G), "{path:?}");
    }

    #[test]
    fn a_node_in_the_way_is_walked_around() {
        let nodes = [node(0.0, 0.0, 3.0, 2.0), node(4.0, -2.0, 3.0, 6.0), node(8.0, 0.0, 3.0, 2.0)];
        let y = nodes[0].min.y + 30.0;
        let routes = route_wires(&nodes, &[wire(&nodes, 0, y, 2, y, 1)], &params());
        let path = routes.paths[0].as_ref().unwrap();
        assert_orthogonal(path);
        assert_clear_of(path, nodes[1]);
    }

    #[test]
    fn a_feedback_wire_goes_around_its_own_nodes() {
        let nodes = [node(0.0, 0.0, 3.0, 2.0), node(4.0, 0.0, 3.0, 2.0)];
        let (ay, by) = (nodes[1].min.y + 30.0, nodes[0].min.y + 50.0);
        let routes = route_wires(&nodes, &[wire(&nodes, 1, ay, 0, by, 1)], &params());
        let path = routes.paths[0].as_ref().unwrap();
        assert_orthogonal(path);
        for &r in &nodes {
            assert_clear_of(&path[1..path.len() - 1], r);
        }
    }

    #[test]
    fn wires_sharing_a_gap_run_side_by_side() {
        // Two sources stacked on the left, two targets stacked lower right: both wires
        // drop down the same gap.
        let nodes = [
            node(0.0, 0.0, 3.0, 1.0),
            node(0.0, 2.0, 3.0, 1.0),
            node(4.0, 5.0, 3.0, 1.0),
            node(4.0, 7.0, 3.0, 1.0),
        ];
        let y = |i: usize| nodes[i].center().y;
        let wires = [wire(&nodes, 0, y(0), 2, y(2), 1), wire(&nodes, 1, y(1), 3, y(3), 2)];
        let routes = route_wires(&nodes, &wires, &params());
        let (a, b) = (routes.paths[0].as_ref().unwrap(), routes.paths[1].as_ref().unwrap());
        assert_orthogonal(a);
        assert_orthogonal(b);
        let (xa, xb) = (verticals(a)[0], verticals(b)[0]);
        assert!((xa - xb).abs() >= 3.9, "lanes overlap: {a:?} {b:?}");
    }

    #[test]
    fn wires_from_one_pin_get_their_own_lanes() {
        let nodes = [node(0.0, 0.0, 3.0, 1.0), node(4.0, 3.0, 3.0, 1.0), node(4.0, 5.0, 3.0, 1.0)];
        let y = |i: usize| nodes[i].center().y;
        let wires = [wire(&nodes, 0, y(0), 1, y(1), 7), wire(&nodes, 0, y(0), 2, y(2), 7)];
        let routes = route_wires(&nodes, &wires, &params());
        let (a, b) = (routes.paths[0].as_ref().unwrap(), routes.paths[1].as_ref().unwrap());
        assert_orthogonal(a);
        assert_orthogonal(b);
        let (xa, xb) = (verticals(a)[0], verticals(b)[0]);
        assert!((xa - xb).abs() >= 3.9, "{a:?} {b:?}");
    }

    #[test]
    fn off_grid_neighbors_close_together_still_route_around() {
        // Frames 30 points apart, off the grid: no block line fits between them.
        let a = Rect::from_min_size(pos2(13.0, 7.0), egui::vec2(150.0, 60.0));
        let b = Rect::from_min_size(pos2(193.0, 91.0), egui::vec2(150.0, 60.0));
        let c = Rect::from_min_size(pos2(193.0, -80.0), egui::vec2(150.0, 140.0));
        let nodes = [a, b, c];
        let w = wire(&nodes, 0, a.min.y + 20.0, 1, b.min.y + 30.0, 1);
        let routes = route_wires(&nodes, &[w], &params());
        let path = routes.paths[0].as_ref().unwrap();
        assert_orthogonal(path);
        for r in nodes {
            assert_clear_of(&path[1..path.len() - 1], r);
        }
    }

    #[test]
    fn a_buried_pin_still_gets_a_square_route() {
        // The target's input side is under another node: the wire goes in through it.
        let nodes = [node(0.0, 0.0, 3.0, 1.0), node(5.0, 0.0, 3.0, 1.0), node(4.0, -1.0, 1.5, 3.0)];
        let y = nodes[0].center().y;
        let routes = route_wires(&nodes, &[wire(&nodes, 0, y, 1, y + 3.0, 1)], &params());
        assert_orthogonal(routes.paths[0].as_ref().unwrap());
    }

    #[test]
    fn thick_wires_get_wider_lanes() {
        let nodes = [
            node(0.0, 0.0, 3.0, 1.0),
            node(0.0, 2.0, 3.0, 1.0),
            node(4.0, 5.0, 3.0, 1.0),
            node(4.0, 7.0, 3.0, 1.0),
        ];
        let y = |i: usize| nodes[i].center().y;
        let mut wires = [wire(&nodes, 0, y(0), 2, y(2), 1), wire(&nodes, 1, y(1), 3, y(3), 2)];
        wires[0].width = 8.0;
        wires[1].width = 8.0;
        let routes = route_wires(&nodes, &wires, &params());
        let (a, b) = (routes.paths[0].as_ref().unwrap(), routes.paths[1].as_ref().unwrap());
        let (xa, xb) = (verticals(a)[0], verticals(b)[0]);
        // Edge to edge: half of each plus the clear space.
        assert!((xa - xb).abs() >= 8.0 + 2.0 - 0.01, "{a:?} {b:?}");
    }

    #[test]
    fn lanes_beside_a_wall_move_away_from_it() {
        // A wall just above y = 0, lots of room below: three lanes all end up below.
        let walls = [Rect::from_min_max(pos2(0.0, -50.0), pos2(100.0, -4.0))];
        let raw: Vec<Option<Vec<Pos2>>> = (0..3)
            .map(|i| {
                let x = 10.0 + i as f32;
                Some(vec![pos2(-20.0, 30.0 + x), pos2(-10.0, 30.0 + x), pos2(-10.0, 0.0), pos2(80.0, 0.0), pos2(80.0, 40.0 + x), pos2(90.0, 40.0 + x)])
            })
            .collect();
        let (paths, _, crowding) = spread_lanes(&raw, &[2.0; 3], &walls, &params(), G);
        assert!(crowding.is_empty());
        for p in paths.iter().flatten() {
            let y = p.iter().find(|q| q.x > 20.0 && q.x < 60.0).map(|q| q.y);
            let run = p.windows(2).find(|s| (s[0].y - s[1].y).abs() < EPS && s[0].x < 30.0 && s[1].x > 50.0);
            let y = run.map(|s| s[0].y).or(y).unwrap();
            // The wire's edge (it's 2 thick) stays out of the wall, which already keeps
            // the clearance from the node.
            assert!(y - 1.0 >= -4.0 - 0.01, "a lane rides the wall: {p:?}");
        }
    }

    fn frames_clear(curve: &[Pos2], frames: &[Rect]) -> bool {
        curve.windows(2).all(|s| frames.iter().all(|f| !segment_enters(s[0], s[1], f.shrink(1.0))))
    }

    #[test]
    fn a_taut_wire_with_nothing_in_the_way_is_one_plain_curve() {
        let nodes = [node(0.0, 0.0, 3.0, 2.0), node(6.0, 3.0, 3.0, 2.0)];
        let w = wire(&nodes, 0, nodes[0].min.y + 30.0, 1, nodes[1].min.y + 30.0, 1);
        let routes = route_wires(&nodes, &[w], &params());
        let walls: Vec<Rect> = nodes.iter().map(|r| wall(*r, &params())).collect();
        let mut berth = vec![0.0; nodes.len()];
        let curve = taut(routes.raw[0].as_ref().unwrap(), (w.from_node, w.to_node), &nodes, &walls, 10.0, 5.0, &mut berth);
        assert_eq!(curve.first(), Some(&w.from));
        assert_eq!(curve.last(), Some(&w.to));
        // Left to right and downhill all the way: a plain S, no corners kept.
        assert!(curve.len() > 4, "{curve:?}");
        for s in curve.windows(2) {
            assert!(s[1].x >= s[0].x - 1e-3 && s[1].y >= s[0].y - 1e-3, "{s:?}");
        }
        assert!(frames_clear(&curve, &nodes));
    }

    #[test]
    fn a_taut_wire_bends_around_a_node_in_the_way() {
        let nodes = [node(0.0, 0.0, 3.0, 2.0), node(4.0, -2.0, 3.0, 6.0), node(8.0, 0.0, 3.0, 2.0)];
        let y = nodes[0].min.y + 30.0;
        let w = wire(&nodes, 0, y, 2, y, 1);
        let routes = route_wires(&nodes, &[w], &params());
        let walls: Vec<Rect> = nodes.iter().map(|r| wall(*r, &params())).collect();
        let mut berth = vec![0.0; nodes.len()];
        let curve = taut(routes.raw[0].as_ref().unwrap(), (w.from_node, w.to_node), &nodes, &walls, 10.0, 5.0, &mut berth);
        assert!(frames_clear(&curve, &nodes), "{curve:?}");
        // It goes over or under the middle node rather than through it.
        let middle = nodes[1];
        assert!(curve.iter().any(|p| p.y < middle.min.y || p.y > middle.max.y));
        // Not along the grid: some of it runs on a slant.
        let slanted = curve.windows(2).any(|s| (s[0].x - s[1].x).abs() > 0.5 && (s[0].y - s[1].y).abs() > 0.5);
        assert!(slanted);
    }

    #[test]
    fn a_taut_wire_with_a_long_first_stub_does_not_panic() {
        // The crash of 2026-11-09: the wire runs 476 points along its pin's row before
        // it turns, longer than the lead's cap.
        let raw = [pos2(0.0, 0.0), pos2(476.0, 0.0), pos2(476.0, 200.0), pos2(520.0, 200.0), pos2(530.0, 200.0)];
        let curve = taut(&raw, (0, 0), &[], &[], 10.0, 5.0, &mut []);
        assert_eq!(curve.first(), raw.first());
        assert_eq!(curve.last(), raw.last());
    }

    #[test]
    fn a_taut_wire_whose_stubs_met_does_not_panic() {
        let raw = [pos2(0.0, 0.0), pos2(30.0, 0.0), pos2(60.0, 0.0)];
        let curve = taut(&raw, (0, 0), &[], &[], 10.0, 5.0, &mut []);
        assert_eq!(curve.last(), raw.last());
    }

    /// Random patches, both routed styles: whatever the layout (nodes overlapping, off
    /// the grid, wires into their own node, backwards, thick), nothing panics and every
    /// point is finite.
    #[test]
    fn random_patches_never_panic() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..300 {
            let count = 2 + (rnd() % 9) as usize;
            let nodes: Vec<Rect> = (0..count)
                .map(|_| {
                    let x = (rnd() % 1400) as f32 - 200.0 + (rnd() % 100) as f32 * 0.37;
                    let y = (rnd() % 1000) as f32 - 200.0 + (rnd() % 100) as f32 * 0.29;
                    let w = 40.0 + (rnd() % 400) as f32;
                    let h = 30.0 + (rnd() % 500) as f32;
                    Rect::from_min_size(pos2(x, y), egui::vec2(w, h))
                })
                .collect();
            let wires: Vec<RouteWire> = (0..(rnd() % 14) as usize)
                .map(|k| {
                    let (a, b) = ((rnd() as usize) % count, (rnd() as usize) % count);
                    let ay = nodes[a].min.y + (rnd() % nodes[a].height().max(1.0) as u64) as f32;
                    let by = nodes[b].min.y + (rnd() % nodes[b].height().max(1.0) as u64) as f32;
                    RouteWire {
                        from: pos2(nodes[a].max.x + 7.0, ay),
                        to: pos2(nodes[b].min.x - 7.0, by),
                        from_node: a,
                        to_node: b,
                        net: (rnd() % 4) + k as u64 * 0,
                        width: if rnd() % 3 == 0 { 9.0 } else { 2.4 },
                    }
                })
                .collect();
            let p = RouteParams { block: 56.0, side_clear: 13.8, end_clear: 4.0, lane_gap: 3.0, radius: 12.0 };
            let routes = route_wires(&nodes, &wires, &p);
            let walls: Vec<Rect> = nodes.iter().map(|r| wall(*r, &p)).collect();
            let mut berth = vec![0.0; nodes.len()];
            for (k, raw) in routes.raw.iter().enumerate() {
                let Some(raw) = raw else { continue };
                let ends = (wires[k].from_node, wires[k].to_node);
                let curve = taut(raw, ends, &nodes, &walls, 12.0, 5.0, &mut berth);
                let grid = rounded(routes.paths[k].as_ref().unwrap(), routes.radii[k].as_ref().unwrap());
                for pt in curve.iter().chain(&grid) {
                    assert!(pt.x.is_finite() && pt.y.is_finite(), "{raw:?}");
                }
            }
        }
    }

    /// Random routes straight into `taut`: stubs short and long (past every cap),
    /// corners anywhere, nodes or none. Nothing panics, every point is finite, and the
    /// curve runs pin to pin.
    #[test]
    fn random_routes_pulled_taut_never_panic() {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..3000 {
            let p = pos2((rnd() % 600) as f32, (rnd() % 600) as f32);
            let mut raw = vec![p, p + vec2(1.0 + (rnd() % 600) as f32, 0.0)];
            for k in 0..(rnd() % 6) as usize {
                let last = raw[raw.len() - 1];
                let len = (rnd() % 500) as f32 - 250.0;
                raw.push(if k % 2 == 0 { last + vec2(0.0, len) } else { last + vec2(len, 0.0) });
            }
            let last = raw[raw.len() - 1];
            raw.push(last + vec2(1.0 + (rnd() % 300) as f32, 0.0));
            let frames: Vec<Rect> = (0..(rnd() % 4) as usize)
                .map(|_| {
                    let at = pos2((rnd() % 900) as f32 - 100.0, (rnd() % 900) as f32 - 100.0);
                    Rect::from_min_size(at, egui::vec2(20.0 + (rnd() % 300) as f32, 20.0 + (rnd() % 300) as f32))
                })
                .collect();
            let walls: Vec<Rect> = frames.iter().map(|f| f.expand(10.0)).collect();
            let mut berth = vec![0.0; frames.len()];
            let curve = taut(&raw, (usize::MAX, usize::MAX), &frames, &walls, 10.0, 5.0, &mut berth);
            assert_eq!(curve.first(), raw.first(), "{raw:?}");
            assert_eq!(curve.last(), raw.last(), "{raw:?}");
            assert!(curve.iter().all(|q| q.x.is_finite() && q.y.is_finite()), "{raw:?}");
        }
    }

    #[test]
    fn inner_lanes_turn_tighter() {
        // A wire offset toward the inside of its turn gets a smaller radius.
        let old = [pos2(0.0, 0.0), pos2(40.0, 0.0), pos2(40.0, 80.0), pos2(80.0, 80.0), pos2(90.0, 80.0)];
        let (_, plain) = lay_out(&old, |_| 0.0, 10.0);
        // Segment 1 (vertical) moved left: inside of the first turn (east then south),
        // outside of the second (south then east).
        let (_, shifted) = lay_out(&old, |k| if k == 1 { -4.0 } else { 0.0 }, 10.0);
        assert!(shifted[1] < plain[1] && shifted[2] > plain[2], "{plain:?} {shifted:?}");
    }

    #[test]
    fn a_short_jog_eases_into_an_s() {
        // Pins 6 points out of line: no corners, a smooth S level at both ends.
        let path = [pos2(0.0, 0.0), pos2(50.0, 0.0), pos2(50.0, 6.0), pos2(100.0, 6.0)];
        let r = rounded(&path, &[0.0, 10.0, 10.0, 0.0]);
        assert_eq!(r.first(), path.first());
        assert_eq!(r.last(), path.last());
        // Monotonic both ways: no point turns back, as a corner pair's arcs would not.
        for w in r.windows(2) {
            assert!(w[1].x >= w[0].x - 1e-3 && w[1].y >= w[0].y - 1e-3, "{w:?}");
        }
        // Spread over more than the jog's own height.
        let moving: Vec<&Pos2> = r.iter().filter(|p| p.y > 0.05 && p.y < 5.95).collect();
        let span = moving.last().unwrap().x - moving.first().unwrap().x;
        assert!(span > 6.0, "{span}");
    }

    #[test]
    fn rounding_keeps_the_ends_and_stays_near_the_corner() {
        let path = [pos2(0.0, 0.0), pos2(20.0, 0.0), pos2(20.0, 40.0), pos2(50.0, 40.0)];
        let r = rounded(&path, &[0.0, 6.0, 6.0, 0.0]);
        assert_eq!(r.first(), path.first());
        assert_eq!(r.last(), path.last());
        for p in &r {
            assert!(distance_sq(&path, *p) < 3.0 * 3.0, "{p:?} strays");
        }
    }
}
