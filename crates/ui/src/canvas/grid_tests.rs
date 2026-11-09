//! The layout grid through the real canvas: node sizes, drag snapping, pushes
//! and routed wires, rendered headless.

use super::*;
use crate::canvas::viewer::{node_rects_id, NodeRectMap};
use crate::touch::tests::input;
use egui::emath::TSTransform;
use egui_snarl::ui::{wire_route, GridLayout, SnarlState, WireRouting};

const B: f32 = 56.0;

struct Rig {
    ctx: egui::Context,
    canvas: Canvas,
    descriptors: Vec<ModuleDescriptor>,
    time: f64,
    to_global: TSTransform,
    snarl_id: egui::Id,
    grid: GridLayout,
}

impl Rig {
    /// Nodes with one input and one output each, at `at`.
    fn new(at: &[egui::Pos2]) -> (Self, Vec<NodeId>) {
        Self::with("test.module", at)
    }

    /// Nodes of module `id`, with one input and one output each, at `at`.
    fn with(id: &'static str, at: &[egui::Pos2]) -> (Self, Vec<NodeId>) {
        let desc = ModuleDescriptor {
            id,
            display_name: "Test",
            category: "Test",
            inputs: vec![PinDescriptor::new("in", SignalType::Float)],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        };
        let mut canvas = Canvas::new();
        let ids = at.iter().map(|&p| canvas.snarl.insert_node(p, NodeData::from(&desc))).collect();
        let mut rig = Rig {
            ctx: egui::Context::default(),
            canvas,
            descriptors: vec![desc],
            time: 0.0,
            to_global: TSTransform::IDENTITY,
            snarl_id: egui::Id::NULL,
            grid: GridLayout::default(),
        };
        rig.idle(6);
        (rig, ids)
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        self.time += 1.0 / 30.0;
        let mut raw = input(self.time, events);
        raw.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0)));
        let Rig { ctx, canvas, descriptors, to_global, snarl_id, grid, .. } = self;
        let (ids, signals) = (HashSet::new(), HashMap::new());
        let _ = ctx.run(raw, |ctx| {
            set_canvas_grid(ctx, Some(*grid));
            egui::CentralPanel::default().show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                let id = ui.make_persistent_id(("flexinput_canvas", canvas.view_salt));
                canvas.show(
                    descriptors, &ids, &signals, &signals,
                    &crate::app::PanicShortcut::default(), &[], &HashMap::new(),
                    DeviceParamDefaults::default(), ui, None, None,
                );
                *to_global = SnarlState::load(ui.ctx(), id, &canvas.snarl, rect, 0.2, 2.0).to_global();
                *snarl_id = id;
            });
        });
    }

    fn idle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame(vec![]);
        }
    }

    /// A node's frame, as last drawn (graph space).
    fn frame_rect(&self, node: NodeId) -> egui::Rect {
        let map = self
            .ctx
            .data(|d| d.get_temp::<NodeRectMap>(node_rects_id(self.canvas.view_salt)))
            .unwrap_or_default();
        let r = map.get(&node.0).copied().expect("the node was drawn");
        egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
    }

    /// Drags with the mouse from `from` (graph space) by `by` (graph points).
    fn drag(&mut self, from: egui::Pos2, by: egui::Vec2) {
        let screen = |p: egui::Pos2, t: TSTransform| t * p;
        let start = screen(from, self.to_global);
        let end = screen(from + by, self.to_global);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frame(vec![egui::Event::PointerMoved(start)]);
        self.frame(vec![button(start, true)]);
        const STEPS: usize = 12;
        for s in 1..=STEPS {
            let p = start + (end - start) * (s as f32 / STEPS as f32);
            self.frame(vec![egui::Event::PointerMoved(p)]);
        }
        self.frame(vec![button(end, false)]);
        self.idle(4);
    }

    /// Somewhere on a node's frame to grab it by: the empty strip along its
    /// bottom edge, under the pins.
    fn header(&self, node: NodeId) -> egui::Pos2 {
        let r = self.frame_rect(node);
        egui::pos2(r.center().x, r.bottom() - 3.0)
    }
}

fn whole_blocks(v: f32) -> bool {
    ((v / B) - (v / B).round()).abs() < 1e-3
}

fn on_block_center(v: f32) -> bool {
    whole_blocks(v - B * 0.5)
}

#[test]
fn nodes_are_whole_blocks() {
    let (rig, ids) = Rig::new(&[egui::pos2(100.0, 300.0), egui::pos2(500.0, 300.0)]);
    for id in ids {
        let r = rig.frame_rect(id);
        assert!(whole_blocks(r.width()) && whole_blocks(r.height()), "{r:?}");
    }
}

#[test]
fn a_dragged_node_lands_on_a_block_center() {
    let (mut rig, ids) = Rig::new(&[egui::pos2(103.0, 297.0)]);
    let grab = rig.header(ids[0]);
    rig.drag(grab, egui::vec2(53.0, 31.0));
    let r = rig.frame_rect(ids[0]);
    assert!(on_block_center(r.min.x) && on_block_center(r.min.y), "{r:?}");
    // And it went the way it was dragged.
    assert!(r.min.x > 103.0 && r.min.y > 297.0, "{r:?}");
}

#[test]
fn a_node_dropped_on_another_pushes_it_clear() {
    let (mut rig, ids) = Rig::new(&[egui::pos2(100.0, 300.0), egui::pos2(500.0, 300.0)]);
    let (a, b) = (ids[0], ids[1]);
    let target = rig.frame_rect(b);
    let grab = rig.header(a);
    // Onto b's left half.
    let by = egui::pos2(target.left() + 20.0, target.top() + 10.0) - rig.frame_rect(a).min;
    rig.drag(grab, by);
    rig.idle(4);
    let (ra, rb) = (rig.frame_rect(a), rig.frame_rect(b));
    let footprint = |r: egui::Rect| r.expand(B * 0.5 - 1.0);
    assert!(!footprint(ra).intersects(footprint(rb)), "a {ra:?} b {rb:?}");
    assert!(rb != target, "b moved");
}

#[test]
fn a_wire_runs_down_a_gap_line() {
    let (mut rig, ids) = Rig::new(&[egui::pos2(100.0, 300.0), egui::pos2(500.0, 500.0)]);
    rig.grid.wires = WireRouting::Grid;
    let (out_pin, in_pin) = (OutPinId { node: ids[0], output: 0 }, InPinId { node: ids[1], input: 0 });
    rig.canvas.snarl.connect(out_pin, in_pin);
    // Put both on the grid first.
    for &id in &ids {
        let grab = rig.header(id);
        rig.drag(grab, egui::vec2(1.0, 1.0));
    }
    rig.idle(3);
    let path = wire_route(&rig.ctx, rig.snarl_id, out_pin, in_pin).expect("the wire was routed");
    assert!(path.len() >= 3, "{path:?}");
    for s in path.windows(2) {
        let (p, q) = (s[0], s[1]);
        assert!((p.x - q.x).abs() < 0.01 || (p.y - q.y).abs() < 0.01, "diagonal {s:?}");
        if (p.x - q.x).abs() < 0.01 && (p.y - q.y).abs() > 1.0 {
            assert!(whole_blocks(p.x), "vertical run off the block lines: {path:?}");
        }
    }
    // Clear of both nodes, between the pins.
    for &id in &ids {
        let r = rig.frame_rect(id).shrink(0.5);
        for s in path[1..path.len() - 1].windows(2) {
            assert!(!egui::Rect::from_two_pos(s[0], s[1]).intersects(r), "{s:?} crosses {r:?}");
        }
    }
}

#[test]
fn kit_modules_settle_on_one_width() {
    for module in ["logic.counter", "module.delay", "math.quantize", "logic.delay"] {
        let (mut rig, ids) = Rig::with(module, &[egui::pos2(100.0, 300.0)]);
        rig.idle(6);
        let settled = rig.frame_rect(ids[0]);
        assert!(whole_blocks(settled.width()) && whole_blocks(settled.height()), "{module}: {settled:?}");
        for _ in 0..20 {
            rig.idle(1);
            assert_eq!(rig.frame_rect(ids[0]).size(), settled.size(), "{module} changed size");
        }
    }
}

#[test]
fn a_kit_module_takes_the_width_its_grip_asks_for() {
    let (mut rig, ids) = Rig::with("module.delay", &[egui::pos2(100.0, 300.0)]);
    let before = rig.frame_rect(ids[0]).width();
    let wider = before + 3.0 * B;
    rig.canvas.snarl.get_node_mut(ids[0]).unwrap().params
        .insert(kit_width_param().into(), serde_json::json!(wider));
    rig.idle(6);
    assert_eq!(rig.frame_rect(ids[0]).width(), wider);
    // Narrower than its rows allow: it stops at the narrowest.
    rig.canvas.snarl.get_node_mut(ids[0]).unwrap().params
        .insert(kit_width_param().into(), serde_json::json!(B));
    rig.idle(6);
    // It goes back to the narrowest it can be, which is where it started.
    assert_eq!(rig.frame_rect(ids[0]).width(), before);
}

fn kit_width_param() -> &'static str {
    crate::canvas::viewer::kit_width_param()
}

#[test]
fn a_module_added_on_another_stays_put_and_pushes_it_aside() {
    let (mut rig, ids) = Rig::new(&[egui::pos2(300.0, 300.0)]);
    let old = rig.frame_rect(ids[0]);
    let at = old.min + egui::vec2(30.0, 20.0);
    let data = NodeData::from(&rig.descriptors[0]);
    let new = rig.canvas.snarl.insert_node_placed(at, data);
    rig.idle(8);
    let placed = rig.frame_rect(new);
    assert!(on_block_center(placed.min.x) && on_block_center(placed.min.y), "{placed:?}");
    assert!((placed.min - at).length() < B, "landed {placed:?}, asked for {at:?}");
    let pushed = rig.frame_rect(ids[0]);
    let footprint = |r: egui::Rect| r.expand(B * 0.5 - 1.0);
    assert!(!footprint(placed).intersects(footprint(pushed)), "{placed:?} {pushed:?}");
}

#[test]
fn a_wire_around_modules_bends_past_one_in_the_way() {
    // Source left, target right, a third node square in between on the pins' row.
    let (mut rig, ids) = Rig::new(&[
        egui::pos2(100.0, 300.0),
        egui::pos2(700.0, 300.0),
        egui::pos2(400.0, 285.0),
    ]);
    rig.grid.wires = WireRouting::Around;
    let (out_pin, in_pin) = (OutPinId { node: ids[0], output: 0 }, InPinId { node: ids[1], input: 0 });
    rig.canvas.snarl.connect(out_pin, in_pin);
    for &id in &ids {
        let grab = rig.header(id);
        rig.drag(grab, egui::vec2(1.0, 1.0));
    }
    // The middle one has to really be in the way of the straight line.
    rig.idle(3);
    let path = wire_route(&rig.ctx, rig.snarl_id, out_pin, in_pin).expect("the wire was routed");
    let middle = rig.frame_rect(ids[2]).shrink(1.0);
    let (from, to) = (path[0], path[path.len() - 1]);
    assert!(crosses_rect(from, to, middle), "the setup has the middle node off the straight line");
    for s in path.windows(2) {
        assert!(!egui::Rect::from_two_pos(s[0], s[1]).intersects(middle) || !crosses_rect(s[0], s[1], middle),
            "{s:?} crosses {middle:?}");
    }
    assert!(path.len() > 4, "a curve, not a line: {path:?}");
}

/// Whether the segment a-b passes through r (sampled finely).
fn crosses_rect(a: egui::Pos2, b: egui::Pos2, r: egui::Rect) -> bool {
    (0..=32).any(|i| r.contains(a + (b - a) * (i as f32 / 32.0)))
}

#[test]
fn hovering_a_module_focuses_its_wires() {
    let (mut rig, ids) = Rig::new(&[egui::pos2(100.0, 300.0), egui::pos2(600.0, 300.0)]);
    let mut settings = crate::settings::CanvasGrid::default();
    settings.focus = 1.0;
    rig.ctx.data_mut(|d| d.insert_temp(egui::Id::new(CANVAS_GRID_SETTINGS_KEY), settings));
    rig.canvas.snarl.connect(OutPinId { node: ids[0], output: 0 }, InPinId { node: ids[1], input: 0 });
    let focus_id = rig.snarl_id.with("flexinput_wire_focus");
    let focus = |rig: &Rig| rig.ctx.data(|d| d.get_temp::<Vec<NodeId>>(focus_id)).unwrap_or_default();

    // Point at the second module (screen space, through the canvas's transform).
    let over = rig.to_global * rig.frame_rect(ids[1]).center();
    rig.frame(vec![egui::Event::PointerMoved(over)]);
    rig.idle(2);
    assert_eq!(focus(&rig), vec![ids[1]]);

    // Off every module: nothing new is focused (the last focus stays only to fade out).
    let away = rig.to_global * egui::pos2(380.0, 900.0);
    rig.frame(vec![egui::Event::PointerMoved(away)]);
    rig.idle(12);
    let anim = rig.ctx.animate_bool_with_time(focus_id, false, 0.15);
    assert!(anim <= 0.0, "still fading: {anim}");
}
