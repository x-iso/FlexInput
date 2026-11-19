//! FLEXINPUT PATCH: the layout grid.
//!
//! The canvas is divided into square blocks. Node frames sit with their edges on block
//! centers and are a whole number of blocks in size, so no two nodes ever share a block:
//! between any two there is at least one block's gap, and the block lines run down the
//! middle of every gap. Wires are routed along those lines (see [`super::route`]).
//!
//! Dragging snaps a node to the grid as it goes (Alt drags freely). A node dropped onto
//! others, or one that grows into them, pushes them out of its way.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    sync::Arc,
};

use egui::{Color32, Context, Id, Painter, Pos2, Rect, Stroke, Vec2, pos2, vec2};

use super::route::{self, Crowding, RouteParams, RouteWire};
use crate::{InPinId, NodeId, OutPinId};

/// How the grid is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum GridStyle {
    /// Thin solid lines.
    Lines,
    /// Thin dashed lines.
    Dashed,
    /// A small cross on each block corner.
    #[default]
    Crosses,
}

/// How connected wires are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum WireRouting {
    /// A plain curve from pin to pin, over whatever's in between.
    Direct,
    /// Around the nodes in the way, otherwise as straight as it can: a flowing curve.
    #[default]
    Around,
    /// Along the grid's lines, side by side where wires share a gap, like traces on a
    /// circuit board.
    Grid,
}

/// The layout grid's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GridLayout {
    /// Size of one block, in graph points.
    pub block: f32,
    /// Draw the grid.
    pub show: bool,
    /// How it's drawn.
    pub style: GridStyle,
    /// Every this many blocks a line is drawn a little brighter (0: none are).
    pub accent_every: u32,
    /// How bright it's drawn, times its usual faintness (1 is usual).
    pub brightness: f32,
    /// Snap nodes to the grid: positions as they're dragged, sizes to whole blocks.
    pub snap: bool,
    /// Push nodes out of the way of one that's dropped onto them or grows into them.
    pub push: bool,
    /// How connected wires are drawn.
    pub wires: WireRouting,
    /// How far wires going around nodes keep from them, beyond the pins' room.
    pub clearance: f32,
    /// FLEXINPUT PATCH: how long a node takes to ease to where it has moved (a grid step
    /// while dragged, a push, an arrange), and a routed wire to its new route, in seconds.
    /// 0 jumps.
    pub ease: f32,
}

impl Default for GridLayout {
    fn default() -> Self {
        Self {
            block: 56.0,
            show: true,
            style: GridStyle::Crosses,
            accent_every: 8,
            brightness: 1.0,
            snap: true,
            push: true,
            wires: WireRouting::Around,
            clearance: 12.0,
            ease: 0.0,
        }
    }
}

impl GridLayout {
    /// A length rounded up to whole blocks (at least one). Half a point of slack keeps a
    /// length that's a whole number of blocks but for float noise from growing a block.
    #[must_use]
    pub fn snap_len(&self, len: f32) -> f32 {
        ((len - 0.5) / self.block).ceil().max(1.0) * self.block
    }

    /// The inner size of a node whose frame adds `margins` (left + right, top + bottom),
    /// grown so the frame is a whole number of blocks.
    #[must_use]
    pub fn snap_inner_size(&self, inner: Vec2, margins: Vec2) -> Vec2 {
        vec2(
            self.snap_len(inner.x + margins.x) - margins.x,
            self.snap_len(inner.y + margins.y) - margins.y,
        )
    }

    /// The node position nearest `pos` that puts the frame's top-left corner, `offset`
    /// up and left of the node's position, on a block center.
    #[must_use]
    pub fn snap_pos(&self, pos: Pos2, offset: Vec2) -> Pos2 {
        let corner = pos - offset;
        let b = self.block;
        pos2(((corner.x / b - 0.5).round() + 0.5) * b, ((corner.y / b - 0.5).round() + 0.5) * b) + offset
    }

    /// How far to move a frame to put its top-left corner on the nearest block center.
    #[must_use]
    pub fn snap_offset(&self, frame: Rect) -> Vec2 {
        self.snap_pos(frame.min, Vec2::ZERO) - frame.min
    }

    /// The blocks a frame takes up, shrunk a point so neighbors one gap apart don't touch.
    fn footprint(&self, frame: Rect) -> Rect {
        frame.expand(self.block * 0.5 - 1.0)
    }
}

/// Draws the grid across `viewport` in its style, a screen pixel thin and faint enough to
/// stay behind the wires, with every `accent_every`th line a little brighter. Zooming out
/// past the point where blocks are tellable apart steps it to every second, fourth, eighth
/// block rather than dropping out.
pub fn draw_grid(grid: &GridLayout, painter: &Painter, viewport: &Rect, scale: f32, color: Color32) {
    let b = grid.block.max(1.0);
    // Crosses, a pair of strokes per block corner, go sooner than lines.
    let fade_from = if grid.style == GridStyle::Crosses { 14.0 } else { 8.0 };
    // Zooming out used to drop the grid entirely once the blocks crowded. It doesn't: past the
    // point where blocks are too close to tell apart, the same lines are drawn every second,
    // fourth, eighth block instead, and they stay at a floor of brightness rather than fading
    // to nothing. A canvas with no grid at all loses the one thing the grid is for at that
    // zoom - saying how big a module is - and the stepped grid still says it.
    let mut step = 1i64;
    while b * scale * (step as f32) < fade_from && step < 128 {
        step *= 2;
    }
    let unit = b * step as f32;
    let fade = if step == 1 {
        ((b * scale - fade_from) / 16.0).clamp(0.0, 1.0)
    } else {
        // The stepped grid is a ruler, not a texture: dim, but there.
        ((unit * scale - fade_from) / 16.0).clamp(0.3, 1.0)
    };
    if fade <= 0.0 {
        return;
    }
    let (x0, x1) = ((viewport.min.x / unit).floor() as i64, (viewport.max.x / unit).ceil() as i64);
    let (y0, y1) = ((viewport.min.y / unit).floor() as i64, (viewport.max.y / unit).ceil() as i64);
    if x1 - x0 > 1000 || y1 - y0 > 1000 {
        return;
    }
    let px = 1.0 / scale.max(0.01);
    // A bright line has to fall on a line the stepped grid actually draws, and it stays an
    // odd number of blocks away so it keeps bracketing a four-block module: an odd multiple
    // of the step, the first one at least as wide as what was asked for.
    let every = {
        let want = i64::from(grid.accent_every.max(2));
        if step == 1 {
            want
        } else {
            let mut m = ((want + step - 1) / step).max(1);
            if m % 2 == 0 {
                m += 1;
            }
            step * m
        }
    };
    let accent = |k: i64| grid.accent_every > 1 && (k * step).rem_euclid(every) == 0;
    let bright = grid.brightness.max(0.0);
    let minor = Stroke::new(px, color.gamma_multiply((0.045 * bright).min(1.0) * fade));
    let major = Stroke::new(px, color.gamma_multiply((0.11 * bright).min(1.0) * fade));
    let stroke = |k: i64| if accent(k) { major } else { minor };
    match grid.style {
        GridStyle::Lines | GridStyle::Dashed => {
            let dashed = grid.style == GridStyle::Dashed;
            let line = |a: Pos2, c: Pos2, s: Stroke| {
                if dashed {
                    painter.extend(egui::Shape::dashed_line(&[a, c], s, 4.0 * px, 4.0 * px));
                } else {
                    painter.line_segment([a, c], s);
                }
            };
            for x in x0..=x1 {
                let xf = x as f32 * unit;
                line(pos2(xf, viewport.min.y), pos2(xf, viewport.max.y), stroke(x));
            }
            for y in y0..=y1 {
                let yf = y as f32 * unit;
                line(pos2(viewport.min.x, yf), pos2(viewport.max.x, yf), stroke(y));
            }
        }
        GridStyle::Crosses => {
            // Crosses are seen more than lines, so they take a little more of the color.
            let arm = (4.0 * px).min(unit * 0.2);
            for x in x0..=x1 {
                for y in y0..=y1 {
                    let s = if accent(x) && accent(y) { major } else { minor };
                    let s = Stroke::new(s.width, s.color.gamma_multiply(2.0));
                    let c = pos2(x as f32 * unit, y as f32 * unit);
                    painter.line_segment([c - vec2(arm, 0.0), c + vec2(arm, 0.0)], s);
                    painter.line_segment([c - vec2(0.0, arm), c + vec2(0.0, arm)], s);
                }
            }
        }
    }
}


/// A snapped drag in progress: where the dragged nodes started and how far the pointer
/// has gone since, so each frame's snap starts from the unsnapped spot.
#[derive(Clone, Debug, Default)]
pub struct GridDrag {
    pub origins: Vec<(NodeId, Pos2)>,
    pub moved: Vec2,
}

impl GridDrag {
    fn id(snarl_id: Id) -> Id {
        snarl_id.with("flexinput_grid_drag")
    }

    #[must_use]
    pub fn load(cx: &Context, snarl_id: Id) -> Option<Self> {
        cx.data(|d| d.get_temp::<Self>(Self::id(snarl_id)))
    }

    pub fn store(self, cx: &Context, snarl_id: Id) {
        cx.data_mut(|d| d.insert_temp(Self::id(snarl_id), self));
    }

    pub fn clear(cx: &Context, snarl_id: Id) {
        cx.data_mut(|d| d.remove::<Self>(Self::id(snarl_id)));
    }
}

/// What the grid remembers of each node between frames: its frame's size, to see it
/// grow, and the last press, to tell a node the user grew from one that grew on its own
/// (a patch loading, an undo).
#[derive(Clone, Debug, Default)]
pub struct GridMemory {
    sizes: HashMap<NodeId, Vec2>,
    /// Where (graph space) and when the primary button last went down.
    last_press: Option<(Pos2, f64)>,
    /// How many wires there were last frame, to see the user connect one.
    wires: usize,
    /// Rounds left of making room for crowded wires after the user changed something,
    /// and the nodes they placed, which stay put.
    settle: u8,
    movers: Vec<NodeId>,
}

impl GridMemory {
    fn id(snarl_id: Id) -> Id {
        snarl_id.with("flexinput_grid_memory")
    }

    #[must_use]
    pub fn load(cx: &Context, snarl_id: Id) -> Self {
        cx.data(|d| d.get_temp::<Self>(Self::id(snarl_id))).unwrap_or_default()
    }

    pub fn store(self, cx: &Context, snarl_id: Id) {
        cx.data_mut(|d| d.insert_temp(Self::id(snarl_id), self));
    }

    pub fn note_press(&mut self, at: Pos2, time: f64) {
        self.last_press = Some((at, time));
    }

    /// The button that last went down is still down: it counts as pressed just now, so
    /// a node grown by a long drag (a resize) still pushes.
    pub fn note_held(&mut self, time: f64) {
        if let Some((_, t)) = self.last_press.as_mut() {
            *t = time;
        }
    }

    /// Whether the user pressed within the last second.
    #[must_use]
    pub fn pressed_lately(&self, now: f64) -> bool {
        self.last_press.is_some_and(|(_, t)| now - t < 1.0)
    }

    /// The wire count noted last frame.
    #[must_use]
    pub const fn wire_count(&self) -> usize {
        self.wires
    }

    /// Notes this frame's wire count; true when a wire was just added.
    pub fn wire_added(&mut self, count: usize) -> bool {
        let added = count > self.wires;
        self.wires = count;
        added
    }

    /// The user changed the layout (`movers` placed): make room for crowded wires
    /// over the next few frames, as the wires reroute.
    pub fn settle(&mut self, movers: &[NodeId]) {
        self.settle = 3;
        self.movers = movers.to_vec();
    }

    /// Takes one round of making room, if one is due: the nodes the user placed.
    pub fn settle_round(&mut self) -> Option<Vec<NodeId>> {
        if self.settle == 0 {
            return None;
        }
        self.settle -= 1;
        Some(self.movers.clone())
    }

    /// A round moved nothing: the wires have their room.
    pub fn settled(&mut self) {
        self.settle = 0;
    }

    /// The nodes that grew this frame within a second of being pressed on, and forgets
    /// nodes that are gone.
    pub fn grown(&mut self, rects: &[(NodeId, Rect)], now: f64) -> Vec<NodeId> {
        let mut grown = Vec::new();
        let mut seen = HashSet::with_capacity(rects.len());
        for &(node, rect) in rects {
            seen.insert(node);
            let size = rect.size();
            let old = self.sizes.insert(node, size);
            let Some(old) = old else { continue };
            let bigger = size.x > old.x + 0.5 || size.y > old.y + 0.5;
            let pressed = self
                .last_press
                .is_some_and(|(at, t)| now - t < 1.0 && rect.expand(4.0).contains(at));
            if bigger && pressed {
                grown.push(node);
            }
        }
        self.sizes.retain(|node, _| seen.contains(node));
        grown
    }
}

/// Moves nodes out of the way of `movers`, which stay put: every node whose blocks
/// overlap a mover's is pushed along whichever axis takes the shorter push, away from
/// the mover, by whole blocks; a node pushed into others pushes them in turn.
///
/// `rects` are this frame's frames; returns how far to move each pushed node.
#[must_use]
pub fn push_apart(grid: &GridLayout, rects: &[(NodeId, Rect)], movers: &[NodeId]) -> Vec<(NodeId, Vec2)> {
    let mut frames: HashMap<NodeId, Rect> = rects.iter().copied().collect();
    let fixed: HashSet<NodeId> = movers.iter().copied().collect();
    let mut moved: HashMap<NodeId, Vec2> = HashMap::new();
    let mut queue: VecDeque<NodeId> = movers.iter().copied().filter(|n| frames.contains_key(n)).collect();
    let order: Vec<NodeId> = rects.iter().map(|&(n, _)| n).collect();
    let b = grid.block;
    let mut steps = 0;
    while let Some(m) = queue.pop_front() {
        steps += 1;
        if steps > 4 * rects.len() + 64 {
            break;
        }
        let mf = grid.footprint(frames[&m]);
        for &n in &order {
            if n == m || fixed.contains(&n) {
                continue;
            }
            let nf = grid.footprint(frames[&n]);
            if !mf.intersects(nf) {
                continue;
            }
            let (mc, nc) = (mf.center(), nf.center());
            let dx = if nc.x >= mc.x { mf.max.x - nf.min.x } else { -(nf.max.x - mf.min.x) };
            let dy = if nc.y >= mc.y { mf.max.y - nf.min.y } else { -(nf.max.y - mf.min.y) };
            let whole = |d: f32| d.signum() * (d.abs() / b).ceil() * b;
            let d = if dx.abs() <= dy.abs() { vec2(whole(dx), 0.0) } else { vec2(0.0, whole(dy)) };
            if d == Vec2::ZERO {
                continue;
            }
            frames.insert(n, frames[&n].translate(d));
            *moved.entry(n).or_default() += d;
            queue.push_back(n);
        }
    }
    let mut out: Vec<(NodeId, Vec2)> = moved.into_iter().filter(|(_, d)| *d != Vec2::ZERO).collect();
    out.sort_by_key(|(n, _)| n.0);
    out
}

/// Makes room for wires that ran out of it: where a crowded stretch of line runs
/// down the gap between two nodes, the node on one side moves away from the line by
/// as many blocks as the lanes need beyond the gap, and pushes whatever it lands on. The side
/// moved is the one without `movers` (what the user just placed stays where they put
/// it), or else the right / lower one.
#[must_use]
pub fn spread_crowded(
    grid: &GridLayout,
    rects: &[(NodeId, Rect)],
    crowding: &[Crowding],
    movers: &[NodeId],
) -> Vec<(NodeId, Vec2)> {
    let b = grid.block;
    let mut moves: HashMap<NodeId, Vec2> = HashMap::new();
    for c in crowding {
        let extra = (c.excess / b).ceil().max(1.0) * b;
        let overlaps = |lo: f32, hi: f32| lo < c.hi && hi > c.lo;
        let (before, after): (Vec<NodeId>, Vec<NodeId>) = {
            let mut before = Vec::new();
            let mut after = Vec::new();
            for &(n, r) in rects {
                let (near_lo, near_hi, along) = if c.vertical {
                    (r.max.x, r.min.x, overlaps(r.min.y, r.max.y))
                } else {
                    (r.max.y, r.min.y, overlaps(r.min.x, r.max.x))
                };
                if !along {
                    continue;
                }
                if near_lo <= c.coord && c.coord - near_lo < b {
                    before.push(n);
                } else if near_hi >= c.coord && near_hi - c.coord < b {
                    after.push(n);
                }
            }
            (before, after)
        };
        if before.is_empty() || after.is_empty() {
            continue;
        }
        let placed = |side: &[NodeId]| side.iter().any(|n| movers.contains(n));
        let (side, sign) = if placed(&after) && !placed(&before) { (before, -1.0) } else { (after, 1.0) };
        let by = if c.vertical { vec2(sign * extra, 0.0) } else { vec2(0.0, sign * extra) };
        for n in side {
            let d = moves.entry(n).or_default();
            // A node beside two crowded lines moves for the bigger of the two.
            if by.length() > d.length() {
                *d = by;
            }
        }
    }
    if moves.is_empty() {
        return Vec::new();
    }
    let moved: Vec<(NodeId, Rect)> = rects
        .iter()
        .map(|&(n, r)| (n, moves.get(&n).map_or(r, |d| r.translate(*d))))
        .collect();
    let shifted: Vec<NodeId> = moves.keys().copied().collect();
    for (n, d) in push_apart(grid, &moved, &shifted) {
        *moves.entry(n).or_default() += d;
    }
    let mut out: Vec<(NodeId, Vec2)> = moves.into_iter().collect();
    out.sort_by_key(|(n, _)| n.0);
    out
}

/// Routed wires, ready to draw and hit-test.
#[derive(Clone, Debug, Default)]
pub struct RouteSet {
    /// The route of each connected wire that found one, corners as computed.
    pub paths: HashMap<(OutPinId, InPinId), Vec<Pos2>>,
    /// The same with corners rounded, for drawing.
    pub drawn: HashMap<(OutPinId, InPinId), Vec<Pos2>>,
    /// Stretches of line that ran out of room for their wires.
    pub crowding: Vec<Crowding>,
}

#[derive(Clone, Default)]
struct RouteCache {
    key: u64,
    routes: Arc<RouteSet>,
    /// The settings and node frames the routes were made for.
    block: f32,
    pin_reach: f32,
    nodes: HashMap<NodeId, Rect>,
    /// Each wire's ends and its route before lanes were spread.
    wires: HashMap<(OutPinId, InPinId), (Pos2, Pos2, Option<Vec<Pos2>>)>,
}

/// Clear space between the edges of wires in neighboring lanes.
pub const WIRE_CLEARANCE: f32 = 3.0;

/// One connected wire: (out pin, in pin, out pin's center, in pin's center, how thick
/// it's drawn).
pub type WireEnds = (OutPinId, InPinId, Pos2, Pos2, f32);

/// The route a connected wire was last drawn along (graph space, corners not rounded),
/// if it was routed. `snarl_id` is the id the snarl was shown with.
#[must_use]
pub fn wire_route(cx: &Context, snarl_id: Id, out_pin: OutPinId, in_pin: InPinId) -> Option<Vec<Pos2>> {
    cx.data(|d| d.get_temp::<RouteCache>(snarl_id.with("flexinput_routes")))
        .and_then(|cache| cache.routes.paths.get(&(out_pin, in_pin)).cloned())
}

fn same_pos(a: Pos2, b: Pos2) -> bool {
    (a - b).length_sq() < 0.01
}

fn same_rect(a: Rect, b: Rect) -> bool {
    same_pos(a.min, b.min) && same_pos(a.max, b.max)
}

/// The routes for `wires` around `nodes`. Only wires something happened to are routed
/// again: ones whose ends moved, or whose route ran where a node moved, grew, came or
/// went.
#[must_use]
pub fn routes(
    cx: &Context,
    snarl_id: Id,
    grid: &GridLayout,
    pin_reach: f32,
    nodes: &[(NodeId, Rect)],
    wires: &[WireEnds],
) -> Arc<RouteSet> {
    let q = |v: f32| (v * 2.0).round() as i32;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    q(grid.block).hash(&mut h);
    q(pin_reach).hash(&mut h);
    (grid.wires as u8).hash(&mut h);
    q(grid.clearance).hash(&mut h);
    for (n, r) in nodes {
        (n.0, q(r.min.x), q(r.min.y), q(r.max.x), q(r.max.y)).hash(&mut h);
    }
    for (o, i, a, b, w) in wires {
        (o.node.0, o.output, i.node.0, i.input, q(a.x), q(a.y), q(b.x), q(b.y), q(*w)).hash(&mut h);
    }
    let key = h.finish();
    let id = snarl_id.with("flexinput_routes");
    let cache = cx.data(|d| d.get_temp::<RouteCache>(id)).unwrap_or_default();
    if cache.key == key && !cache.nodes.is_empty() {
        return cache.routes;
    }

    let params = RouteParams {
        block: grid.block,
        side_clear: pin_reach,
        end_clear: 4.0,
        lane_gap: WIRE_CLEARANCE,
        radius: 12.0,
    };
    let fresh = (cache.block - grid.block).abs() > 0.01 || (cache.pin_reach - pin_reach).abs() > 0.01;

    // Where something changed: around every node that moved, resized, came or went.
    let mut changed: Vec<Rect> = Vec::new();
    let current: HashMap<NodeId, Rect> = nodes.iter().copied().collect();
    for (n, r) in nodes {
        match cache.nodes.get(n) {
            Some(old) if same_rect(*old, *r) => {}
            Some(old) => {
                changed.push(route::wall(*old, &params));
                changed.push(route::wall(*r, &params));
            }
            None => changed.push(route::wall(*r, &params)),
        }
    }
    for (n, old) in &cache.nodes {
        if !current.contains_key(n) {
            changed.push(route::wall(*old, &params));
        }
    }

    let index: HashMap<NodeId, usize> = nodes.iter().enumerate().map(|(i, (n, _))| (*n, i)).collect();
    let rects: Vec<Rect> = nodes.iter().map(|(_, r)| *r).collect();
    let mut ends = Vec::with_capacity(wires.len());
    let mut route_wires = Vec::with_capacity(wires.len());
    let mut keep = Vec::with_capacity(wires.len());
    for &(o, i, a, b, width) in wires {
        let (Some(&from_node), Some(&to_node)) = (index.get(&o.node), index.get(&i.node)) else {
            continue;
        };
        let mut nh = std::collections::hash_map::DefaultHasher::new();
        (o.node.0, o.output).hash(&mut nh);
        route_wires.push(RouteWire { from: a, to: b, from_node, to_node, net: nh.finish(), width });
        ends.push((o, i));
        let kept = cache.wires.get(&(o, i)).and_then(|(ka, kb, raw)| {
            let raw = raw.as_ref()?;
            let untouched = !fresh
                && same_pos(*ka, a)
                && same_pos(*kb, b)
                && !changed.iter().any(|c| route::crosses(raw, *c));
            untouched.then(|| raw.clone())
        });
        keep.push(kept);
    }
    // The routing is geometry over whatever layout the patch has, every frame something
    // moves. Should it ever panic, the wires are drawn the plain way rather than the app
    // going down (the panic hook still logs it); the empty result is cached under this
    // layout's key, so it isn't tried again until something moves.
    let around = grid.wires == WireRouting::Around;
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let routed = route::route_some(&rects, &route_wires, &params, &keep);

        // Around the nodes: each wire's route pulled taut into a curve. The lanes are
        // for wires along the grid, and so is making room for them.
        let walls: Vec<Rect> =
            if around { rects.iter().map(|r| route::wall(*r, &params)).collect() } else { Vec::new() };
        let crowding = if around { Vec::new() } else { routed.crowding };
        let mut set = RouteSet { crowding, ..RouteSet::default() };
        let mut memo = HashMap::with_capacity(ends.len());
        for (k, &ends) in ends.iter().enumerate() {
            let w = &route_wires[k];
            memo.insert(ends, (w.from, w.to, routed.raw[k].clone()));
            if !around && let Some(path) = &routed.paths[k] {
                let radii = routed.radii[k].as_deref().unwrap_or(&[]);
                set.drawn.insert(ends, route::rounded(path, radii));
                set.paths.insert(ends, path.clone());
            }
        }
        if around {
            // Each wire pulled taut from its lane, the shortest first: every node a
            // wire goes round widens for the next, so wires going round a node
            // together stack beside each other, the shortest innermost.
            let mut order: Vec<usize> = (0..ends.len()).collect();
            let span = |k: usize| {
                let w = &route_wires[k];
                (w.to.x - w.from.x).abs() + (w.to.y - w.from.y).abs()
            };
            order.sort_by(|&a, &b| span(a).total_cmp(&span(b)));
            let mut berth = vec![0.0_f32; rects.len()];
            for k in order {
                let w = &route_wires[k];
                let Some(route) = routed.paths[k].as_ref().or(routed.raw[k].as_ref()) else { continue };
                let pitch = w.width + WIRE_CLEARANCE;
                let curve = route::taut(route, (w.from_node, w.to_node), &rects, &walls, grid.clearance, pitch, &mut berth);
                set.drawn.insert(ends[k], curve.clone());
                set.paths.insert(ends[k], curve);
            }
        }
        (set, memo)
    }));
    let (set, memo) = built.unwrap_or_default();
    let routes = Arc::new(set);
    let cache = RouteCache {
        key,
        routes: Arc::clone(&routes),
        block: grid.block,
        pin_reach,
        nodes: current,
        wires: memo,
    };
    cx.data_mut(|d| d.insert_temp(id, cache));
    routes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 40-point grid: round numbers for the expectations below.
    fn grid() -> GridLayout {
        GridLayout { block: 40.0, ..GridLayout::default() }
    }

    #[test]
    fn sizes_round_up_to_whole_blocks_and_stay() {
        let g = grid();
        let m = vec2(14.0, 14.0);
        let s = g.snap_inner_size(vec2(100.0, 50.0), m);
        assert_eq!(s + m, vec2(120.0, 80.0));
        assert_eq!(g.snap_inner_size(s, m), s);
        assert_eq!(g.snap_inner_size(s + vec2(0.3, 0.3), m), s);
    }

    #[test]
    fn positions_put_the_frame_corner_on_a_block_center() {
        let g = grid();
        let offset = vec2(7.0, 7.0);
        let p = g.snap_pos(pos2(53.0, -31.0), offset);
        let corner = p - offset;
        assert_eq!(corner, pos2(60.0, -20.0));
    }

    #[test]
    fn a_dropped_node_pushes_its_neighbor_clear() {
        let g = grid();
        let b = g.block;
        let a = Rect::from_min_size(pos2(0.5 * b, 0.5 * b), vec2(3.0 * b, 2.0 * b));
        // The neighbor's left edge two blocks into `a`.
        let n = Rect::from_min_size(pos2(2.5 * b, 0.5 * b), vec2(3.0 * b, 2.0 * b));
        let pushed = push_apart(&g, &[(NodeId(0), a), (NodeId(1), n)], &[NodeId(0)]);
        assert_eq!(pushed, vec![(NodeId(1), vec2(2.0 * b, 0.0))]);
    }

    #[test]
    fn neighbors_one_gap_apart_stay() {
        let g = grid();
        let b = g.block;
        let a = Rect::from_min_size(pos2(0.5 * b, 0.5 * b), vec2(3.0 * b, 2.0 * b));
        let n = Rect::from_min_size(pos2(4.5 * b, 0.5 * b), vec2(3.0 * b, 2.0 * b));
        assert!(push_apart(&g, &[(NodeId(0), a), (NodeId(1), n)], &[NodeId(0)]).is_empty());
    }

    #[test]
    fn a_crowded_gap_widens_away_from_what_the_user_placed() {
        let g = grid();
        let b = g.block;
        let left = Rect::from_min_size(pos2(0.5 * b, 0.5 * b), vec2(3.0 * b, 4.0 * b));
        let right = Rect::from_min_size(pos2(4.5 * b, 0.5 * b), vec2(3.0 * b, 4.0 * b));
        let rects = [(NodeId(0), left), (NodeId(1), right)];
        // Lanes 20 points wider than the gap down its line: one more block.
        let crowd = Crowding { vertical: true, coord: 4.0 * b, lo: b, hi: 3.0 * b, lanes: 9, excess: 20.0 };
        assert_eq!(spread_crowded(&g, &rects, &[crowd], &[]), vec![(NodeId(1), vec2(b, 0.0))]);
        // The user just put the right one there: the left one gives way.
        assert_eq!(spread_crowded(&g, &rects, &[crowd], &[NodeId(1)]), vec![(NodeId(0), vec2(-b, 0.0))]);
        // A crowded line with nothing on one side moves nothing.
        let open = Crowding { coord: 12.0 * b, ..crowd };
        assert!(spread_crowded(&g, &rects, &[open], &[]).is_empty());
    }

    #[test]
    fn a_frame_snaps_to_the_nearest_block_center() {
        let g = grid();
        let b = g.block;
        let frame = Rect::from_min_size(pos2(1.3 * b, 0.9 * b), vec2(2.0 * b, b));
        let moved = frame.translate(g.snap_offset(frame));
        assert_eq!(moved.min, pos2(1.5 * b, 0.5 * b));
    }

    #[test]
    fn pushes_cascade() {
        let g = grid();
        let b = g.block;
        let r = |x: f32| Rect::from_min_size(pos2((x + 0.5) * b, 0.5 * b), vec2(2.0 * b, b));
        let rects = [(NodeId(0), r(0.0)), (NodeId(1), r(2.0)), (NodeId(2), r(5.0))];
        let pushed = push_apart(&g, &rects, &[NodeId(0)]);
        assert_eq!(pushed, vec![(NodeId(1), vec2(b, 0.0)), (NodeId(2), vec2(b, 0.0))]);
    }
}
