//! The layout grid through the real canvas: node sizes, drag snapping, pushes
//! and routed wires, rendered headless.

use super::*;
use crate::canvas::viewer::simple_bodies::Xysr;
use crate::canvas::viewer::{node_rects_id, NodeRectMap};
use crate::touch::tests::input;
use egui::emath::TSTransform;
use egui_snarl::ui::{wire_route, GridLayout, SnarlState, WireRouting};

/// The canvas's own block, so these tests measure the grid the app actually snaps to.
const B: f32 = crate::settings::CanvasGrid::BLOCK;
/// The major line: two blocks, the step the modules were designed against. Sizes and grip
/// drags that mean "112 points" or "drag it out two blocks" are written in these, so halving
/// the grid doesn't quietly halve what a test is asking for.
const MAJOR: f32 = 2.0 * B;

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
        Self::with_pins(id, &["in"], &["out"], at)
    }

    /// Nodes of module `id` with pins named as given, at `at`.
    fn with_pins(id: &'static str, inputs: &[&str], outputs: &[&str], at: &[egui::Pos2]) -> (Self, Vec<NodeId>) {
        Self::build(
            ModuleDescriptor {
                id,
                display_name: "Test",
                category: "Test",
                inputs: inputs.iter().map(|&n| PinDescriptor::new(n, SignalType::Any)).collect(),
                outputs: outputs.iter().map(|&n| PinDescriptor::new(n, SignalType::Any)).collect(),
            },
            at,
        )
    }

    /// Nodes of a real registered module: its own name and its own pins, at `at`.
    /// Sizes come out of the name as much as out of the controls, so measuring a
    /// made-up "Test" only proves something about made-up names.
    fn real(id: &'static str, at: &[egui::Pos2]) -> (Self, Vec<NodeId>) {
        let desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == id)
            .expect("the module is registered");
        Self::build(desc, at)
    }

    /// As [`Self::real`], but with the grid's sizing off: what a module asks for out of
    /// its own content, with no block rounding to cover for it.
    fn real_no_snap(id: &'static str, at: &[egui::Pos2]) -> (Self, Vec<NodeId>) {
        let (mut rig, ids) = Self::real(id, at);
        rig.grid.snap = false;
        rig.idle(6);
        (rig, ids)
    }

    fn build(desc: ModuleDescriptor, at: &[egui::Pos2]) -> (Self, Vec<NodeId>) {
        let mut canvas = Canvas::new();
        let ids = at.iter().map(|&p| canvas.snarl.insert_node(p, NodeData::from(&desc))).collect();
        let mut rig = Rig {
            ctx: egui::Context::default(),
            canvas,
            descriptors: vec![desc],
            time: 0.0,
            to_global: TSTransform::IDENTITY,
            snarl_id: egui::Id::NULL,
            grid: GridLayout { block: B, ..GridLayout::default() },
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

    /// One left click at `at` (graph space).
    fn click(&mut self, at: egui::Pos2) {
        self.tap(at);
        self.idle(4);
    }

    /// Press and release at `at`, with no frames in between: the atom of a double-click,
    /// which has to happen inside egui's interval for one.
    fn tap(&mut self, at: egui::Pos2) {
        let p = self.to_global * at;
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frame(vec![egui::Event::PointerMoved(p)]);
        self.frame(vec![button(p, true)]);
        self.frame(vec![button(p, false)]);
    }

    /// Two taps on the same spot: what opens an inline edit (a pill's value, a strip's
    /// name).
    fn double_click(&mut self, at: egui::Pos2) {
        self.tap(at);
        self.tap(at);
        self.idle(2);
    }

    /// Types into whatever holds the keyboard, then presses Enter.
    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.frame(vec![egui::Event::Text(ch.to_string())]);
        }
        self.frame(vec![egui::Event::Key {
            key: egui::Key::Enter,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
            physical_key: None,
        }]);
        self.idle(2);
    }

    /// Where a body painted one of its controls, as the body traced it.
    fn traced(&self, node: NodeId, what: &str) -> egui::Rect {
        crate::canvas::viewer::simple_bodies::traced_rect(&self.ctx, node, what)
            .unwrap_or_else(|| panic!("the body never traced {what:?}"))
    }
    /// The same, for things a body only traces when they exist at all.
    fn traced_rect(&self, node: NodeId, what: &str) -> Option<egui::Rect> {
        crate::canvas::viewer::simple_bodies::traced_rect(&self.ctx, node, what)
    }
    fn inputs(&self, node: NodeId) -> usize {
        self.canvas.snarl.get_node(node).map_or(0, |n| n.inputs.len())
    }

    /// Give a node `n` inputs, named after letters as the header stepper names them.
    fn set_inputs(&mut self, node: NodeId, n: usize) {
        const LETTERS: [&str; 10] = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];
        while self.inputs(node) < n {
            let name = LETTERS[self.inputs(node)];
            if let Some(node) = self.canvas.snarl.get_node_mut(node) {
                node.inputs.push(PinDescriptor::new(name, SignalType::Any));
            }
        }
        while self.inputs(node) > n {
            let last = self.inputs(node) - 1;
            self.canvas.snarl.drop_inputs(egui_snarl::InPinId { node, input: last });
            if let Some(node) = self.canvas.snarl.get_node_mut(node) {
                node.inputs.remove(last);
            }
        }
        self.idle(6);
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
    let wider = before + 3.0 * MAJOR;
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

fn quantize_steps() -> &'static [f32] {
    crate::canvas::viewer::quantize_step_values()
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

// The basic operations: their content is their pins, so they pay no body row for
// them. `в€’ +` sits in the header, the frame stays the smallest the grid allows, and
// it grows by a block only when the pins need one.

/// Where a node's header `в€’ +` sits: the stepper's two buttons are
/// `interact_size.y` on a side, `+` is the rightmost, `в€’` is a stepper's width in
/// from there, and the header band starts under the header frame's top margin вЂ” the
/// 4 px `Canvas::new` gives it (see the same note in `viewer::glow`).
fn stepper_points(rig: &Rig, node: NodeId) -> (egui::Pos2, egui::Pos2) {
    let h = rig.ctx.style().spacing.interact_size.y;
    let gap = 4.0; // kit::GAP
    let r = rig.frame_rect(node);
    let y = r.top() + 4.0 + h * 0.5;
    (egui::pos2(r.right() - (2.0 * h + gap) + h * 0.5, y), egui::pos2(r.right() - h * 0.5, y))
}

#[test]
fn an_operation_is_two_blocks_square() {
    // The five variadic operations (their mark and their `в€’ +` in the header), the
    // two comparisons (their mark, and one toggle under it) and the gates: a header
    // and pin rows, nothing else.
    for module in ["math.add", "math.subtract", "math.multiply", "math.divide", "math.min_max",
                   "math.clamp", "math.map_range", "math.abs", "math.quantize",
                   "module.axis_to_vec", "module.vec_to_axis",
                   "logic.greater_than", "logic.less_than",
                   "logic.and", "logic.or", "logic.not", "logic.xor",
                   "logic.nand", "logic.nor", "logic.xnor",
                   "logic.equal", "logic.not_equal"] {
        let (mut rig, ids) = Rig::real(module, &[egui::pos2(100.0, 300.0)]);
        let settled = rig.frame_rect(ids[0]);
        assert_eq!(settled.width(), 2.0 * MAJOR, "{module}: {settled:?} not the two blocks wide it should be");
        assert!(settled.height() <= 2.0 * MAJOR, "{module}: {settled:?} taller than its pins need");
        for _ in 0..10 {
            rig.idle(1);
            assert_eq!(rig.frame_rect(ids[0]).size(), settled.size(), "{module} did not settle");
        }
        // Sized right and drawn right are different claims: the mark is painted rather
        // than allocated, so a module can measure perfectly and still show an empty body
        // where its glyph should be - which is what happened to Abs and NOT. The painter
        // leaves the side it used behind, so that failure reaches a test.
        let side = rig
            .ctx
            .data(|d| d.get_temp::<i32>(egui::Id::new(("module_glyph_side", ids[0].0))));
        assert!(
            side.is_some_and(|s| s >= 16),
            "{module}: frame {settled:?} but no mark painted ({side:?})",
        );
    }
}

#[test]
fn five_inputs_fit_the_two_blocks_and_the_sixth_takes_a_third() {
    // Measured, so the trade is on the record: the stepper in the header costs no
    // height at all, the second block has room for five inputs, and the sixth is what
    // takes a third вЂ” and the third block comes back when the sixth pin goes.
    for n in 2..=5 {
        let (mut rig, ids) = Rig::real("math.add", &[egui::pos2(100.0, 300.0)]);
        rig.set_inputs(ids[0], n);
        let r = rig.frame_rect(ids[0]);
        assert!(r.height() <= 2.0 * MAJOR, "{n} inputs took a block early: {r:?}");
        assert_eq!(r.width(), 2.0 * MAJOR, "{n} inputs widened it: {r:?}");
    }
    let (mut rig, ids) = Rig::real("math.add", &[egui::pos2(100.0, 300.0)]);
    rig.set_inputs(ids[0], 6);
    let six = rig.frame_rect(ids[0]);
    // 140 rather than 168: at a 56-point grid the sixth input pushed it a whole old block
    // over; at 28 the row has somewhere to go that is half as big.
    assert_eq!(six.height(), 5.0 * B, "6 inputs: {six:?}");
    rig.set_inputs(ids[0], 5);
    assert!(rig.frame_rect(ids[0]).height() <= 2.0 * MAJOR, "the third block was not given back");
}

#[test]
fn the_header_stepper_adds_a_pin_row_and_gives_it_back() {
    let (mut rig, ids) = Rig::real("math.add", &[egui::pos2(100.0, 300.0)]);
    let at_start = rig.frame_rect(ids[0]).size();

    let (_, plus) = stepper_points(&rig, ids[0]);
    rig.click(plus);
    assert_eq!(rig.inputs(ids[0]), 3, "the + took no pin on");
    let grown = rig.frame_rect(ids[0]).size();
    assert!(whole_blocks(grown.x) && whole_blocks(grown.y), "{grown:?} after a pin");

    // And the other way: the same control takes it back, and the frame with it вЂ” a
    // frame that only ever grows is the failure this is guarding.
    let (minus, _) = stepper_points(&rig, ids[0]);
    rig.click(minus);
    assert_eq!(rig.inputs(ids[0]), 2, "the в€’ took no pin off");
    rig.idle(6);
    assert_eq!(rig.frame_rect(ids[0]).size(), at_start, "the frame never gave the room back");

    // And the pair stays inside the module: a point a few points past the frame's
    // right edge, on the header's row, is not part of the `+`. egui's own 8-point
    // item spacing once made the pair 8 wider than the box the header reserved for
    // it, and the `+` finished over the frame border.
    let r = rig.frame_rect(ids[0]);
    let y = r.top() + 4.0 + rig.ctx.style().spacing.interact_size.y * 0.5;
    rig.click(egui::pos2(r.right() + 4.0, y));
    assert_eq!(rig.inputs(ids[0]), 2, "the + reached outside the module's frame");
}

#[test]
fn an_operation_does_not_grow_from_its_corner() {
    // Greater / Less Than used to be on the kit and carry a grip for its one checkbox;
    // a frame that nothing stretches into is worse than no frame.
    for module in ["math.add", "math.min_max", "logic.and", "logic.greater_than", "module.constant"] {
        let (mut rig, ids) = Rig::real(module, &[egui::pos2(100.0, 300.0)]);
        let at_start = rig.frame_rect(ids[0]);
        // Where a kit module's corner grip would be, pulled out by three blocks.
        // Nothing in these has room to stretch into, so there's no grip to draw.
        rig.drag(at_start.max - egui::vec2(6.0, 6.0), egui::vec2(3.0 * MAJOR, 3.0 * MAJOR));
        let after = rig.frame_rect(ids[0]);
        assert_eq!(after.size(), at_start.size(), "{module} resized from its corner");
    }
}

#[test]
fn a_gate_has_no_stepper_to_click() {
    // Its two inputs are the engine's, so nothing sits at the header's right edge.
    let (mut rig, ids) = Rig::with_pins("logic.and", &["a", "b"], &["out"], &[egui::pos2(100.0, 300.0)]);
    let (minus, plus) = stepper_points(&rig, ids[0]);
    rig.click(plus);
    rig.click(minus);
    assert_eq!(rig.inputs(ids[0]), 2, "a gate was given a pin it can't read");
}

#[test]
fn a_one_row_module_is_still_square_for_its_mark() {
    // Abs and NOT have a single pin row between them. Without a band of their own they
    // come out a block shorter than every other mark-carrying module, and the mark has
    // nothing to sit in.
    for module in ["math.abs", "logic.not"] {
        let (mut rig, ids) = Rig::real(module, &[egui::pos2(100.0, 300.0)]);
        for _ in 0..10 {
            rig.idle(1);
        }
        let r = rig.frame_rect(ids[0]);
        assert_eq!(r.width(), 2.0 * MAJOR, "{module}: {r:?}");
        assert_eq!(r.height(), 2.0 * MAJOR, "{module}: {r:?} is a block shorter than its mark needs");
        // The frame being the right size is not the same as the mark being drawn. These
        // two came out 2x2 with nothing on them for a whole round: the band reserved *for*
        // the mark was also where the mark was told to start, which left six points of
        // band and no room to read. The painter leaves the side it used behind, so a mark
        // that quietly stops being painted fails a test instead of a screenshot.
        let side = rig
            .ctx
            .data(|d| d.get_temp::<i32>(egui::Id::new(("module_glyph_side", ids[0].0))));
        assert_eq!(side, Some(56), "{module}: frame is square but no mark was painted");
    }
}

/// Write a Constant's value, as the box would.
fn set_constant(rig: &mut Rig, node: egui_snarl::NodeId, v: f64) {
    rig.canvas
        .snarl
        .get_node_mut(node)
        .unwrap()
        .params
        .insert("value".to_string(), serde_json::json!(v));
}

/// Read it back.
fn constant(rig: &Rig, node: egui_snarl::NodeId) -> f64 {
    rig.canvas
        .snarl
        .get_node(node)
        .unwrap()
        .params
        .get("value")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

#[test]
fn a_constants_resolution_pill_drags_under_the_box() {
    // One pill under the number: `res` on the left, the digits it keeps on the right,
    // stepping whole numbers -> 1 -> 2 -> 3 -> 4 -> whole numbers again.
    let (mut rig, ids) = Rig::real("module.constant", &[egui::pos2(100.0, 300.0)]);
    set_constant(&mut rig, ids[0], 3.7);
    rig.idle(6);
    let f = rig.frame_rect(ids[0]);
    assert_eq!(
        (f.width(), f.height()),
        (2.0 * MAJOR, 2.0 * MAJOR),
        "box and pill together should stay two blocks square: {f:?}",
    );
    let res_of = |rig: &Rig| {
        rig.canvas
            .snarl
            .get_node(ids[0])
            .unwrap()
            .params
            .get("res")
            .and_then(|v| v.as_f64())
            .map(|r| r as i32)
    };
    assert_eq!(res_of(&rig), None, "a Constant that was never set shouldn't write a resolution");

    // The value cell answers along its whole width: the pointer's place across it picks
    // the setting, so the resolution is dragged across rather than clicked through.
    // The pill is as wide as the number box, and for `3.7` the box is its floor - the
    // width of `9999.99`, about 66 points - centred in a module 112 wide. So the pill
    // runs from about 33 points left of the module's centre to 33 right of it, its first
    // 34 being the label cell: the value cell starts just right of the centre.
    let left = f.center().x + 2.0;
    let right = f.center().x + 30.0;
    // The group is centred in the space under the header, not in the whole module, so the
    // pill sits noticeably below the module's middle: 54 points of box, gap and pill with
    // about 9 points of air above and below inside the frame.
    let y = f.center().y + 29.0;
    rig.click(egui::pos2(left, y));
    assert_eq!(res_of(&rig), Some(0), "the left end should be whole numbers");
    rig.click(egui::pos2(right, y));
    assert_eq!(res_of(&rig), Some(4), "the right end should be four digits");
    rig.drag(egui::pos2(right, y), egui::vec2(-30.0, 0.0));
    assert_eq!(res_of(&rig), Some(0), "dragging back left should return to whole numbers");
    rig.drag(egui::pos2(left, y), egui::vec2(30.0, 0.0));
    assert_eq!(res_of(&rig), Some(4), "and dragging right should go up again");
    // Where the pair used to sit - centred on the module - shouldn't answer any more.
    rig.click(egui::pos2(left, f.center().y + 17.0));
    assert_eq!(res_of(&rig), Some(4), "the pill moved down; the old spot still answers it");

    // Stepping the resolution doesn't rewrite the number. A `3.7` shown at whole numbers
    // reads `4`, but the tenths are still there for when the resolution steps back.
    assert_eq!(constant(&rig, ids[0]), 3.7, "the resolution clobbered the stored value");
}

#[test]
fn a_sample_hold_shows_in_and_out_in_two_blocks() {
    // Two blocks square: the mode moved to the header, which is where the name had to
    // give way to a mark, and the two readings are painted rather than allocated.
    let (mut rig, ids) = Rig::real("module.sample_hold", &[egui::pos2(100.0, 300.0)]);
    for _ in 0..8 {
        rig.idle(1);
    }
    let f = rig.frame_rect(ids[0]);
    assert_eq!(
        (f.width(), f.height()),
        (2.0 * MAJOR, 2.0 * MAJOR),
        "Sample and Hold should be two blocks square: {f:?}",
    );

    // A number on the wire doesn't change the module's size: the readings fit themselves
    // to the band, they don't ask the band to grow.
    let wide = |rig: &mut Rig, v: f32| {
        if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
            n.extra.last_signals = vec![
                Some(flexinput_core::Signal::Float(v)),
                Some(flexinput_core::Signal::Bool(false)),
            ];
            n.extra.last_out = vec![Some(flexinput_core::Signal::Float(v))];
        }
        rig.idle(3);
    };
    for v in [0.0f32, 1.5, -123456.0, 9.9e25] {
        wide(&mut rig, v);
        assert_eq!(
            rig.frame_rect(ids[0]).size(),
            f.size(),
            "the reading {v} resized the module to {:?}",
            rig.frame_rect(ids[0]),
        );
    }

    // The mode button is at the header's right edge, and one click switches it; the
    // second pin's name goes with it.
    let mode_of = |rig: &Rig| {
        rig.canvas
            .snarl
            .get_node(ids[0])
            .unwrap()
            .params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("sample")
            .to_string()
    };
    assert_eq!(mode_of(&rig), "sample", "the mode defaults to Sample");
    // The button is in the body, centred on the module under the pin rows: sweeping the
    // middle column finds it wherever the layout happens to have put it.
    let h = rig.frame_rect(ids[0]);
    let mut spot = None;
    for k in 0..18 {
        let y = h.top() + 22.0 + 4.0 * k as f32;
        rig.click(egui::pos2(h.center().x, y));
        if mode_of(&rig) == "hold" {
            spot = Some(y);
            break;
        }
    }
    let y = spot.unwrap_or_else(|| panic!("no mode button answered anywhere in {h:?}"));
    assert!(y > h.top() + 20.0 && y < h.bottom(), "the mode button sat outside the body: {y} in {h:?}");
    rig.click(egui::pos2(h.center().x, y));
    assert_eq!(mode_of(&rig), "sample", "and didn't switch it back");

    // The two readings land in the space under the pin rows, not up among the labels вЂ”
    // that is what made the input look like a stray dash next to the header.
    let lines = rig
        .ctx
        .data(|d| d.get_temp::<Xysr>(egui::Id::new(("sample_hold_lines", ids[0].0))))
        .unwrap_or_else(|| panic!("no readings painted in {h:?}"));
    let (in_y, out_y, size) = (lines.0[0], lines.0[1], lines.0[2]);
    assert!(in_y < out_y, "the input line ({in_y}) should sit above the output line ({out_y})");
    assert!(out_y < h.bottom() - 2.0, "the output reading ran past the frame: {out_y} in {h:?}");
    assert!(in_y > y + 14.0, "the input reading is up in the pin rows: {in_y}, button at {y}");
    assert!(size >= 10.0, "the readings shrank to {size}, too small to read");
}

#[test]
fn a_text_note_fills_the_frame_its_corner_grip_asks_for() {
    let (mut rig, ids) = Rig::real("module.label", &[egui::pos2(100.0, 300.0)]);
    for _ in 0..8 {
        rig.idle(1);
    }
    let open = rig.frame_rect(ids[0]);
    assert_eq!(
        (open.width(), open.height()),
        (3.0 * MAJOR, 2.0 * MAJOR),
        "a note opens three blocks by two: {open:?}",
    );

    // Drag the frame's own bottom-right corner: the grip is the node's, not the text
    // box's, and it moves both axes in whole blocks.
    let f = rig.frame_rect(ids[0]);
    rig.drag(f.max - egui::vec2(3.0, 3.0), egui::vec2(60.0, 60.0));
    rig.idle(4);
    let n = rig.canvas.snarl.get_node(ids[0]).unwrap();
    let wide = n.params.get("ui_w").and_then(|v| v.as_f64());
    let tall = n.params.get("ui_h").and_then(|v| v.as_f64());
    assert!(wide.is_some_and(|w| (w as f32) % B == 0.0), "the grip left width off the grid: {wide:?}");
    assert!(tall.is_some_and(|h| (h as f32) % B == 0.0), "the grip left height off the grid: {tall:?}");
    let grown = rig.frame_rect(ids[0]);
    assert_eq!(
        (grown.width(), grown.height()),
        (wide.unwrap() as f32, tall.unwrap() as f32),
        "the frame didn't take the size the grip asked for: {grown:?} from {open:?}",
    );

    // And it comes back down again, to the size it opened at. This used to ratchet: the
    // header, holding its controls against its right edge, drew across the whole frame,
    // and the node's width formula took that as the header's claim - so a note could be
    // dragged wider and never narrower.
    let g = rig.frame_rect(ids[0]);
    rig.drag(g.max - egui::vec2(3.0, 3.0), egui::vec2(-400.0, -200.0));
    rig.idle(4);
    let back = rig.frame_rect(ids[0]);
    // Not ratcheted: it came back to no bigger than it opened. At a 28-point step it can
    // land *smaller* than the opening size - 140 rather than 168 - because its content floor
    // sits between the two and the grid can now express it.
    assert!(
        back.width() <= open.width() && back.height() <= open.height(),
        "the note didn't shrink back from what it opened at: {open:?} -> {grown:?} -> {back:?}",
    );
    assert!(
        whole_blocks(back.width()) && whole_blocks(back.height()),
        "the shrunk note left the grid: {back:?}",
    );
}

#[test]
fn a_text_field_covers_the_body_it_is_given() {
    // The field fills the body: no padding that changes with how much text happens to be
    // inside, and no inner border a few points inside the module's own frame.
    let (mut rig, ids) = Rig::real("module.label", &[egui::pos2(100.0, 300.0)]);
    for (text, w, h) in [
        ("one word", 224.0f32, 168.0),
        ("a great deal more writing than fits in the space at all, so it wraps and wraps", 336.0, 224.0),
    ] {
        if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
            n.params.insert("text".into(), serde_json::json!(text));
            n.params.insert("ui_w".into(), serde_json::json!(w));
            n.params.insert("ui_h".into(), serde_json::json!(h));
        }
        rig.idle(6);
        let frame = rig.frame_rect(ids[0]);
        let field = rig
            .ctx
            .data(|d| d.get_temp::<Xysr>(egui::Id::new(("label_field_rect", ids[0].0))))
            .unwrap_or_else(|| panic!("the note's field wasn't drawn at {frame:?}"));
        let inner = frame.shrink(6.0);
        // The field spans the body's width and reaches the bottom of the frame: the
        // header takes the top, and nothing else takes anything.
        assert!(
            (field.0[2] - inner.width()).abs() < 2.0,
            "the field is {:.1} wide in a {inner:?} body: {field:?}",
            field.0[2],
        );
        assert!(
            (field.0[3] - (inner.height() - 26.0)).abs() < 8.0,
            "the field is {:.1} tall in a {inner:?} body under a 26pt header: {field:?}",
            field.0[3],
        );
    }
}

#[test]
fn a_readout_is_two_blocks_and_its_type_fills_the_space() {
    // Two blocks to start with - the old three were wider than any ordinary reading -
    // and the grip works both ways, including back down to the floor.
    let (mut rig, ids) = Rig::real("display.readout", &[egui::pos2(100.0, 300.0)]);
    rig.idle(6);
    let open = rig.frame_rect(ids[0]);
    assert_eq!(
        (open.width(), open.height()),
        (2.0 * MAJOR, 2.0 * MAJOR),
        "a readout with no value on it: {open:?}",
    );
    if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
        n.extra.last_signals = vec![Some(flexinput_core::Signal::Float(-123456.0))];
    }
    rig.idle(4);
    assert_eq!(
        rig.frame_rect(ids[0]).size(),
        open.size(),
        "a six-figure value should be written smaller, not wider",
    );
    // And no reading, however odd, resizes it: the type gives way, the module doesn't.
    for v in [0.0f32, 1.0, -9999.4, 1.0e30, -1.0e-30] {
        if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
            n.extra.last_signals = vec![Some(flexinput_core::Signal::Float(v))];
        }
        rig.idle(3);
        assert_eq!(
            rig.frame_rect(ids[0]).size(),
            open.size(),
            "the reading {v} resized the module to {:?}",
            rig.frame_rect(ids[0]),
        );
    }
    let f = rig.frame_rect(ids[0]);
    rig.drag(f.max - egui::vec2(3.0, 3.0), egui::vec2(120.0, 60.0));
    rig.idle(4);
    let grown = rig.frame_rect(ids[0]);
    assert!(
        grown.width() > open.width() && grown.height() > open.height(),
        "the grip didn't grow the readout: {open:?} -> {grown:?}",
    );
    rig.drag(grown.max - egui::vec2(3.0, 3.0), egui::vec2(-400.0, -200.0));
    rig.idle(4);
    assert_eq!(
        rig.frame_rect(ids[0]).size(),
        open.size(),
        "the readout didn't come back to its floor: {grown:?} -> {:?}",
        rig.frame_rect(ids[0]),
    );
}

#[test]
fn a_text_note_grows_itself_when_the_text_outgrows_the_frame() {
    let (mut rig, ids) = Rig::real("module.label", &[egui::pos2(100.0, 300.0)]);
    rig.idle(4);
    if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
        n.params.insert("ui_h".into(), serde_json::json!(112.0));
    }
    rig.idle(4);
    let set = rig.frame_rect(ids[0]);
    assert_eq!(set.height(), 2.0 * MAJOR, "the requested height wasn't taken: {set:?}");

    // Twelve lines won't fit two blocks. The field overruns the floor and the module
    // takes whole blocks to hold it - the grip never has to know.
    if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
        n.params.insert("text".into(), serde_json::json!("one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve"));
    }
    rig.idle(6);
    let tall = rig.frame_rect(ids[0]);
    assert!(tall.height() > 2.0 * MAJOR, "the note clipped itself instead of growing: {set:?} -> {tall:?}");
    assert_eq!(
        (tall.height() % B, tall.width() % B),
        (0.0, 0.0),
        "the note grew off the grid: {tall:?}",
    );

    // Back to one line and the floor holds it: it shrinks to what was asked for, not to
    // the tallest it ever was.
    if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
        n.params.insert("text".into(), serde_json::json!("one"));
    }
    rig.idle(6);
    let back = rig.frame_rect(ids[0]);
    assert_eq!(back.height(), 2.0 * MAJOR, "the note kept the height its longest text needed: {tall:?} -> {back:?}");
}

#[test]
fn a_quantize_is_two_blocks_and_its_pills_answer() {
    // It used to be four blocks wide: a factor drag box and a rounding choice spelling
    // out four words, each on its own row. The mark and both settings are painted into the
    // frame now, so the module's size is its pins' business and nothing else.
    let (mut rig, ids) = Rig::real("math.quantize", &[egui::pos2(100.0, 300.0)]);
    for _ in 0..8 {
        rig.idle(1);
    }
    let f = rig.frame_rect(ids[0]);
    assert_eq!(
        (f.width(), f.height()),
        (2.0 * MAJOR, 2.0 * MAJOR),
        "Quantize should be two blocks square: {f:?}",
    );
    let side = rig
        .ctx
        .data(|d| d.get_temp::<i32>(egui::Id::new(("module_glyph_side", ids[0].0))));
    assert!(
        side.is_some_and(|s| s >= 16),
        "the staircase wasn't painted ({side:?}) under a {f:?} frame",
    );
    let par = |rig: &Rig| {
        let n = rig.canvas.snarl.get_node(ids[0]).unwrap();
        (
            n.params.get("factor").and_then(|v| v.as_f64()),
            n.params
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("round")
                .to_string(),
        )
    };
    assert_eq!(par(&rig), (None, "round".to_string()), "both settings start at their defaults");

    // The factor pill is the left one, at the bottom of the frame; its value cell - the
    // half that answers to a drag - sits just left of the module's centre. Dragging it
    // right steps up through the doublings.
    let left = egui::pos2(f.left() + 42.0, f.bottom() - 12.0);
    rig.drag(left, egui::vec2(6.0, 0.0));
    rig.idle(2);
    let factor = par(&rig).0.unwrap_or(1.0) as f32;
    let steps: &[f32] = quantize_steps();
    assert!(
        steps.iter().any(|s| (s - factor).abs() < 0.001),
        "the drag landed on {factor}, which isn't one of the steps the pill snaps through",
    );
    assert_ne!(factor, 1.0, "dragging the factor pill right didn't move it");

    // The mode pill opens a list rather than being dragged through the words: one click
    // opens it under the pill, the next picks an entry. The body leaves the list's rect
    // behind, so this clicks it rather than guessing where it went.
    let menu_x = f.center().x + 12.0;
    rig.click(egui::pos2(menu_x, f.bottom() - 12.0));
    rig.idle(2);
    let list = rig
        .ctx
        .data(|d| d.get_temp::<Xysr>(egui::Id::new(("quantize_menu_rect", ids[0].0))))
        .unwrap_or_else(|| panic!("the rounding list didn't open for {f:?}"));
    let [lx, ly, lw, lh] = list.0;
    assert!(lw > 40.0 && lh > 60.0, "the rounding list is the wrong shape: {list:?}");
    let row_h = (lh - 4.0) / 4.0;
    rig.click(egui::pos2(lx + lw * 0.5, ly + row_h * 1.5));
    rig.idle(2);
    assert_eq!(par(&rig).1, "floor", "the second entry of the rounding list didn't take");
    // And it closed afterwards: one click, one choice.
    let still = rig
        .ctx
        .data(|d| d.get_temp::<Xysr>(egui::Id::new(("quantize_menu_rect", ids[0].0))));
    assert!(still.is_none(), "the rounding list stayed open after choosing");

    // Neither control resized the module.
    let after = rig.frame_rect(ids[0]);
    assert_eq!(after.size(), f.size(), "the pills grew the module: {f:?} -> {after:?}");

    // And there is no corner grip: the body allocates nothing, so a drag there asks for
    // room that nothing needs.
    let g = rig.frame_rect(ids[0]);
    rig.drag(g.max - egui::vec2(3.0, 3.0), egui::vec2(120.0, 60.0));
    rig.idle(4);
    assert_eq!(
        rig.frame_rect(ids[0]).size(),
        f.size(),
        "the corner still resized Quantize: {f:?} -> {:?}",
        rig.frame_rect(ids[0]),
    );
}

#[test]
fn the_small_modules_are_two_blocks_square() {
    // Each of these was wider than its pins and settings ask for - Inverse and Logic Delay
    // five blocks, Has Changed and Vec to Deflection six, Switch and Knob one too many the
    // other way - because labelled rows and long pin names added a block on top of what the
    // module needs.
    for module in [
        "module.switch",
        "math.negate",
        "module.knob",
        "logic.delay",
        "logic.has_changed",
        "module.vec_to_deflection",
    ] {
        let (mut rig, ids) = Rig::real(module, &[egui::pos2(100.0, 300.0)]);
        for _ in 0..8 {
            rig.idle(1);
        }
        let f = rig.frame_rect(ids[0]);
        assert_eq!(
            (f.width(), f.height()),
            (2.0 * MAJOR, 2.0 * MAJOR),
            "{module} should be two blocks square: {f:?}",
        );
    }
}

#[test]
fn a_pinned_label_is_short_on_the_canvas_and_full_on_hover() {
    // The short forms are what let these modules be two blocks; the names a patch stores
    // must not move.
    use crate::canvas::viewer::kit::{pin_abbreviates, pin_short};
    for module in ["module.switch", "logic.has_changed", "module.split", "math.quantize"] {
        assert!(pin_abbreviates(module), "{module} should draw its pins short");
    }
    assert!(!pin_abbreviates("module.remapper"), "a big module keeps its labels");
    assert_eq!(pin_short("direct"), ("dir".to_string(), true));
    assert_eq!(pin_short("increased"), ("inc".to_string(), true));
    assert_eq!(pin_short("out_0"), ("0".to_string(), true));
    assert_eq!(pin_short("in_11"), ("11".to_string(), true));
    assert_eq!(pin_short("in"), ("in".to_string(), false), "short names stay as they are");
    let (long, shortened) = pin_short("Orientation");
    assert!(shortened && long.chars().count() <= 4, "an unknown long name should lose its tail, got {long:?}");
    // The id is never what changes: `pin_short` returns a display string only.
    assert_eq!(pin_short("out_0").0, "0", "the column already says out");
}

#[test]
fn a_constant_keeps_its_digits_and_still_answers_the_drag() {
    let (mut rig, ids) = Rig::real("module.constant", &[egui::pos2(100.0, 300.0)]);
    set_constant(&mut rig, ids[0], 123456789.0);
    rig.idle(6);
    assert_eq!(constant(&rig, ids[0]), 123456789.0, "the digits were rounded on the way in");

    // Drag the box sideways. A nine-digit number is larger than the gap between
    // neighbouring 32-bit floats, so with the value read through f32 and a fixed small
    // step, every drag computed a change, stored nothing, and the box looked dead.
    let f = rig.frame_rect(ids[0]);
    let before = constant(&rig, ids[0]);
    rig.drag(f.center(), egui::vec2(40.0, 0.0));
    let after = constant(&rig, ids[0]);
    assert!(after > before, "dragging a large constant changed nothing: {before} -> {after}");

    // Past what the wire could ever carry, it is clamped to the stated limit rather than
    // left to turn into nothing downstream.
    set_constant(&mut rig, ids[0], 1.0e20);
    rig.idle(6);
    assert_eq!(constant(&rig, ids[0]), 1.0e12, "a huge value wasn't clamped");
    let wide = rig.frame_rect(ids[0]);
    assert!(wide.width() <= 4.0 * MAJOR, "even clamped, the number took {wide:?}");
}

#[test]
fn a_constant_fits_two_blocks_and_extends_for_a_longer_number() {
    // No grip: the box is as wide as the digits it holds. `9999.99` is the number it
    // keeps inside two blocks; anything longer takes the module a block wider, and
    // putting the short value back hands that block back.
    let (mut rig, ids) = Rig::real("module.constant", &[egui::pos2(100.0, 300.0)]);
    set_constant(&mut rig, ids[0], 9999.99);
    rig.idle(6);
    let fits = rig.frame_rect(ids[0]);
    assert_eq!(fits.width(), 2.0 * MAJOR, "9999.99 should stay two blocks wide: {fits:?}");
    assert_eq!(fits.height(), 2.0 * MAJOR, "at rest: {fits:?}");

    set_constant(&mut rig, ids[0], 123456789.0);
    rig.idle(6);
    let longer = rig.frame_rect(ids[0]);
    assert_eq!(
        longer.width(),
        fits.width() + B,
        "nine digits should take exactly one more block - half an old one now the block is \
         28: {fits:?} -> {longer:?}",
    );
    assert_eq!(
        longer.height(),
        fits.height(),
        "a longer number changed the height, not the width: {longer:?}"
    );

    set_constant(&mut rig, ids[0], 9999.99);
    rig.idle(6);
    let back = rig.frame_rect(ids[0]);
    assert_eq!(back.width(), fits.width(), "the width wasn't handed back: {back:?}");
}

#[test]
fn the_or_equal_toggle_is_centred_on_the_module() {
    // The body of a comparison sits in the column between its pin labels, and that
    // column is not the middle of the module: `a`/`b` on one side, the wider `out` on
    // the other. The toggle has to answer to the module's centre, not to the column.
    let (mut rig, ids) = Rig::real("logic.greater_than", &[egui::pos2(100.0, 300.0)]);
    let frame = rig.frame_rect(ids[0]);
    let node = ids[0];
    let or_equal = |rig: &Rig| {
        rig.canvas
            .snarl
            .get_node(node)
            .and_then(|n| n.params.get("or_equal").and_then(|v| v.as_bool()))
            .unwrap_or(false)
    };

    // Find the row the toggle is on: the first point down the middle that changes the
    // setting when clicked.
    let mut row: Option<f32> = None;
    let mut y = frame.top() + 16.0;
    while y < frame.bottom() - 6.0 && row.is_none() {
        let before = or_equal(&rig);
        rig.click(egui::pos2(frame.center().x, y));
        if or_equal(&rig) != before {
            row = Some(y);
        }
        y += 6.0;
    }
    let row = row.expect("clicking down the module's middle reaches the toggle");

    // Sweep that row and take the range that answers. Its middle is where the control
    // actually sits.
    let (mut first, mut last) = (None, frame.left());
    for i in 0..=44 {
        let x = frame.left() + frame.width() * (i as f32 / 44.0);
        let before = or_equal(&rig);
        rig.click(egui::pos2(x, row));
        if or_equal(&rig) != before {
            first = Some(first.unwrap_or(x).min(x));
            last = x;
        }
    }
    let (first, last) = (first.expect("the toggle answers somewhere on its row"), last);
    let middle = (first + last) * 0.5;
    assert!(
        (middle - frame.center().x).abs() <= 4.0,
        "the toggle answers from {first:.1} to {last:.1}, centred at {middle:.1} against the module's {:.1}",
        frame.center().x,
    );
    // And it's a real hit area, not a dot: the row it sits on is inside the module.
    assert!(last - first >= 40.0, "the toggle answers over only {:.1} points", last - first);
}

#[test]
fn a_body_centres_its_controls_on_the_module_and_the_column() {
    // The house rule: horizontally on the module - the body column is fenced in by pin
    // columns of different widths, so its middle isn't the module's - and vertically in
    // what's under the header, because centring on the module parks a stack high with all
    // the air below it. The bodies trace what they place, so this measures the rule rather
    // than the module's size.
    for (module, ctrl) in [
        ("math.negate", "inverse_group"),
        ("logic.delay", "delay_group"),
        ("logic.has_changed", "hysteresis_pill"),
        ("module.switch", "switch_button"),
    ] {
        let (mut rig, ids) = Rig::real(module, &[egui::pos2(100.0, 300.0)]);
        rig.idle(4);
        let f = rig.frame_rect(ids[0]);
        let group = rig.traced(ids[0], ctrl);
        let column = rig.traced(ids[0], "body_column");
        assert!(
            (group.center().x - f.center().x).abs() <= 1.0,
            "{module}: {ctrl} centred at x {:.1}, the module's is {:.1} ({f:?})",
            group.center().x,
            f.center().x,
        );
        assert!(
            (group.center().y - column.center().y).abs() <= 1.0,
            "{module}: {ctrl} centred at y {:.1}, the body column's is {:.1} ({f:?})",
            group.center().y,
            column.center().y,
        );
        assert!(
            f.contains_rect(group.expand(-1.0)),
            "{module}: {ctrl} painted outside the frame {f:?}",
        );
    }
}

#[test]
fn the_painted_pills_stay_inside_the_frame() {
    // The two modules that paint their settings across the frame rather than allocating
    // them: a pill a point wider than the frame reads as a pill with its value cut off.
    for (module, pill) in [
        ("math.quantize", "quantize_factor_pill"),
        ("module.vec_to_deflection", "unit_pill"),
    ] {
        let (mut rig, ids) = Rig::real(module, &[egui::pos2(100.0, 300.0)]);
        rig.idle(4);
        let f = rig.frame_rect(ids[0]);
        let p = rig.traced(ids[0], pill);
        assert!(
            p.left() >= f.left() + 4.0 && p.right() <= f.right() - 4.0,
            "{module}: the {pill} runs past the frame: {p:?} in {f:?}",
        );
    }
}

#[test]
fn a_pill_value_can_be_typed_as_well_as_dragged() {
    // A pill answers a drag, but a drag can't land on an exact number - which is what made
    // the Logic Delay's time feel read-only once the box stopped being a DragValue. A
    // double-click opens a field in the value cell itself.
    let (mut rig, ids) = Rig::real("logic.delay", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    let time = |rig: &Rig| {
        rig.canvas
            .snarl
            .get_node(node)
            .and_then(|n| n.params.get("time").and_then(|v| v.as_f64()))
            .unwrap_or(100.0)
    };
    let before = time(&rig);
    let pill = rig.traced(node, "delay_time_pill");
    rig.double_click(pill.center());
    rig.type_text("250");
    assert_eq!(time(&rig), 250.0, "typing 250 into the time pill didn't keep it (was {before})");

    // The cell still scrubs, so the two ways in don't crowd each other out.
    rig.drag(pill.center(), egui::vec2(20.0, 0.0));
    assert_ne!(time(&rig), 250.0, "dragging the time pill stopped answering");
    let f = rig.frame_rect(node);
    assert_eq!(
        (f.width(), f.height()),
        (2.0 * MAJOR, 2.0 * MAJOR),
        "the pill's edit resized the module: {f:?}",
    );
}

#[test]
fn a_quantize_factor_can_be_typed_exactly() {
    // Dragging the factor pill steps through doublings; typing puts the number you meant,
    // which is the only way to get 7 subdivisions.
    let (mut rig, ids) = Rig::real("math.quantize", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    let before = rig.frame_rect(node);
    let factor = |rig: &Rig| {
        rig.canvas
            .snarl
            .get_node(node)
            .and_then(|n| n.params.get("factor").and_then(|v| v.as_f64()))
    };
    let pill = rig.traced(node, "quantize_factor_pill");
    rig.double_click(pill.center());
    rig.type_text("7");
    assert_eq!(factor(&rig), Some(7.0), "the typed factor wasn't kept as typed");
    assert_eq!(
        rig.frame_rect(node).size(),
        before.size(),
        "typing a factor resized the module: {before:?} -> {:?}",
        rig.frame_rect(node),
    );
}

#[test]
fn a_small_module_holds_its_two_blocks_without_the_grid() {
    // The grid rounds a frame up to whole blocks, which is what hid the fact that these
    // bodies ask for less than two blocks of their own content. With the sizing off, the
    // content is all there is - and a module whose pills are painted rather than allocated
    // used to fall to one block wide, too narrow to paint them in, which kept them from
    // ever being painted.
    for module in ["module.vec_to_deflection", "math.quantize", "math.abs", "module.axis_to_vec"] {
        let (mut rig, ids) = Rig::real_no_snap(module, &[egui::pos2(100.0, 300.0)]);
        rig.idle(4);
        let f = rig.frame_rect(ids[0]);
        assert!(
            f.width() > B + 8.0,
            "{module}: only {:.0} wide with the grid's sizing off - its own content should ask for two blocks: {f:?}",
            f.width(),
        );
        let side = rig
            .ctx
            .data(|d| d.get_temp::<i32>(egui::Id::new(("module_glyph_side", ids[0].0))));
        assert!(
            side.is_some_and(|s| s >= 16),
            "{module}: no mark painted ({side:?}) in {f:?}",
        );
    }
}

#[test]
fn a_knob_fills_the_frame_its_grip_gives_it() {
    let (mut rig, ids) = Rig::real("module.knob", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    for _ in 0..6 {
        rig.idle(1);
    }
    let square = rig.frame_rect(node);
    assert_eq!(
        (square.width(), square.height()),
        (2.0 * MAJOR, 2.0 * MAJOR),
        "a knob left alone: {square:?}"
    );
    // The dial takes the body it's given - it doesn't give up a line for the reading, which
    // sits in its middle instead, clear of the indicator.
    let face = rig.traced(node, "knob_face");
    let column = rig.traced(node, "body_column");
    assert!((face.width() - face.height()).abs() <= 1.0, "a knob this size isn't a round dial: {face:?}");
    assert!(
        face.height() >= column.height() - 16.0,
        "the dial {face:?} leaves more than its air in {column:?}",
    );
    // Centred on the module.
    assert!((face.center().x - square.center().x).abs() <= 1.0, "the face {face:?} is off the module's middle line: {square:?}");
    let number = rig.traced(node, "knob_number");
    assert!(
        face.contains(number.center()) && (number.center() - face.center()).length() <= 2.0,
        "the reading {number:?} isn't in the middle of the dial {face:?}",
    );

    // Wide: the dial becomes a horizontal fader, and drags left-to-right.
    rig.drag(square.max - egui::vec2(3.0, 3.0), egui::vec2(2.0 * MAJOR, 0.0));
    let wide = rig.frame_rect(node);
    let wide_face = rig.traced(node, "knob_face");
    assert!(wide.width() >= 4.0 * MAJOR - 1.0, "the grip didn't widen the knob: {square:?} -> {wide:?}");
    assert!(
        wide_face.width() / wide_face.height() >= 2.0,
        "a {wide:?} knob isn't a horizontal strip: {wide_face:?}",
    );
    // With a block and more of height, the reading still goes under, not to the side.
    let wide_number = rig.traced(node, "knob_number");
    assert!(
        wide_number.top() >= wide_face.bottom() - 1.0,
        "a wide knob with room under its track put the reading elsewhere: {wide_number:?} vs {wide_face:?}",
    );

    // And tall: a vertical one.
    rig.drag(wide.max - egui::vec2(3.0, 3.0), egui::vec2(-(B * 2.0), B));
    let tall = rig.frame_rect(node);
    let tall_face = rig.traced(node, "knob_face");
    assert!(tall.height() > wide.height(), "the grip didn't take the knob taller: {wide:?} -> {tall:?}");
    assert!(
        tall_face.height() >= tall_face.width(),
        "a {tall:?} knob didn't turn for the taller frame: {tall_face:?}",
    );

    // Slim: a fader is a strip of track, so the grip can take a knob down to one old block
    // that way - 56 points, which is the knob's own floor now that the grid's block is 28:
    // one grid block of track would be too thin to grab. A dial won't, and a module one
    // block *tall* has no room for a control once its header and margins are counted - so
    // it's the width that goes slim.
    let before_slim = rig.frame_rect(node);
    rig.drag(before_slim.max - egui::vec2(3.0, 3.0), egui::vec2(-3.0 * MAJOR, 0.0));
    let slim = rig.frame_rect(node);
    assert_eq!(slim.width(), MAJOR, "the grip couldn't take a knob to its thinnest: {before_slim:?} -> {slim:?}");
    let slim_face = rig.traced(node, "knob_face");
    assert!(
        slim_face.height() > slim_face.width(),
        "a {slim:?} knob isn't a vertical strip: {slim_face:?}",
    );

    // Wide and one old block tall: a slider strip - 56 points, which is what its header and
    // margins leave rather than the grid's step. There's nothing under it, so the reading
    // goes at the end of the track instead.
    let before_flat = rig.frame_rect(node);
    rig.drag(before_flat.max - egui::vec2(3.0, 3.0), egui::vec2(2.0 * MAJOR, -2.0 * MAJOR));
    let flat = rig.frame_rect(node);
    assert_eq!(flat.height(), MAJOR, "the grip couldn't take a knob to its flattest: {before_flat:?} -> {flat:?}");
    let flat_face = rig.traced(node, "knob_face");
    let flat_number = rig.traced(node, "knob_number");
    assert!(
        flat_face.width() > flat_face.height() * 2.0,
        "a {flat:?} knob isn't a horizontal strip: {flat_face:?}",
    );
    assert!(
        flat_number.right() >= flat_face.right() - 2.0 && flat_number.left() < flat_face.right() - 4.0,
        "a one-block-tall fader's reading isn't at the end of its track: {flat_number:?} vs {flat_face:?}",
    );
}

#[test]
fn a_switch_grows_for_the_label_it_has_to_show() {
    // A Switch's caption is set from its context menu, and can be any length; the button
    // and the module make room for it rather than shrinking the text into unreadability.
    // The size comes from the stored caption, so it takes the extra block and hands it back.
    let (mut rig, ids) = Rig::real("module.switch", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(4);
    let plain = rig.frame_rect(node);
    assert_eq!((plain.width(), plain.height()), (2.0 * MAJOR, 2.0 * MAJOR), "a switch as it starts: {plain:?}");
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.params.insert("caption_off".into(), serde_json::json!("Afterburner engaged"));
    }
    rig.idle(6);
    let captioned = rig.frame_rect(node);
    assert!(
        captioned.width() > plain.width(),
        "a long caption didn't take the switch wider: {plain:?} -> {captioned:?}",
    );
    let button = rig.traced(node, "switch_button");
    assert!(
        button.right() <= captioned.right() && button.left() >= captioned.left(),
        "the button {button:?} doesn't fit inside its frame {captioned:?}",
    );
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.params.insert("caption_off".into(), serde_json::json!(""));
    }
    rig.idle(6);
    let back = rig.frame_rect(node);
    assert!(
        back.width() < captioned.width(),
        "the width didn't come back with the caption: {captioned:?} -> {back:?}",
    );
}

#[test]
fn the_combiners_port_rows_are_one_width() {
    // The Combiner's rows carry a port name, a device label and two dropdowns. With
    // different labels they came out at different widths and the list read as several
    // controls; every row is now as wide as the widest content in the list.
    let (mut rig, ids) = Rig::real("module.automap_combiner", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.inputs.push(PinDescriptor::new("in_2", SignalType::AutoMap));
    }
    rig.idle(6);
    let (a, b) = (rig.traced(node, "combiner_row_0"), rig.traced(node, "combiner_row_2"));
    assert!(a.width() > 100.0, "no Combiner rows were traced: {a:?} {b:?}");
    assert!((a.width() - b.width()).abs() <= 1.0, "the Combiner's rows are different widths: {a:?} {b:?}");
    let f = rig.frame_rect(node);
    assert!(f.contains_rect(a) && f.contains_rect(b), "a row runs past its frame: {a:?} {b:?} in {f:?}");
}

#[test]
fn a_collapsed_knob_shows_its_number_and_nothing_else() {
    // Collapsing a knob is how the user keeps it from being nudged while moving things
    // about it - so what's left has to still tell them what it's set to.
    let (mut rig, ids) = Rig::real("module.knob", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(4);
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.params.insert("value".into(), serde_json::json!(0.75));
    }
    rig.canvas.snarl.open_node(node, false);
    rig.idle(6);
    // Nothing of the face is drawn. Snarl runs the body of a collapsed node anyway and
    // shifts it up under the header's clip, which is what painted a slice of the dial's
    // rim across the header row.
    let face = rig.traced(node, "knob_face");
    assert!(!face.is_positive(), "a collapsed knob still painted its face: {face:?}");
    // The number is written in the band under the header, which a folded node still has -
    // the frame keeps its blocks.
    let number = rig.traced(node, "knob_number");
    let frame = rig.frame_rect(node);
    assert!(number.width() > 8.0, "no reading was drawn on the collapsed knob: {number:?}");
    assert!(frame.contains_rect(number), "the collapsed reading {number:?} isn't inside {frame:?}");
    assert!(
        number.center().y > frame.top() + 30.0,
        "the collapsed reading {number:?} sits in the header row of {frame:?}",
    );
    // And it comes back when the knob is opened again.
    rig.canvas.snarl.open_node(node, true);
    rig.idle(6);
    assert!(rig.traced(node, "knob_face").is_positive(), "the face didn't come back when the knob was opened");
}

#[test]
fn a_title_is_left_out_when_it_doesnt_fit_and_comes_back_when_it_does() {
    let (mut rig, ids) = Rig::real("module.knob", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.display_name = "Blend amount for the slow filter".to_string();
    }
    rig.idle(6);
    let at = rig.frame_rect(node);
    assert_eq!((at.width(), at.height()), (2.0 * MAJOR, 2.0 * MAJOR), "a renamed knob: {at:?}");
    // The name doesn't fit beside the Uni/Bi switch in a header this narrow, so it isn't
    // drawn - and it isn't what kept the module wide, either.
    let title = rig.traced(node, "header_title");
    assert!(
        !title.is_positive() || title.width() < 8.0,
        "a name too long for {at:?} was still drawn at {title:?}",
    );
    // The name isn't what's holding the frame open either: dragged inward, the knob goes
    // all the way down to its own thinnest - 56 points, its floor rather than the grid's,
    // which is half that now - name or no name.
    let at_now = rig.frame_rect(node);
    rig.drag(at_now.max - egui::vec2(3.0, 3.0), egui::vec2(-3.0 * MAJOR, 0.0));
    let narrow = rig.frame_rect(node);
    assert_eq!(narrow.width(), MAJOR, "the name kept the knob wider than its content needs: {at_now:?} -> {narrow:?}");
    // Widen the frame and the name comes back.
    rig.canvas
        .snarl
        .get_node_mut(node)
        .unwrap()
        .params
        .insert(kit_width_param().into(), serde_json::json!(5.0 * MAJOR));
    rig.idle(6);
    let wide = rig.frame_rect(node);
    let shown = rig.traced(node, "header_title");
    assert!(
        wide.width() >= 5.0 * B - 1.0 && shown.width() > 40.0,
        "the name didn't come back at {wide:?}: {shown:?}",
    );
}

#[test]
fn a_port_list_lands_on_the_block_grid() {
    // A strip row is 28 points - a 25-point strip and the pin column's 3-point spacing -
    // which at this grid is exactly one block. So the empty band under the last strip is the
    // same size whatever the port count: measured, 26 points from three ports to seven. At
    // the old 56-point block the same list alternated between 26 and 55 depending on whether
    // the number of rows was odd, and a 27-point row slid two points further off the block
    // with every pair of ports.
    let (mut rig, ids) = Rig::real("module.selector", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    let mut left = std::collections::BTreeMap::new();
    for ports in 3..=7usize {
        while rig.inputs(node) < ports + 1 {
            rig.set_inputs(node, rig.inputs(node) + 1);
        }
        rig.idle(5);
        let f = rig.frame_rect(node);
        let (a, b) = (rig.traced(node, "strip_0"), rig.traced(node, "strip_1"));
        let last = rig.traced(node, &format!("strip_{}", ports - 1));
        let pitch = b.top() - a.top();
        assert!(
            (pitch - B).abs() < 0.5,
            "{ports} ports: a row of {pitch:.1} isn't one block ({B})",
        );
        assert!(
            (f.height().rem_euclid(pitch)).abs() < 1.5,
            "{ports} ports: a row of {pitch:.1} doesn't divide the frame's {:.1}",
            f.height(),
        );
        left.insert(ports, f.bottom() - last.bottom());
    }
    let vals: Vec<f32> = left.values().copied().collect();
    let spread = vals.iter().fold(0.0f32, |m, v| m.max((v - vals[0]).abs()));
    assert!(
        spread < 1.5,
        "the space under the last strip changes with the port count: {left:?}",
    );
}

#[test]
fn the_port_the_selection_is_on_glows() {
    // A Selector or Split lights the strip its selection is pointing at, so the module says
    // what it is doing without being opened. In interpolate mode the light splits between
    // the two ports the value sits between.
    let (mut rig, ids) = Rig::real("module.selector", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.set_inputs(node, 4);
    rig.idle(4);
    let n = (0..).take(12).filter(|i| rig.traced_rect(node, &format!("strip_{i}")).is_some()).count();
    assert!(n >= 3, "the Selector's strips weren't drawn: {n}");
    rig.canvas
        .snarl
        .get_node_mut(node)
        .unwrap()
        .extra
        .last_signals
        .push(Some(flexinput_core::Signal::Float(0.5)));
    rig.idle(4);
    let sel = (((n - 1) as f32 * 0.5) + 0.5) as usize;
    let lit = rig.traced(node, &format!("strip_glow_{sel}"));
    assert!(lit.is_positive(), "nothing glows on port {sel} of a Selector pointed at 0.5");
    assert_eq!(
        lit,
        rig.traced_rect(node, &format!("strip_{sel}")).map(|r| r.expand(1.0)).unwrap(),
        "the glow isn't on the strip it names",
    );
    for i in 0..n {
        if i != sel {
            assert!(
                rig.traced_rect(node, &format!("strip_glow_{i}")).is_none(),
                "port {i} glows as well on a Selector that isn't interpolating",
            );
        }
    }
    // Interpolate on, the value a quarter of the way: the first two ports share the light.
    if let Some(nd) = rig.canvas.snarl.get_node_mut(node) {
        nd.params.insert("interpolate".into(), serde_json::json!(true));
        nd.extra.last_signals = vec![Some(flexinput_core::Signal::Float(0.25))];
    }
    rig.idle(4);
    let (a, b) = (rig.traced(node, "strip_glow_0"), rig.traced(node, "strip_glow_1"));
    assert!(a.is_positive() && b.is_positive(), "a quarter didn't light both ports: {a:?} {b:?}");
    assert!(
        rig.traced_rect(node, &format!("strip_glow_{}", n - 1)).is_none() || n <= 2,
        "the far port glows too on a value of a quarter",
    );
}

#[test]
fn a_folded_knob_keeps_its_name_clear_of_the_socket() {
    // A collapsed node draws its sockets on the header row, so a name has to stop short of
    // them - and in a module one block wide there's nothing left to draw at all.
    let (mut rig, ids) = Rig::real("module.knob", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.display_name = "test".to_string();
        n.params.insert(kit_width_param().into(), serde_json::json!(1.0 * B));
    }
    rig.idle(4);
    rig.canvas.snarl.open_node(node, false);
    rig.idle(6);
    let f = rig.frame_rect(node);
    let title = rig.traced(node, "header_title");
    let number = rig.traced(node, "knob_number");
    assert!(f.width() <= 1.5 * MAJOR, "a one-block folded knob came out as {f:?}");
    assert!(
        !title.is_positive() || title.right() <= f.right() - 26.0,
        "the folded knob's name {title:?} is drawn under its output socket: {f:?}",
    );
    assert!(
        number.is_positive() && f.contains_rect(number),
        "the folded knob lost its reading: {number:?} in {f:?}",
    );
}

#[test]
fn a_title_does_not_hold_a_module_wide() {
    // A module's width comes from its pins and its controls. The name in its header is
    // shown while it fits, and left out rather than cropped when it doesn't - so a long
    // name, even one the user typed themselves, can't be what decides how wide the module
    // has to be.
    let (mut rig, ids) = Rig::real("logic.delay", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.display_name = "Wait until the button has settled before letting go".to_string();
    }
    rig.idle(6);
    let f = rig.frame_rect(node);
    assert_eq!(
        (f.width(), f.height()),
        (2.0 * MAJOR, 2.0 * MAJOR),
        "a renamed Logic Delay should still be 112 square, not as wide as its name: {f:?}",
    );
    // The body's controls still fit inside it, which is what the size is actually for.
    let group = rig.traced(node, "delay_group");
    assert!(f.contains_rect(group.expand(-1.0)), "the controls {group:?} don't fit {f:?}");
    // A module the user *can* widen - one with a corner grip - still takes the width it's
    // asked for, name or no name.
    let (mut rig, ids) = Rig::real("module.delay", &[egui::pos2(100.0, 300.0)]);
    if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
        n.display_name = "Wait until the signal has settled before letting go".to_string();
    }
    rig.idle(6);
    let at = rig.frame_rect(ids[0]);
    rig.canvas
        .snarl
        .get_node_mut(ids[0])
        .unwrap()
        .params
        .insert(kit_width_param().into(), serde_json::json!(at.width() + 2.0 * MAJOR));
    rig.idle(6);
    let wider = rig.frame_rect(ids[0]);
    assert!(
        wider.width() >= at.width() + 2.0 * MAJOR - 1.0,
        "the grip's width was refused: {at:?} -> {wider:?}",
    );
}

#[test]
fn the_port_strips_add_remove_rename_and_reorder_their_pins() {
    // Selector and Split: `+` in the header adds a port, the `Г—` on a strip drops one, a
    // double-click renames the port (which is the pin's own name), and the handle moves it
    // - taking its wire with it, because the engine reads these by position.
    // - no snap: the last part of this test is that the room a port needed comes back when
    //   the port goes, and at a 28-point strip pitch both the four-port and the three-port
    //   module round to three blocks with the grid on.
    let (mut rig, ids) = Rig::real_no_snap("module.selector", &[egui::pos2(100.0, 300.0), egui::pos2(400.0, 300.0)]);
    let (sel, other) = (ids[0], ids[1]);
    rig.idle(4);
    let names = |rig: &Rig| -> Vec<String> {
        rig.canvas
            .snarl
            .get_node(sel)
            .map(|n| n.inputs.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default()
    };
    assert_eq!(names(&rig), ["select", "in_0", "in_1"], "the ports it ships with");

    // The header's `+`, at the right edge of the header row.
    let f = rig.frame_rect(sel);
    let h = rig.ctx.style().spacing.interact_size.y;
    rig.click(egui::pos2(f.right() - 6.0 - h * 0.5, f.top() + 4.0 + h * 0.5));
    assert_eq!(names(&rig).len(), 4, "the header's + didn't add a port");
    assert_eq!(names(&rig)[3], "in_2", "the new port isn't named like its neighbours");

    // A wire on the first port, to see whether a move carries it.
    let src = egui_snarl::OutPinId { node: other, output: 0 };
    rig.canvas.snarl.connect(src, egui_snarl::InPinId { node: sel, input: 1 });
    rig.idle(2);

    // Drag the first strip's handle down one row: the port, and the wire on it, move.
    let strip0 = rig.traced(sel, "strip_0");
    let strip_next = rig.traced(sel, "strip_1");
    let handle = egui::pos2(strip0.left() + 8.0, strip0.center().y);
    rig.drag(handle, egui::vec2(0.0, strip_next.top() - strip0.top()));
    let after = names(&rig);
    assert_eq!(after, ["select", "in_1", "in_0", "in_2"], "the handle didn't move the port down the list: {after:?}");
    let wired = rig
        .canvas
        .snarl
        .wires()
        .find(|(o, _)| o.node == other)
        .map(|(_, i)| i.input);
    assert_eq!(wired, Some(2), "the wire stayed behind on the old index");

    // Rename: double-click the name, type, Enter.
    let strip0 = rig.traced(sel, "strip_0");
    rig.double_click(egui::pos2(strip0.right() - 20.0, strip0.center().y));
    rig.type_text("kick");
    assert_eq!(names(&rig)[1], "kick", "the strip's name field didn't rename the pin");

    // The `Г—` only shows once there are more than two ports to choose between; it drops
    // the port it's on and the module shrinks back with it.
    let tall = rig.frame_rect(sel);
    let strip1 = rig.traced(sel, "strip_1");
    rig.click(egui::pos2(strip1.left() + 26.0, strip1.center().y));
    assert_eq!(names(&rig).len(), 3, "the strip's Г— didn't drop the port: {:?}", names(&rig));
    rig.idle(4);
    assert!(rig.frame_rect(sel).height() < tall.height(), "the module kept the room the port needed: {tall:?} -> {:?}", rig.frame_rect(sel));
}

#[test]
fn the_strips_sit_on_the_rows_of_their_sockets() {
    // A port strip and the socket that carries it are one row: the strips are placed at the
    // same pitch the pin rows are laid out at, so a port added moves the module by exactly
    // one row on both sides. Measured without the grid's rounding, where the frame is
    // exactly what the content asks for.
    for module in ["module.selector", "module.split"] {
        let (mut rig, ids) = Rig::real_no_snap(module, &[egui::pos2(100.0, 300.0)]);
        let node = ids[0];
        let two = rig.frame_rect(node);
        let (a, b) = (rig.traced(node, "strip_0"), rig.traced(node, "strip_1"));
        let pitch = b.top() - a.top();
        assert!(pitch > 20.0 && pitch < 34.0, "{module}: strips {a:?} {b:?} aren't one row apart");
        // Every strip is flush with the others on the side its sockets are on: inputs on the
        // left for a Selector, outputs on the right for a Split.
        let flush = if module == "module.split" { a.right() - b.right() } else { a.left() - b.left() };
        assert!(flush.abs() <= 0.6, "{module}: its strips aren't flush: {a:?} {b:?}");
        // Three strips, three rows: a Selector's `select` row is drawn at the pitch too.
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            if module == "module.selector" {
                n.inputs.push(PinDescriptor::new("a", SignalType::Any));
            } else {
                n.outputs.push(PinDescriptor::new("a", SignalType::Any));
            }
        }
        rig.idle(6);
        let grown = rig.frame_rect(node);
        assert!(
            (grown.height() - two.height()).abs() - pitch < 1.0,
            "{module}: {two:?} -> {grown:?}: adding a port moved the frame by {:.1}, not the strips' {pitch:.1}",
            grown.height() - two.height(),
        );
        let c = rig.traced(node, "strip_0");
        assert!(c.top() >= two.top(), "{module}: the first strip {c:?} is above the frame {two:?}");
        // The list ends near the bottom of the body: what's left under the last strip is the
        // frame's own margin, not a row that widens every time a port is added.
        let last = rig.traced(node, "strip_2");
        let under = grown.bottom() - last.bottom();
        assert!(
            under > 4.0 && under < 30.0,
            "{module}: {under:.1} left under the last strip {last:?} in {grown:?}",
        );
        // And the strips reach the edge their sockets are on, rather than stopping where the
        // longest name happens to end.
        if module == "module.split" {
            let d = rig.traced(node, "strip_0");
            assert!(
                grown.right() - d.right() < 26.0,
                "{module}: its strips stop {:.1} short of the frame's right edge: {d:?} in {grown:?}",
                grown.right() - d.right(),
            );
        }
    }
}
