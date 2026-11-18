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
    /// What the last frame painted, so an audit can read the drawing itself rather than the
    /// rects a body chose to report about itself.
    shapes: Vec<egui::epaint::ClippedShape>,
    /// The textures handed out so far, for [`Rig::snapshot`].
    shot: super::shot::Shot,
    /// The size of the window the canvas is drawn in. The picture rig makes it larger.
    screen: egui::Vec2,
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
            shapes: Vec::new(),
            shot: Default::default(),
            screen: egui::vec2(1600.0, 1000.0),
        };
        rig.idle(6);
        (rig, ids)
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        self.time += 1.0 / 30.0;
        let mut raw = input(self.time, events);
        raw.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.screen));
        let Rig { ctx, canvas, descriptors, to_global, snarl_id, grid, shapes, shot, .. } = self;
        let (ids, signals) = (HashSet::new(), HashMap::new());
        let out = ctx.run(raw, |ctx| {
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
        shot.absorb(&out.textures_delta);
        *shapes = out.shapes;
    }

    /// Save a picture of `node` as the last frame painted it, with a margin for its wires and
    /// sockets, to `target/module_shots/<name>.png`. See [`super::shot`].
    fn snapshot(&self, name: &str, node: NodeId) -> std::path::PathBuf {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, self.screen);
        let margin = std::env::var("SHOOT_MARGIN").ok().and_then(|m| m.parse().ok()).unwrap_or(14.0);
        let area = (self.to_global * self.frame_rect(node).expand(margin)).intersect(screen);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/module_shots")
            .join(format!("{name}.png"));
        self.shot.save(&self.ctx, &self.shapes, area, 1.0, &path).expect("the picture saves");
        path
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

    /// Text painted above a frame's top edge that overlaps it sideways: where a collapsed
    /// node's body ends up, since snarl shifts the body cursor up under the header's clip.
    fn painted_above(&self, frame: egui::Rect) -> Vec<(String, egui::Rect)> {
        let back = self.to_global.inverse();
        self.shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::epaint::Shape::Text(t) => {
                    let r = back * t.galley.rect.translate(t.pos.to_vec2());
                    (r.bottom() < frame.top() + 2.0
                        && r.right() > frame.left()
                        && r.left() < frame.right()
                        && !t.galley.text().is_empty())
                        .then(|| (t.galley.text().to_string(), r))
                }
                _ => None,
            })
            .collect()
    }

    /// Where a bit of text was actually painted, in graph points, found by what it starts with
    /// (a cut name ends in an ellipsis the test shouldn't have to guess the spacing of). A
    /// body's own traces bound the cell a control was given; this is about the glyphs inside
    /// it, which is where a centred word in a left-aligned column shows up.
    fn painted_text(&self, starts_with: &str) -> Option<(String, egui::Rect)> {
        let back = self.to_global.inverse();
        self.shapes.iter().find_map(|cs| match &cs.shape {
            egui::epaint::Shape::Text(t) => {
                let says: &str = &t.galley.text()[..];
                (says.starts_with(starts_with)).then(|| (says.to_string(), back * t.galley.rect.translate(t.pos.to_vec2())))
            }
            _ => None,
        })
    }
    /// Every painted word starting with this text, in the graph's own coordinates: what a list
    /// actually drew per row, rather than what it says it drew.
    fn painted_all(&self, starts_with: &str) -> Vec<(String, egui::Rect)> {
        let back = self.to_global.inverse();
        self.shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::epaint::Shape::Text(t) => {
                    let says: &str = &t.galley.text()[..];
                    (says.starts_with(starts_with))
                        .then(|| (says.to_string(), back * t.galley.rect.translate(t.pos.to_vec2())))
                }
                _ => None,
            })
            .collect()
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

    fn outputs(&self, node: NodeId) -> usize {
        self.canvas.snarl.get_node(node).map_or(0, |n| n.outputs.len())
    }

    /// Give a node `n` outputs, named `out_0, out_1, вЂ¦` as adding one from the menu would.
    fn set_outputs(&mut self, node: NodeId, n: usize) {
        while self.outputs(node) < n {
            let name = format!("out_{}", self.outputs(node));
            if let Some(node) = self.canvas.snarl.get_node_mut(node) {
                node.outputs.push(PinDescriptor::new(name, SignalType::Any));
            }
        }
        while self.outputs(node) > n {
            let last = self.outputs(node) - 1;
            self.canvas.snarl.drop_outputs(egui_snarl::OutPinId { node, output: last });
            if let Some(node) = self.canvas.snarl.get_node_mut(node) {
                node.outputs.remove(last);
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
fn the_counter_has_no_grip_and_keeps_the_size_its_cells_ask_for() {
    // The Counter is the last body built from the old kit rows, and the only one that ever
    // had a corner grip. The cells want one size; a grip let the same module stand at four,
    // with the difference all air. So the grip is gone, and a width written into the patch by
    // an older build is ignored.
    let (mut rig, ids) = Rig::real("logic.counter", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(6);
    let at = rig.frame_rect(node);
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.params
            .insert(crate::canvas::viewer::kit::WIDTH_PARAM.into(), serde_json::json!(420.0));
        n.params.insert("ui_h".into(), serde_json::json!(420.0));
    }
    rig.idle(6);
    let sized = rig.frame_rect(node);
    assert!(
        (sized.width() - at.width()).abs() < 1.0 && (sized.height() - at.height()).abs() < 1.0,
        "the Counter took a size it has no grip to be given: {at:?} -> {sized:?}",
    );
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
        // In the fonts the app draws with: Map Range's pins are `in↓ in↑ out↓ out↑`, and the
        // stock fonts have no arrows - their fallback box is wide enough to cost a block.
        crate::app::fonts::setup_fonts(&rig.ctx);
        rig.idle(6);
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
fn a_readout_starts_three_blocks_by_two_and_comes_down_to_one_by_two() {
    // Three blocks across and two down: big enough to read off the patch, small enough to
    // stand a row of them beside each other. The grip works both ways, down to one block by
    let (mut rig, ids) = Rig::real("display.readout", &[egui::pos2(100.0, 300.0)]);
    rig.idle(6);
    let open = rig.frame_rect(ids[0]);
    assert_eq!(
        (open.width(), open.height()),
        (3.0 * B, 2.0 * B),
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
fn a_pill_field_opens_with_the_value_you_can_edit() {
    // The field used to open empty with the current value painted behind it as a hint, so a
    // pill could be reset but never edited: there was nothing to put a caret into, and the
    // number you could see wasn't yours to change. It opens holding the value instead.
    let (mut rig, ids) = Rig::real("logic.delay", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    let time = |rig: &Rig| {
        rig.canvas
            .snarl
            .get_node(node)
            .and_then(|n| n.params.get("time").and_then(|v| v.as_f64()))
            .unwrap_or(100.0)
    };
    rig.idle(4);
    assert_eq!(time(&rig), 100.0, "the delay pill's starting time");
    let pill = rig.traced_rect(node, "delay_time_pill").unwrap_or(egui::Rect::NOTHING);
    assert!(pill.is_positive(), "no time pill to edit: {pill:?}");
    rig.double_click(pill.center());
    rig.idle(3);
    // One key to move the caret off any selection, then a digit: appended to the value that
    // was already in the field, that reads 1005 - typed into an empty field, 5.
    rig.frame(vec![egui::Event::Key {
        key: egui::Key::ArrowRight,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
        physical_key: None,
    }]);
    rig.idle(1);
    rig.type_text("5");
    assert_eq!(
        time(&rig),
        1005.0,
        "the pill's field didn't open with 100 in it to edit; arrow to the end and type 5 gave {}",
        time(&rig),
    );
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
fn a_title_is_cut_to_fit_and_left_out_when_there_is_no_word_to_show() {
    // One rule on every module: work out the room the name has - to the frame's edge, less
    // whatever sits there - and if the name fits, draw it; if only part of it fits, draw the
    // part with dots to say it was cut; if the room is too small for a word that means
    // anything, draw no name at all. The hover text carries the whole name either way.
    let (mut rig, ids) = Rig::real("module.knob", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    let name = "Blend amount for the slow filter";
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.display_name = name.to_string();
    }
    let title_at = |rig: &mut Rig, w: f32| -> (egui::Rect, egui::Rect) {
        rig.canvas
            .snarl
            .get_node_mut(node)
            .unwrap()
            .params
            .insert(kit_width_param().into(), serde_json::json!(w));
        rig.idle(6);
        rig.ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new(("module_ctrl_rect", node.0, "header_title")),
                Xysr([0.0; 4]),
            )
        });
        rig.idle(3);
        (rig.frame_rect(node), rig.traced_rect(node, "header_title").unwrap_or(egui::Rect::NOTHING))
    };

    // Three blocks across: room enough for part of the name, so it is drawn cut with dots - and
    // stops where the Uni/Bi switch begins.
    let (frame, title) = title_at(&mut rig, 3.0 * MAJOR);
    assert!(
        title.is_positive() && title.width() > 20.0,
        "a {frame:?} header dropped its name instead of cutting it: {title:?}",
    );
    assert!(
        frame.contains_rect(title.expand(0.5)),
        "the cut name {title:?} isn't inside {frame:?}",
    );
    assert!(
        title.right() <= frame.right() - 24.0 + 1.0,
        "the cut name {title:?} runs under the 24-point Uni/Bi switch at the right of {frame:?}",
    );
    // The cut is what made this rule worth having: the name is never what holds a module open.
    let cut = title.width();

    // Too little room for a word: no name, and the knob still goes down to its own floor.
    let (narrow, title) = title_at(&mut rig, MAJOR);
    assert_eq!(narrow.width(), MAJOR, "the name kept the knob wider than its content needs: {narrow:?}");
    assert!(
        !title.is_positive() || title.width() < 8.0,
        "a {narrow:?} header painted {title:?} where there is no room for a word",
    );

    // Wide enough for the whole thing: the name comes back, and widening it further changes
    // nothing, which is how we know it isn't being cut any more.
    let (wide, whole) = title_at(&mut rig, 5.0 * MAJOR);
    assert!(
        whole.is_positive() && whole.width() > cut,
        "the name didn't come back whole at {wide:?}: {whole:?} (cut was {cut:.0})",
    );
    let (wider, whole_again) = title_at(&mut rig, 7.0 * MAJOR);
    assert!(
        (whole_again.width() - whole.width()).abs() < 1.0,
        "the name was still being cut at {wider:?}: {whole:?} -> {whole_again:?}",
    );
    assert!(
        whole.width() > cut,
        "the whole name {whole:?} should be wider than the cut one at {frame:?} ({cut:.0})",
    );
}


#[test]
fn a_dragged_combiner_row_rides_the_finger_and_its_neighbour_steps_aside() {
    // The Combiner's rows move the way every other list's do (`StripAnim`): the row under the
    // finger follows it, and the row it has passed slides into the gap it left - before the
    // finger lets go, so a drag says where it will land. They used to be live drop zones that
    // stood still and drew a line.
    let (mut rig, ids) = Rig::real("module.automap_combiner", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(6);
    let (row0, row1) = (rig.traced(node, "combiner_row_0"), rig.traced(node, "combiner_row_1"));
    let pitch = row1.top() - row0.top();
    let grab = egui::pos2(row0.left() + 8.0, row0.center().y);
    let button = |pos: egui::Pos2, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    let start = rig.to_global * grab;
    let end = rig.to_global * egui::pos2(grab.x, row1.center().y);
    rig.frame(vec![egui::Event::PointerMoved(start)]);
    rig.frame(vec![button(start, true)]);
    for s in 1..=8 {
        rig.frame(vec![egui::Event::PointerMoved(start + (end - start) * (s as f32 / 8.0))]);
    }
    // Held there while the rows ease to where they belong.
    for _ in 0..12 {
        rig.frame(vec![egui::Event::PointerMoved(end)]);
    }
    let (held, passed) = (rig.traced(node, "combiner_row_0"), rig.traced(node, "combiner_row_1"));
    assert!(
        (held.top() - row0.top() - pitch).abs() < 2.0,
        "the held row didn't follow the finger a row down: {held:?} from {row0:?}"
    );
    assert!(
        (passed.top() - row1.top() + pitch).abs() < 2.0,
        "the row it passed didn't step into the gap: {passed:?} from {row1:?}"
    );
    rig.frame(vec![button(end, false)]);
    rig.idle(12);
    // Let go: the ports have changed places and every row is back on its own band.
    let (a, b) = (rig.traced(node, "combiner_row_0"), rig.traced(node, "combiner_row_1"));
    assert!(
        (a.top() - row0.top()).abs() < 1.0 && (b.top() - row1.top()).abs() < 1.0,
        "the rows didn't settle on their bands: {a:?} {b:?}"
    );
}

#[test]
fn a_combiner_row_move_takes_its_settings_with_it() {
    // The Combiner's rows are ports and the engine reads them by position - the lowest port
    // that offers a pin wins - so two of its settings are stored by port number: the port's
    // default merge policy, and a pin pinned to a particular port. Dragging a row down has
    // to carry both, or the port that moved keeps a policy belonging to the row it jumped
    // over, and a pin pinned to a device is suddenly pinned to whatever landed there.
    let (mut rig, ids) = Rig::real("module.automap_combiner", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(6);
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.params.insert(
            "combiner_port_default".into(),
            serde_json::json!({ "0": "ADD", "1": "MULT" }),
        );
        n.params
            .insert("combiner_pin_port".into(), serde_json::json!({ "left_stick": 0 }));
    }
    rig.idle(4);
    let (row0, row1) = (rig.traced(node, "combiner_row_0"), rig.traced(node, "combiner_row_1"));
    assert!(
        row0.is_positive() && row1.is_positive(),
        "no Combiner rows to drag: {row0:?} {row1:?}",
    );
    rig.drag(egui::pos2(row0.left() + 8.0, row0.center().y), egui::vec2(0.0, row1.top() - row0.top()));
    rig.idle(4);
    let n = rig.canvas.snarl.get_node(node).unwrap();
    let pd = n.params.get("combiner_port_default").and_then(|v| v.as_object());
    let pd = pd.unwrap_or_else(|| panic!("the port policies vanished after the move: {:?}", n.params));
    assert_eq!(
        pd.get("1").and_then(|v| v.as_str()),
        Some("ADD"),
        "the moved port lost its merge policy: {pd:?}",
    );
    assert_eq!(
        pd.get("0").and_then(|v| v.as_str()),
        Some("MULT"),
        "the port it passed took the wrong policy: {pd:?}",
    );
    let pp = n.params.get("combiner_pin_port").and_then(|v| v.as_object());
    let pp = pp.unwrap_or_else(|| panic!("the pin pins vanished: {:?}", n.params));
    assert_eq!(
        pp.get("left_stick").and_then(|v| v.as_u64()),
        Some(1),
        "a pin pinned to the moved port followed the position, not the port: {pp:?}",
    );
}

#[test]
fn a_knob_shows_its_value_once_the_frame_has_room_for_it() {
    // The smallest knob is a face and a pointer: at 56 by 56 there's nowhere to put the number
    // without covering the arc or shrinking what you drag, so it shows none. Any frame taller
    // than that floor has a line the round face can't use, and the number goes under it. At the
    // floor itself - one row of face - the number only starts at four blocks of width, where
    // there's still a track worth aiming at beside it.
    let (mut rig, ids) = Rig::real("module.knob", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(6);
    let measure = |rig: &mut Rig, w: f32, h: f32| -> (egui::Rect, egui::Rect, egui::Rect) {
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            n.params.insert(kit_width_param().into(), serde_json::json!(w));
            n.params.insert("ui_h".into(), serde_json::json!(h));
        }
        rig.idle(6);
        for what in ["knob_face", "knob_number", "body_column"] {
            rig.ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new(("module_ctrl_rect", node.0, what)), Xysr([0.0; 4]))
            });
        }
        rig.idle(4);
        (
            rig.frame_rect(node),
            rig.traced_rect(node, "knob_face").unwrap_or(egui::Rect::NOTHING),
            rig.traced_rect(node, "knob_number").unwrap_or(egui::Rect::NOTHING),
        )
    };

    let (f, face, number) = measure(&mut rig, 56.0, 56.0);
    assert!(face.is_positive(), "a 56x56 knob lost its face: {f:?}");
    assert!(!number.is_positive(), "a 56x56 knob draws {number:?} it has no room for");

    let (f, _face, number) = measure(&mut rig, 84.0, 56.0);
    assert!(!number.is_positive(), "a {f:?} knob at the two-block floor crowds a reading {number:?} beside a 3-block track");

    let (f, _face, number) = measure(&mut rig, 112.0, 56.0);
    assert!(
        number.is_positive() && f.contains_rect(number.expand(0.5)),
        "a {f:?} knob four blocks across at the floor should read its value, got {number:?}",
    );

    let (f, face, number) = measure(&mut rig, 56.0, 84.0);
    assert!(
        number.is_positive() && f.contains_rect(number.expand(0.5)),
        "a {f:?} knob taller than the floor should read its value, got {number:?}",
    );
    assert!(
        number.top() >= face.bottom() - 1.0,
        "the reading {number:?} isn't under the face {face:?} it shares {f:?} with",
    );

    let (_f, face, number) = measure(&mut rig, 112.0, 112.0);
    assert!(
        face.contains(number.center()),
        "a dial's reading {number:?} should sit inside the face {face:?}",
    );
}
#[test]
fn a_knob_at_any_step_of_the_finer_grid_keeps_its_dial_inside() {
    // The 28-point step makes sizes reachable that the old grid skipped over: 140 wide over 84
    // tall, 84 over 112, a 56-wide fader strip with a 140-tall body. The dial, the track and
    // the reading are all drawn from the room they're given, so each of those shapes is one the
    // drawing has to have thought about - and a shape it hasn't thought about shows up as the
    // face or the number hanging outside the frame.
    let (mut rig, ids) = Rig::real("module.knob", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(6);
    for w in [56.0f32, 84.0, 112.0, 140.0, 168.0] {
        for h in [56.0f32, 84.0, 112.0, 140.0] {
            if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
                n.params.insert(kit_width_param().into(), serde_json::json!(w));
                n.params.insert("ui_h".into(), serde_json::json!(h));
            }
            // Traces are written when a body draws and stay otherwise, so a measurement at a
            // new size has to start from nothing: without this the number read back would be
            // the one from the previous, larger frame.
            for what in ["knob_face", "knob_number", "body_column"] {
                rig.ctx.data_mut(|d| {
                    d.insert_temp(
                        egui::Id::new(("module_ctrl_rect", node.0, what)),
                        Xysr([0.0; 4]),
                    )
                });
            }
            rig.idle(6);
            // A frame the grip has just resized is laid out at its old size for a frame or two
            // on the way to the new one. Clear the traces once the size has arrived, so what's
            // read back is what a settled frame of this size actually draws.
            for what in ["knob_face", "knob_number", "body_column"] {
                rig.ctx.data_mut(|d| {
                    d.insert_temp(
                        egui::Id::new(("module_ctrl_rect", node.0, what)),
                        Xysr([0.0; 4]),
                    )
                });
            }
            rig.idle(4);
            let f = rig.frame_rect(node);
            assert!(
                f.width() >= MAJOR && f.height() >= MAJOR,
                "a knob asked for {w}x{h} and the frame went under its 56-point floor: {f:?}",
            );
            let face = rig.traced_rect(node, "knob_face").unwrap_or(egui::Rect::NOTHING);
            if !face.is_positive() {
                continue; // collapsed, or a shape that draws no face
            }
            assert!(
                f.contains_rect(face.shrink(-0.5)),
                "a {f:?} knob puts its face {face:?} outside the frame (asked {w}x{h})",
            );
            // A reading is drawn where it costs nothing to drag: any frame taller than the
            // two-block floor has a line the round face can't use, so it shows the number; at
            // the floor itself only four blocks of width and more do, because below that the
            // track left beside the number is too short to aim at.
            let at_floor = f.height() <= MAJOR + 0.5;
            let shows = !at_floor || f.width() >= 2.0 * MAJOR;
            let num = rig
                .traced_rect(node, "knob_number")
                .unwrap_or(egui::Rect::NOTHING);
            if shows {
                assert!(
                    num.is_positive() && f.contains_rect(num.expand(0.5)),
                    "a {f:?} knob with a {face:?} face has no reading inside the frame (asked {w}x{h})",
                );
            } else {
                assert!(
                    !num.is_positive(),
                    "a {f:?} knob at its two-block floor draws a reading {num:?} it has no room for (asked {w}x{h})",
                );
            }
        }
    }
}

#[test]
#[ignore]
fn measure_every_module_size() {
    // A census, not a claim: what each module's body actually asks for (`no-snap`) next to
    // the size the grid gives it, and its pin count. The gap between the two is what a body
    // redesign is buying, so this says which bodies are still too big for what they hold.
    let mut rows: Vec<(f32, String, f32, f32, f32, f32, usize, usize)> = Vec::new();
    for r in flexinput_modules::all_modules() {
        let desc = r.descriptor;
        let (mut rig, ids) = Rig::build(desc.clone(), &[egui::pos2(100.0, 300.0)]);
        rig.idle(10);
        let f = rig.frame_rect(ids[0]);
        let (mut rig2, ids2) = Rig::build(desc, &[egui::pos2(100.0, 300.0)]);
        rig2.grid.snap = false;
        rig2.idle(10);
        let c = rig2.frame_rect(ids2[0]);
        let n = rig.canvas.snarl.get_node(ids[0]).unwrap();
        rows.push((
            c.width() * c.height(),
            n.module_id.clone(),
            c.width(),
            c.height(),
            f.width(),
            f.height(),
            n.inputs.len(),
            n.outputs.len(),
        ));
    }
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!("{:<32} {:>10} {:>10} {:>8}", "module", "content", "grid", "in/out");
    for r in &rows {
        println!("{:<32} {:>4.0}x{:<4.0} {:>4.0}x{:<4.0}  {:>2}/{}", r.1, r.2, r.3, r.4, r.5, r.6, r.7);
    }
    println!("{} modules, total content area {:.0}", rows.len(), rows.iter().map(|r| r.0).sum::<f32>());
}

#[test]
fn the_per_channel_filters_are_pills_with_their_channels_in_the_header() {
    // Delay, Average and DC Filter were a label, a slider rail and a value box per setting,
    // plus a "Channels" row: 252 points of module to hold one or two numbers. The settings are
    // pills now, centred on the module, and a channel - always a matched In/Out pair - is
    // added from the header the way the variadic operations' pins are.
    for (module, salts) in [
        ("module.delay", ["delay_ms"].as_slice()),
        ("module.average", ["samples", "spike_mad"].as_slice()),
        ("module.dc_filter", ["window_ms", "decay_ms"].as_slice()),
    ] {
        let (mut rig, ids) = Rig::real(module, &[egui::pos2(100.0, 300.0)]);
        let node = ids[0];
        rig.idle(8);
        let f = rig.frame_rect(node);
        assert!(
            whole_blocks(f.width()) && whole_blocks(f.height()),
            "{module}: {f:?} off the grid",
        );
        // Five blocks: the pills are what they are and the name in the header is painted over
        // the space rather than added to the row, so "DC Filter" costs nothing over "Delay".
        assert!(
            f.width() <= 5.0 * B,
            "{module}: {f:?} is still as wide as the label rows it used to be",
        );
        assert!(
            f.height() >= 4.0 * B,
            "{module}: {f:?} is shallower than the other small modules",
        );
        for s in salts {
            let r = rig.traced(node, &format!("{s}_pill"));
            assert!(
                r.is_positive() && f.contains_rect(r.shrink(-0.5)),
                "{module}: the {s} pill {r:?} isn't inside {f:?}",
            );
            assert!(
                (r.center().x - f.center().x).abs() < 1.0,
                "{module}: the {s} pill {r:?} isn't on the module's centre line ({f:?})",
            );
        }
        let before = rig.frame_rect(node);
        let h = rig.ctx.style().spacing.interact_size.y;
        rig.click(egui::pos2(before.right() - 6.0 - h * 0.5, before.top() + 4.0 + h * 0.5));
        rig.idle(6);
        let n = rig.canvas.snarl.get_node(node).unwrap();
        assert_eq!(n.inputs.len(), 2, "{module}: the header's + didn't add a channel");
        assert_eq!(n.outputs.len(), 2, "{module}: the channel's output pin is missing");
        assert_eq!(n.inputs[1].name, "In 2", "{module}: the new channel isn't named like its neighbour");
        assert_eq!(n.outputs[1].name, "Out 2", "{module}: the new output isn't named like its neighbour");
    }
}

#[test]
fn the_counter_is_a_stack_of_cells_not_four_labelled_rows() {
    // Counter had a mode strip, an "Output" row, "Step", and a "Range" row with two captioned
    // boxes: 335 points across, most of it the words "Output", "Step" and "Range". Its cells
    // are its own now, two across and centred on the module, so the frame's width is the cells
    // and its six pins - and every cell still has to sit inside the frame it's given.
    let (mut rig, ids) = Rig::real("logic.counter", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(8);
    let f = rig.frame_rect(node);
    assert!(
        f.width() <= 7.0 * B,
        "the counter is still as wide as its label rows were: {f:?}",
    );
    // Two things pay for that: the cells ask for a constant rather than half the width the
    // frame handed them (asking for a share of the handout is how it stayed wide), and its six
    // fixed functional pin words are drawn short - `increase` and `decrease` beside six pins is
    // most of a block of module. The names the patch stores are untouched.
    assert!(
        crate::canvas::viewer::kit::pin_abbreviates("logic.counter"),
        "the counter's pin words are being spelled out again",
    );
    for what in ["counter_mode_0", "counter_mode_1", "counter_out"] {
        let r = rig.traced_rect(node, what).unwrap_or(egui::Rect::NOTHING);
        assert!(
            r.is_positive() && f.contains_rect(r.shrink(-0.5)),
            "the counter's {what} {r:?} isn't inside {f:?}",
        );
        assert!(
            (r.center().x - f.center().x).abs() < 1.0,
            "the counter's {what} {r:?} isn't on the module's centre line ({f:?})",
        );
    }
    // The step and the reset share the last row: the pair is what sits on the centre line, and
    // they don't overlap - the reset used to be at the end of the output row, where it stood on
    // top of the `0..1` cell.
    let (r_step, r_reset) = (
        rig.traced_rect(node, "step_pill").unwrap_or(egui::Rect::NOTHING),
        rig.traced_rect(node, "counter_reset").unwrap_or(egui::Rect::NOTHING),
    );
    let step_row = r_step.union(r_reset);
    assert!(
        r_step.is_positive()
            && r_reset.is_positive()
            && f.contains_rect(step_row.shrink(-0.5))
            && !r_step.intersects(r_reset),
        "the counter's step row {r_step:?} {r_reset:?} isn't two controls inside {f:?}",
    );
    assert!(
        (step_row.center().x - f.center().x).abs() < 1.0,
        "the counter's step row {step_row:?} isn't on the module's centre line ({f:?})",
    );
    assert!(
        (r_step.top() - r_reset.top()).abs() < 1.0,
        "the step pill and its reset aren't on one row: {r_step:?} {r_reset:?}",
    );
    let r_out = rig.traced(node, "counter_out");
    assert!(
        !r_out.intersects(r_reset),
        "the reset {r_reset:?} sits on the output row {r_out:?}",
    );
    assert!(
        (r_out.width() - rig.traced(node, "counter_mode_0").width()).abs() < 0.5,
        "the output row {r_out:?} isn't the width the mode rows are",
    );
    // The range is two cells across, so it's the pair that sits on the centre line.
    let (r_min, r_max) = (
        rig.traced_rect(node, "min_pill").unwrap_or(egui::Rect::NOTHING),
        rig.traced_rect(node, "max_pill").unwrap_or(egui::Rect::NOTHING),
    );
    let pair = r_min.union(r_max);
    assert!(
        r_min.is_positive() && r_max.is_positive() && f.contains_rect(pair.shrink(-0.5)),
        "the counter's range cells {r_min:?} {r_max:?} aren't inside {f:?}",
    );
    assert!(
        (pair.center().x - f.center().x).abs() < 1.0,
        "the counter's range {pair:?} isn't on the module's centre line ({f:?})",
    );
    // The cells are two across, so the two mode rows and the range row line up in columns.
    let (m0, m1) = (rig.traced(node, "counter_mode_0"), rig.traced(node, "counter_mode_1"));
    assert!(
        (m0.left() - m1.left()).abs() < 0.5 && (m0.width() - m1.width()).abs() < 0.5,
        "the mode rows aren't the same width: {m0:?} {m1:?}",
    );
}

#[test]
fn the_dropdown_is_a_column_of_its_options_under_its_reading() {
    // The menu module sized itself from a `box_width` parameter the user could dial to 600,
    // its rows led with their own `Г—`, and its handle was a monospace `в‰Ў` nothing else in the
    // app uses. It wears the list idiom now: one constant column centred on the module, the
    // current option as the reading at the top, then a row per option - handle, the name cut at
    // the column's edge with the whole of it on hover, the `Г—` at the right - and the `+` that
    // adds one in that same column. Reordering moves the option and keeps the selection on it,
    // because the engine outputs whatever sits at the selected index.
    let (mut rig, ids) = Rig::real("module.dropdown", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.params.insert(
            "options".into(),
            serde_json::Value::Array(
                ["Alpha", "Beta", "Gamma"].into_iter().map(|s| serde_json::Value::String(s.to_string())).collect(),
            ),
        );
        n.params.insert("selected_index".into(), serde_json::Value::from(0_u64));
    }
    rig.idle(8);
    let f = rig.frame_rect(node);
    assert!(whole_blocks(f.width()) && whole_blocks(f.height()), "module.dropdown: {f:?} off the grid");
    assert!(
        f.width() <= 7.0 * B,
        "module.dropdown: {f:?} is wider than its column asks for - a parameter is driving it again",
    );
    let rows: Vec<egui::Rect> = (0..3)
        .map(|i| rig.traced_rect(node, &format!("dropdown_row_{i}")).unwrap_or(egui::Rect::NOTHING))
        .collect();
    for (i, r) in rows.iter().enumerate() {
        assert!(
            r.is_positive() && f.contains_rect(r.expand(1.0)),
            "module.dropdown: option row {i} {r:?} isn't inside {f:?}",
        );
        assert!(
            (r.width() - rows[0].width()).abs() < 0.5,
            "module.dropdown: option row {i} {r:?} is a different width from row 0 {:?}",
            rows[0],
        );
        if i > 0 {
            let pitch = rows[i].top() - rows[i - 1].top();
            assert!(
                (pitch - rows[1].top() + rows[0].top()).abs() < 0.5 && pitch > 20.0,
                "module.dropdown: option rows aren't one pitch apart: {rows:?}",
            );
        }
    }
    // The `+` that adds an option lives in the header, like the other lists' controls, not at
    // the end of the list where it read as one more option.
    let add = rig.traced_rect(node, "header_add").unwrap_or(egui::Rect::NOTHING);
    assert!(
        add.is_positive()
            && f.contains_rect(add.expand(1.0))
            && add.top() < f.top() + 2.0 * B
            && add.right() > f.right() - B,
        "module.dropdown: the header `+` {add:?} isn't at the top right of {f:?}",
    );
    assert!(
        rig.traced_rect(node, "dropdown_add").is_none(),
        "module.dropdown: the list still ends in its own `+` button",
    );
    // Drag the first option's handle down two rows.
    let handle = egui::pos2(rows[0].left() + 6.0, rows[0].center().y);
    let two = 2.0 * (rows[1].top() - rows[0].top());
    rig.drag(handle, egui::vec2(0.0, two));
    rig.idle(6);
    let opts = |r: &Rig| -> Vec<String> {
        r.canvas
            .snarl
            .get_node(node)
            .and_then(|n| n.params.get("options").and_then(|v| v.as_array().cloned()))
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect()
    };
    assert_eq!(opts(&rig), ["Beta", "Gamma", "Alpha"], "module.dropdown: the handle didn't move the option");
    let sel = rig
        .canvas
        .snarl
        .get_node(node)
        .and_then(|n| n.params.get("selected_index").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    assert_eq!(sel, 2, "module.dropdown: the selection stayed on the slot instead of the option");
    // And the header's `+` adds an option.
    rig.click(egui::pos2(add.center().x, add.center().y));
    rig.idle(4);
    assert_eq!(opts(&rig).len(), 4, "module.dropdown: the header `+` didn't add an option");
}

#[test]
fn a_dropdown_shift_pin_moves_the_option_it_shows() {
    // The menu module takes an optional input that adds to the option chosen on it, so a dial
    // or a comparator can walk the list without anyone clicking it. The module shows the option
    // that is actually coming out rather than the one it was left set to, and picking an option
    // while the pin holds it somewhere else moves the base, so the option clicked is the one
    // that lands.
    // A second module stands to the left only to have something to wire into the pin.
    let (mut rig, ids) = Rig::real("module.dropdown", &[egui::pos2(100.0, 300.0), egui::pos2(-400.0, 300.0)]);
    let source = ids[1];
    // A patch saved before the pin existed has no input at all: the module gives itself the
    // missing pin the first time it draws, rather than going on unshiftable.
    if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
        n.inputs.clear();
    }
    rig.idle(4);
    let pin_names: Vec<String> = rig
        .canvas
        .snarl
        .get_node(ids[0])
        .map(|n| n.inputs.iter().map(|d| d.name.clone()).collect())
        .unwrap_or_default();
    assert_eq!(pin_names, ["shift"], "a dropdown saved without the shift pin didn't gain it");
    let node = ids[0];
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.params.insert(
            "options".into(),
            serde_json::Value::Array(
                ["Alpha", "Beta", "Gamma"]
                    .into_iter()
                    .map(|s| serde_json::Value::String(s.to_string()))
                    .collect(),
            ),
        );
        n.params.insert("selected_index".into(), serde_json::Value::from(0_u64));
    }
    rig.idle(8);
    let rows: Vec<egui::Rect> = (0..3)
        .map(|i| rig.traced_rect(node, &format!("dropdown_row_{i}")).unwrap_or(egui::Rect::NOTHING))
        .collect();
    // With nothing wired, the option chosen on the module is the one marked as coming out.
    let lit0 = rig.traced_rect(node, "dropdown_effective").unwrap_or(egui::Rect::NOTHING);
    assert!(
        lit0.is_positive() && (lit0.top() - rows[0].top()).abs() < 0.5,
        "module.dropdown: with nothing wired it should mark the option it was set to: {lit0:?}",
    );
    // What the engine hands the UI is the module's *outputs* - it mirrors no Dropdown input -
    // so the option coming out is read from the `index` output. A reading of the pin itself
    // always came back empty in the app, and the mark never left the option the module was
    // set to. A value on the outputs with nothing wired changes nothing either: unwired, the
    // option set is the option out.
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.extra.last_out = vec![
            Some(flexinput_core::Signal::Float(0.5)),
            Some(flexinput_core::Signal::Int(1)),
        ];
    }
    rig.idle(4);
    let unwired = rig.traced_rect(node, "dropdown_effective").unwrap_or(egui::Rect::NOTHING);
    assert!(
        (unwired.top() - rows[0].top()).abs() < 0.5,
        "module.dropdown: with nothing wired the option set is the option out: {unwired:?}",
    );
    rig.canvas.snarl.connect(
        egui_snarl::OutPinId { node: source, output: 0 },
        egui_snarl::InPinId { node, input: 0 },
    );
    rig.idle(4);
    let lit = rig.traced_rect(node, "dropdown_effective").unwrap_or(egui::Rect::NOTHING);
    assert!(
        lit.is_positive()
            && (lit.left() - rows[1].left()).abs() < 0.5
            && (lit.top() - rows[1].top()).abs() < 0.5
            && (lit.width() - rows[1].width()).abs() < 0.5,
        "module.dropdown: +1 on the shift pin should mark option 2: {lit:?} vs {:?}",
        rows[1],
    );
    // Clicking the first option while the pin is at +1 stores the base a step before it, so the
    // option clicked is the option that comes out.
    rig.click(egui::pos2(rows[0].center().x, rows[0].center().y));
    rig.idle(4);
    let sel = rig
        .canvas
        .snarl
        .get_node(node)
        .and_then(|n| n.params.get("selected_index").and_then(|v| v.as_u64()))
        .unwrap_or(9);
    assert_eq!(sel, 2, "module.dropdown: clicking an option ignored the shift pin");
}

#[test]
fn the_oscillator_is_a_shape_a_wave_and_two_numbers() {
    // The generator used to be three rows of small controls with a separator in one of them:
    // shape buttons, a unit toggle and a DragValue, a "Phase" label with a DragValue and a
    // Bi/Uni pair, then a waveform stretched across whatever width that left. The settings are
    // cells now, in a column of a constant width centred on the module, and the wave is a band
    // of painted line between the shape and the numbers - it costs nothing in width.
    let (mut rig, ids) = Rig::real("generator.oscillator", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(8);
    let f = rig.frame_rect(node);
    assert!(whole_blocks(f.width()) && whole_blocks(f.height()), "generator.oscillator: {f:?} off the grid");
    assert!(f.width() <= 7.0 * B, "generator.oscillator: {f:?} is wider than its cells ask for");
    let rows: Vec<(&str, egui::Rect)> = [
        "osc_shape",
        "osc_preview",
        "osc_freq_pill",
        "osc_unit",
        "osc_phase_pill",
        "osc_bipolar",
    ]
    .into_iter()
    .map(|k| (k, rig.traced_rect(node, k).unwrap_or(egui::Rect::NOTHING)))
    .collect();
    for (k, r) in &rows {
        assert!(
            r.is_positive() && f.contains_rect(r.shrink(-0.5)),
            "generator.oscillator: {k} {r:?} isn't inside {f:?}",
        );
    }
    // Each row's pair is centred on the module: the body column is fenced by pin labels of
    // different widths, so the column's middle isn't the module's.
    for (a, b) in [
        ("osc_shape", "osc_preview"),
        ("osc_freq_pill", "osc_unit"),
        ("osc_phase_pill", "osc_bipolar"),
    ] {
        let ra = rows.iter().find(|(k, _)| *k == a).map(|(_, r)| *r).unwrap();
        let rb = rows.iter().find(|(k, _)| *k == b).map(|(_, r)| *r).unwrap();
        let pair = ra.union(rb);
        assert!(
            (pair.center().x - f.center().x).abs() < 1.0,
            "generator.oscillator: {a} + {b} {pair:?} isn't on the module's centre line ({f:?})",
        );
    }
    // The rows start at one edge.
    let left = rows[0].1.left();
    for (k, r) in rows.iter().skip(1) {
        if *k != "osc_unit" && *k != "osc_bipolar" {
            assert!(
                (r.left() - left).abs() < 0.5,
                "generator.oscillator: {k} {r:?} doesn't start where the shape strip does {left:?}",
            );
        }
    }
    // The wave sits between the shape it belongs to and the frequency beside it.
    let shape = rows.iter().find(|(k, _)| *k == "osc_shape").map(|(_, r)| *r).unwrap();
    let preview = rows.iter().find(|(k, _)| *k == "osc_preview").map(|(_, r)| *r).unwrap();
    let freq = rows.iter().find(|(k, _)| *k == "osc_freq_pill").map(|(_, r)| *r).unwrap();
    assert!(
        preview.top() > shape.bottom() && preview.bottom() < freq.top(),
        "generator.oscillator: the wave {preview:?} isn't between the shape {shape:?} and the frequency {freq:?}",
    );
    // And the strip's last cell is the square wave, which is what the band then shows.
    // The strip is the width of the wave band under it, and stops where the output pin's label
    // begins: a strip laid out with the style's extra spacing between its cells reached into it.
    assert!(
        (shape.width() - preview.width()).abs() < 0.5,
        "generator.oscillator: the shape strip {shape:?} is a different width from the wave band {preview:?}",
    );
    assert!(
        shape.right() < f.right() - 20.0,
        "generator.oscillator: the shape strip reaches to {right:.0} of a frame ending at {fr:.0}",
        right = shape.right(),
        fr = f.right(),
    );
    rig.click(egui::pos2(shape.right() - 8.0, shape.center().y));
    rig.idle(4);
    let shape_now = rig
        .canvas
        .snarl
        .get_node(node)
        .and_then(|n| n.params.get("shape").and_then(|v| v.as_str()).map(str::to_string))
        .unwrap_or_default();
    assert_eq!(shape_now, "square", "generator.oscillator: the shape strip's last cell isn't the square wave");
}
#[test]
fn the_macro_lists_its_ports_like_every_other_list() {
    // A Macro Output's rows used to measure themselves out as an icon button, a 96-point name
    // field, a 58-point type dropdown, a `вњ•`, and a button named "+ Add output" under them: 308
    // points of module for a port. It's the list idiom now - handle, icon, name, the type as one
    // letter with the names in the popup, and the `Г—` at the right edge - and the `+` is in the
    // header with the other lists'. A moved port takes its wires with it, because its row order
    // is the node's output-pin order.
    let (mut rig, ids) = Rig::real("module.macro", &[egui::pos2(100.0, 300.0), egui::pos2(900.0, 300.0)]);
    let (node, other) = (ids[0], ids[1]);
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        use flexinput_core::macros as mac;
        let mk = |id: &str, name: &str, ty: flexinput_core::SignalType| mac::MacroPortDef {
            id: id.to_string(),
            name: name.to_string(),
            icon: String::new(),
            icon_svg: String::new(),
            signal_type: ty,
        };
        n.params.insert(
            mac::MACRO_PORTS_PARAM.to_string(),
            mac::ports_to_value(&[
                mk("aa11bb22", "Left", flexinput_core::SignalType::Bool),
                mk("cc33dd44", "Right", flexinput_core::SignalType::Float),
            ]),
        );
        n.outputs = vec![
            PinDescriptor::new("Left", flexinput_core::SignalType::Bool),
            PinDescriptor::new("Right", flexinput_core::SignalType::Float),
        ];
        n.params.insert(
            "output_pin_ids".into(),
            serde_json::Value::Array(vec![
                serde_json::Value::String("macro:aa11bb22".into()),
                serde_json::Value::String("macro:cc33dd44".into()),
            ]),
        );
    }
    rig.idle(8);
    let f = rig.frame_rect(node);
    assert!(whole_blocks(f.width()) && whole_blocks(f.height()), "module.macro: {f:?} off the grid");
    // Nine blocks: the editor row spells the signal type out (Bool, Float, ...) rather than
    // abbreviating it, which the module pays a block for, and the list runs the width the body
    // has instead of being centred on the frame with dead margin beside it.
    assert!(f.width() <= 9.0 * B, "module.macro: {f:?} is wider than its list rows ask for");
    let rows: Vec<egui::Rect> = (0..2)
        .map(|i| rig.traced_rect(node, &format!("macro_row_{i}")).unwrap_or(egui::Rect::NOTHING))
        .collect();
    for (i, r) in rows.iter().enumerate() {
        assert!(
            r.is_positive() && f.contains_rect(r.shrink(-0.5)),
            "module.macro: port row {i} {r:?} isn't inside {f:?}",
        );
        assert!(
            (r.width() - rows[0].width()).abs() < 0.5,
            "module.macro: port row {i} {r:?} is a different width from row 0 {:?}",
            rows[0],
        );
    }
    let add = rig.traced_rect(node, "header_add").unwrap_or(egui::Rect::NOTHING);
    assert!(
        add.is_positive() && f.contains_rect(add.expand(1.0)) && add.top() < f.top() + 2.0 * B,
        "module.macro: the header `+` {add:?} isn't at the top of {f:?}",
    );
    // A wire on the first port, so the move can be checked to carry it.
    rig.canvas.snarl.connect(
        egui_snarl::OutPinId { node, output: 0 },
        egui_snarl::InPinId { node: other, input: 0 },
    );
    rig.idle(4);
    let names = |r: &Rig| -> Vec<String> {
        r.canvas
            .snarl
            .get_node(node)
            .map(|n| n.outputs.iter().map(|d| d.name.clone()).collect())
            .unwrap_or_default()
    };
    assert_eq!(names(&rig), ["Left", "Right"], "module.macro: the ports as listed");
    let handle = egui::pos2(rows[0].left() + 6.0, rows[0].center().y);
    rig.drag(handle, egui::vec2(0.0, rows[1].center().y - rows[0].center().y));
    rig.idle(6);
    assert_eq!(names(&rig), ["Right", "Left"], "module.macro: the handle didn't move the port");
    let wired = rig
        .canvas
        .snarl
        .out_pin(egui_snarl::OutPinId { node, output: 1 })
        .remotes
        .len();
    assert_eq!(wired, 1, "module.macro: the wire didn't move with the port");
    let ids_param: Vec<String> = rig
        .canvas
        .snarl
        .get_node(node)
        .and_then(|n| n.params.get("output_pin_ids").and_then(|v| v.as_array().cloned()))
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        ids_param,
        ["macro:cc33dd44", "macro:aa11bb22"],
        "module.macro: the port ids didn't follow the port",
    );
}
#[test]
fn the_automap_port_lists_show_their_names_in_full_and_can_be_reordered() {
    // The Auto-Map splitter and collector list the ports they've taken off a bus. Those names
    // are the whole reason the module is on the canvas - which port feeds what, at a glance -
    // so they are never abbreviated the way a small module's `in_2` is, and a long learned name
    // makes the module wide instead. What the rows do share with the port strips is the shape:
    // handle, name, `Г—` at the right edge, all one width. And the handle moves a port, taking
    // its wire and its entry in the parallel id list with it, because the engine reads these
    // pins by position.
    for (module, side, param_key, salt) in [
        ("module.automap_split", "outputs", "output_pin_ids", "am_split"),
        ("module.automap_collect", "inputs", "collect_input_pin_ids", "am_collect"),
    ] {
        assert!(
            !crate::canvas::viewer::kit::pin_abbreviates(module),
            "{module}: its pin labels are being shortened - they're what the module is read by",
        );
        let (mut rig, ids) = Rig::real(module, &[egui::pos2(100.0, 300.0), egui::pos2(900.0, 300.0)]);
        let (node, other) = (ids[0], ids[1]);
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            for name in ["Left Stick", "Right Stick"] {
                if side == "outputs" {
                    n.outputs.push(PinDescriptor::new(name, SignalType::Any));
                } else {
                    n.inputs.push(PinDescriptor::new(name, SignalType::Any));
                }
            }
            // The splitter's first output is the Auto-Map passthrough, which isn't in the list.
            let ids_param: Vec<String> = if side == "outputs" {
                vec!["automap_pass".into(), "left_stick".into(), "right_stick".into()]
            } else {
                vec!["left_stick".into(), "right_stick".into()]
            };
            n.params.insert(
                param_key.into(),
                serde_json::Value::Array(ids_param.into_iter().map(serde_json::Value::String).collect()),
            );
        }
        rig.idle(8);
        let f = rig.frame_rect(node);
        assert!(whole_blocks(f.width()) && whole_blocks(f.height()), "{module}: {f:?} off the grid");
        let rows: Vec<egui::Rect> = (0..2)
            .map(|i| rig.traced_rect(node, &format!("{salt}_row_{i}")).unwrap_or(egui::Rect::NOTHING))
            .collect();
        for (i, r) in rows.iter().enumerate() {
            assert!(
                r.is_positive() && f.contains_rect(r.expand(1.0)),
                "{module}: list row {i} {r:?} isn't inside {f:?}",
            );
            assert!(
                (r.width() - rows[0].width()).abs() < 0.5,
                "{module}: list row {i} {r:?} is a different width from row 0 {:?}",
                rows[0],
            );
        }
        // A wire on the first listed port, so the move can be checked to carry it.
        let listed = 1usize; // index 0 is the bus / passthrough pin, never in the list
        if side == "outputs" {
            rig.canvas.snarl.connect(
                egui_snarl::OutPinId { node, output: listed },
                egui_snarl::InPinId { node: other, input: 0 },
            );
        } else {
            rig.canvas.snarl.connect(
                egui_snarl::OutPinId { node: other, output: 0 },
                egui_snarl::InPinId { node, input: listed },
            );
        }
        rig.idle(4);
        let names = |rig: &Rig| -> Vec<String> {
            rig.canvas
                .snarl
                .get_node(node)
                .map(|n| {
                    let all: &[PinDescriptor] = if side == "outputs" { &n.outputs } else { &n.inputs };
                    all.iter().skip(1).map(|p| p.name.clone()).collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(names(&rig), ["Left Stick", "Right Stick"], "{module}: the ports as listed");
        // Drag row 0's handle down onto row 1: the port moves, and so does its wire.
        let handle = egui::pos2(rows[0].left() + 4.0, rows[0].center().y);
        rig.drag(handle, egui::vec2(0.0, rows[1].center().y - rows[0].center().y));
        rig.idle(6);
        assert_eq!(
            names(&rig),
            ["Right Stick", "Left Stick"],
            "{module}: the handle didn't move the port (rows {rows:?} frame {f:?}, dragged from {:?}",
            egui::pos2(rows[0].left() + 9.0, rows[0].center().y),
        );
        let ids_param: Vec<String> = rig
            .canvas
            .snarl
            .get_node(node)
            .and_then(|n| n.params.get(param_key).and_then(|v| v.as_array().cloned()))
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();
        let moved = if side == "outputs" { &ids_param[2] } else { &ids_param[1] };
        assert_eq!(moved, "left_stick", "{module}: the id list didn't follow the port: {ids_param:?}");
        let wired = if side == "outputs" {
            rig.canvas
                .snarl
                .out_pin(egui_snarl::OutPinId { node, output: 2 })
                .remotes
                .len()
        } else {
            rig.canvas
                .snarl
                .in_pin(egui_snarl::InPinId { node, input: 2 })
                .remotes
                .len()
        };
        assert_eq!(wired, 1, "{module}: the wire didn't move with the port");
    }
}

#[test]
fn a_long_port_list_still_lands_on_the_grid_and_stays_readable() {
    // Nine ports is the case where a column of strips runs out of the module: the frame has to
    // keep growing a block per port, the empty band under the last strip has to stay the same
    // size it is at three ports, and each strip still has to leave room for its handle, its
    // `x` and a name - a strip that shrinks to a pixel is worse than one that makes the module
    // wider.
    let (mut rig, ids) = Rig::real("module.selector", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    let mut left = std::collections::BTreeMap::new();
    for ports in 3..=9usize {
        while rig.inputs(node) < ports + 1 {
            rig.set_inputs(node, rig.inputs(node) + 1);
        }
        rig.idle(6);
        let f = rig.frame_rect(node);
        let (a, b) = (rig.traced(node, "strip_0"), rig.traced(node, "strip_1"));
        let last = rig.traced(node, &format!("strip_{}", ports - 1));
        let pitch = b.top() - a.top();
        assert!(
            (pitch - B).abs() < 0.5,
            "{ports} ports: a row of {pitch:.1} isn't one block ({B})",
        );
        assert!(
            last.bottom() <= f.bottom() + 0.5,
            "{ports} ports: the last strip {last:?} hangs below the frame {f:?}",
        );
        // Handle, the `x`, a name with something in it, and the air between them.
        assert!(
            a.width() >= 70.0,
            "{ports} ports: a strip {a:?} is too narrow to hold its name (frame {f:?})",
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
fn a_switch_at_any_step_of_the_finer_grid_keeps_its_button_inside() {
    // The other body that reflows with the frame. Its button is the whole control, so a size
    // where the button no longer fits is a module that stopped working, not one that merely
    // looks slightly off.
    let (mut rig, ids) = Rig::real("module.switch", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(6);
    for w in [56.0f32, 84.0, 112.0, 140.0] {
        for h in [56.0f32, 84.0, 112.0] {
            if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
                n.params.insert(kit_width_param().into(), serde_json::json!(w));
                n.params.insert("ui_h".into(), serde_json::json!(h));
            }
            for what in ["switch_button", "body_column"] {
                rig.ctx.data_mut(|d| {
                    d.insert_temp(
                        egui::Id::new(("module_ctrl_rect", node.0, what)),
                        Xysr([0.0; 4]),
                    )
                });
            }
            rig.idle(6);
            let f = rig.frame_rect(node);
            let button = rig
                .traced_rect(node, "switch_button")
                .unwrap_or(egui::Rect::NOTHING);
            assert!(
                button.is_positive() && f.contains_rect(button.shrink(-0.5)),
                "a {f:?} switch puts its button {button:?} outside the frame (asked {w}x{h})",
            );
        }
    }
}

#[test]
fn the_grid_ships_without_a_brighter_line_and_offers_odd_ones() {
    // The brighter line used to be the old 56-point grid showing through a 28-point one. It
    // is not needed to keep the canvas readable, and an even step puts the bright lines
    // through a module four blocks across rather than around it - so nothing is brighter by
    // default, and the choices are odd: five blocks is one either side of a 4x4 module.
    let g = crate::settings::CanvasGrid::default();
    assert_eq!(g.accent_every, 0, "the grid ships with a brighter line: {g:?}");
    let choices = crate::settings::CanvasGrid::ACCENT_CHOICES;
    assert_eq!(choices.first().copied(), Some(0), "the menu doesn't offer no highlight at all: {choices:?}");
    for &n in choices.iter().filter(|n| **n > 0) {
        assert_eq!(n % 2, 1, "a brighter line every {n} blocks cuts through a 4-block module");
    }
    assert!(
        choices.contains(&5),
        "the menu lost the step that brackets a 4x4 module: {choices:?}",
    );
}

#[test]
fn every_mark_body_keeps_its_four_blocks_of_height() {
    // The mark bodies - a glyph and a pill or two - reserve nothing of their own, so their
    // height is whatever the grid leaves them. At the old 56-point step that was never under
    // 112 points; at 28 it can be 84, and the body pass tops them up (kit::mark_body) to
    // stop that. This is the guard for the mechanism: a module that joins the mark family
    // without being in that list settles a quarter shorter than its siblings, and nothing
    // else would notice.
    let marks: Vec<&str> = flexinput_modules::all_modules()
        .into_iter()
        .map(|r| r.descriptor.id)
        .filter(|id| crate::canvas::viewer::kit::mark_body(id))
        .collect();
    assert!(marks.len() >= 20, "the mark family went missing: {marks:?}");
    for id in marks {
        let (mut rig, ids) = Rig::real(id, &[egui::pos2(100.0, 300.0)]);
        rig.idle(6);
        let f = rig.frame_rect(ids[0]);
        assert!(
            f.height() >= 4.0 * B - 0.5,
            "{id}: only {f:?} tall - under the four blocks (112) the old grid gave it",
        );
        assert!(whole_blocks(f.height()), "{id}: {f:?} left the grid");
    }
}

#[test]
fn a_port_list_lands_on_the_block_grid() {
    // A strip row is 28 points - a 25-point strip and the pin column's 3-point spacing -
    // which at this grid is exactly one block. So the empty band under the last strip is the
    // same size whatever the port count: measured, 26 points from three ports to seven. At
    // the old 56-point block the same list alternated between 26 and 55 depending on whether
    // the number of rows was odd, and a 27-point row slid two points further off the block
    // with every pair of ports. The Split is measured too: its bands are shared between its
    // one input and its outputs, and its interpolate toggle sits on the first band, so it is
    // the case where the count on one side doesn't match the count on the other.
    for module in ["module.selector", "module.split"] {
        let (mut rig, ids) = Rig::real(module, &[egui::pos2(100.0, 300.0)]);
        let node = ids[0];
        let mut left = std::collections::BTreeMap::new();
        for ports in 3..=7usize {
            if module == "module.selector" {
                // A Selector keeps its `select` pin above the list: the ports are its inputs
                // less one.
                while rig.inputs(node) < ports + 1 {
                    rig.set_inputs(node, rig.inputs(node) + 1);
                }
            } else {
                while rig.outputs(node) < ports {
                    rig.set_outputs(node, rig.outputs(node) + 1);
                }
            }
            rig.idle(5);
            let f = rig.frame_rect(node);
            let (a, b) = (rig.traced(node, "strip_0"), rig.traced(node, "strip_1"));
            let last = rig.traced(node, &format!("strip_{}", ports - 1));
            let pitch = b.top() - a.top();
            assert!(
                (pitch - B).abs() < 0.5,
                "{module}, {ports} ports: a row of {pitch:.1} isn't one block ({B})",
            );
            assert!(
                (f.height().rem_euclid(pitch)).abs() < 1.5,
                "{module}, {ports} ports: a row of {pitch:.1} doesn't divide the frame's {:.1}",
                f.height(),
            );
            left.insert(ports, f.bottom() - last.bottom());
        }
        let vals: Vec<f32> = left.values().copied().collect();
        let spread = vals.iter().fold(0.0f32, |m, v| m.max((v - vals[0]).abs()));
        assert!(
            spread < 1.5,
            "{module}: the space under the last strip changes with the port count: {left:?}",
        );
    }
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
    // And the name is painted rather than laid out, so two modules with the same controls are
    // the same width whatever they're called. This one is called "DC Filter", which is three
    // letters longer than "Average" and used to cost it a whole 28-point block.
    let mut widths = Vec::new();
    for name in ["DC", "DC Filter", "DC Filter, which removes the slowly moving offset"] {
        let (mut rig, ids) = Rig::real("module.dc_filter", &[egui::pos2(100.0, 300.0)]);
        if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
            n.display_name = name.to_string();
        }
        rig.idle(6);
        let f = rig.frame_rect(ids[0]);
        widths.push(f.width());
        assert_eq!(f.width(), 5.0 * B, "{name:?}: the name decided the width ({f:?})");
        if name == "DC Filter" {
            let title = rig.traced_rect(ids[0], "header_title").unwrap_or(egui::Rect::NOTHING);
            assert!(
                title.is_positive() && f.contains_rect(title.expand(1.0)),
                "{name:?}: its name isn't drawn inside {f:?} ({title:?})",
            );
        }
    }
    assert!(
        widths.windows(2).all(|w| (w[0] - w[1]).abs() < 0.5),
        "the same module at the same settings came out different widths: {widths:?}",
    );
    // A module the user *can* widen - one with a corner grip - still takes the width it's
    // asked for, name or no name.
    let (mut rig, ids) = Rig::real("module.label", &[egui::pos2(100.0, 300.0)]);
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

#[test]
fn the_envelope_follows_the_frame_it_is_given() {
    // The curve editor used to carry a `Resize` box of its own: dragging the module's corner
    // moved a border inside the body, and the six stacked rows under it cost the same height
    // whatever the frame looked like. Now the graph is what's left after the rows, the sustain
    // slider is a row of the graph's width, and the three groups of settings fill the width
    // evenly - side by side in one band in a wide frame, wrapped into more bands in a narrow
    // one, where the module takes the height for them rather than losing the graph. The two
    // switches over how the graph is drawn live in the header, not in a row of the body.
    let (mut rig, ids) = Rig::real("generator.envelope", &[egui::pos2(100.0, 100.0)]);
    let node = ids[0];
    let measure = |rig: &mut Rig, w: f32, h: f32| -> (egui::Rect, Vec<egui::Rect>, egui::Rect, egui::Rect) {
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            n.params.insert(kit_width_param().into(), serde_json::json!(w));
            if h > 0.0 {
                n.params.insert("ui_h".into(), serde_json::json!(h));
            }
        }
        rig.idle(6);
        for what in ["body_column", "env_curve", "env_sustain", "env_time", "env_mode", "env_grid", "env_opts"] {
            rig.ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new(("module_ctrl_rect", node.0, what)), Xysr([0.0; 4]))
            });
        }
        rig.idle(4);
        let graph = rig.traced_rect(node, "env_curve").unwrap_or(egui::Rect::NOTHING);
        let sustain = rig.traced_rect(node, "env_sustain").unwrap_or(egui::Rect::NOTHING);
        let rows: Vec<egui::Rect> = ["env_time", "env_mode", "env_grid"]
            .into_iter()
            .map(|k| rig.traced_rect(node, k).unwrap_or(egui::Rect::NOTHING))
            .collect();
        (rig.frame_rect(node), rows, graph, sustain)
    };

    // Wide: one band of four groups, and the graph spanning the body with the sustain slider
    // of the same width under it.
    let (wide_frame, wide_rows, wide_graph, wide_sustain) = measure(&mut rig, 700.0, 0.0);
    let wide_column = rig
        .traced_rect(node, "body_column")
        .unwrap_or(egui::Rect::NOTHING);
    assert!(
        whole_blocks(wide_frame.width()) && whole_blocks(wide_frame.height()),
        "generator.envelope: {wide_frame:?} off the grid",
    );
    assert!(
        (wide_graph.width() - wide_column.width()).abs() < 1.0,
        "generator.envelope: the graph {wide_graph:?} doesn't span the body {wide_column:?}",
    );
    assert!(
        (wide_sustain.width() - wide_graph.width()).abs() < 1.0,
        "generator.envelope: the sustain row {wide_sustain:?} isn't the graph's width {wide_graph:?}",
    );
    assert!(
        wide_sustain.top() >= wide_graph.bottom() - 0.5,
        "generator.envelope: the sustain row {wide_sustain:?} isn't under the graph {wide_graph:?}",
    );
    let switches = rig.traced_rect(node, "env_opts").unwrap_or(egui::Rect::NOTHING);
    assert!(
        switches.is_positive() && switches.bottom() <= wide_graph.top() + 1.0,
        "generator.envelope: the graph switches {switches:?} are not in the header above {wide_graph:?}",
    );
    for r in &wide_rows {
        assert!(
            r.is_positive() && wide_frame.contains_rect(r.shrink(-0.5)),
            "generator.envelope: a setting row {r:?} isn't inside {wide_frame:?}",
        );
        assert!(
            (r.top() - wide_rows[0].top()).abs() < 1.0,
            "generator.envelope: a wide frame should hold its setting groups in one band: {wide_rows:?}",
        );
    }

    // Narrow: the groups wrap into more bands and take height off the graph, but nothing
    // leaves the frame and the graph stays something worth drawing points on.
    let (narrow_frame, narrow_rows, narrow_graph, narrow_sustain) = measure(&mut rig, 196.0, 0.0);
    for r in narrow_rows.iter().chain([narrow_graph, narrow_sustain].iter()) {
        assert!(
            r.is_positive() && narrow_frame.contains_rect(r.shrink(-0.5)),
            "generator.envelope: {r:?} is outside {narrow_frame:?} once the rows wrapped",
        );
    }
    let bands = {
        let mut tops: Vec<f32> = narrow_rows.iter().map(|r| r.top()).collect();
        tops.sort_by(|a, b| a.total_cmp(b));
        tops.dedup();
        tops.len()
    };
    assert!(
        bands > 1,
        "generator.envelope: the groups didn't wrap into more bands at {narrow_frame:?}: {narrow_rows:?}",
    );
    assert!(
        narrow_graph.height() >= 60.0,
        "generator.envelope: the graph {narrow_graph:?} was squeezed out by the rows of {narrow_frame:?}",
    );

    // A frame too short for its rows: the module takes the height it needs rather than letting
    // the graph go to nothing, which is what the old `Resize` box did.
    let (squat_frame, squat_rows, squat_graph, _s) = measure(&mut rig, 252.0, 112.0);
    assert!(
        squat_frame.height() > 112.0,
        "generator.envelope: a 112-tall frame lost its rows to the graph: {squat_frame:?}",
    );
    for r in squat_rows.iter().chain([squat_graph].iter()) {
        assert!(
            r.is_positive() && squat_frame.contains_rect(r.shrink(-0.5)),
            "generator.envelope: {r:?} is outside the {squat_frame:?} it was squeezed into",
        );
    }
    assert!(
        squat_graph.height() >= 55.5,
        "generator.envelope: the graph {squat_graph:?} vanished in {squat_frame:?}",
    );
}

#[test]
fn the_envelope_spends_the_width_it_is_given_on_its_length() {
    // A wide Envelope used to hand its extra width to the unit buttons and the mode words,
    // leaving the length - the one number being read off it - in the same small box it wears
    // at its default size. The pill takes the row's remainder, so width goes where it is read.
    let (mut rig, ids) = Rig::real("generator.envelope", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    let pill_at = |rig: &mut Rig, w: f32| -> (egui::Rect, egui::Rect) {
        rig.canvas
            .snarl
            .get_node_mut(node)
            .unwrap()
            .params
            .insert(kit_width_param().into(), serde_json::json!(w));
        rig.idle(6);
        rig.ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new(("module_ctrl_rect", node.0, "env_time_pill")),
                Xysr([0.0; 4]),
            )
        });
        rig.idle(3);
        (
            rig.frame_rect(node),
            rig.traced_rect(node, "env_time_pill").unwrap_or(egui::Rect::NOTHING),
        )
    };
    let _ = pill_at(&mut rig, 10.0 * B);
    // What the pill promises: it runs to the end of its row, so whatever width the module was
    // given lands on the number being read rather than on the two-letter unit buttons beside
    // it, and it is the wider half of the row. Where the row itself ends up between band
    // wrapping is the layout's business, not the pill's.
    // The floor is the one the row promises rather than one I can derive from the band
    // maths: the number is wider than the three unit words beside it, and it runs to the end
    // of the row it was given. How much of the band a row gets is the layout's business.
    for (w, least) in [(10.0 * B, 76.0), (14.0 * B, 76.0)] {
        let (frame, pill) = pill_at(&mut rig, w);
        let cell = rig.traced_rect(node, "env_time").unwrap_or(egui::Rect::NOTHING);
        assert!(pill.is_positive(), "no length pill drawn in {frame:?}");
        assert!(
            (pill.right() - (cell.right() - 4.0)).abs() < 1.0,
            "the length pill {pill:?} stops short of its row {cell:?}",
        );
        assert!(
            pill.width() > least,
            "the length pill has too little of its row in {frame:?}: {pill:?}",
        );
        assert!(
            frame.contains_rect(pill.expand(0.5)),
            "the length pill {pill:?} left {frame:?}",
        );
    }
}

#[test]
fn a_dropdown_row_can_be_reordered_from_its_name_not_only_the_dots() {
    // The dots at the row's end were the only place a reorder could start, so a finger that
    // landed on the word - where a finger lands - moved nothing and read as the list ignoring
    // it, while the same gesture near the dots did the move. The name is a grab too, and a
    // drag along it is not also a click: an option it passes is not chosen on the way.
    let (mut rig, ids) = Rig::real("module.dropdown", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(6);
    let row = |rig: &Rig, i: usize| -> egui::Rect {
        rig.traced_rect(node, &format!("dropdown_row_{i}")).unwrap_or(egui::Rect::NOTHING)
    };
    let (top, second) = (row(&rig, 0), row(&rig, 1));
    assert!(top.is_positive() && second.is_positive(), "the list drew no rows: {top:?} {second:?}");
    let pitch = second.top() - top.top();
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.params.insert(
            "options".into(),
            serde_json::json!(["Alpha", "Beta", "Gamma"]),
        );
    }
    rig.idle(4);
    rig.drag(top.center(), egui::vec2(0.0, 2.0 * pitch));
    let options: Vec<String> = rig
        .canvas
        .snarl
        .get_node(node)
        .and_then(|n| n.params.get("options").and_then(|v| v.as_array().cloned()))
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    assert_eq!(
        &options[..],
        &["Beta".to_string(), "Gamma".to_string(), "Alpha".to_string()],
        "the drag did not land the grabbed option where it was dragged to: {options:?}",
    );
}

#[test]
fn a_dragged_macro_port_travels_past_its_neighbour() {
    // A port could only ever trade places with the one next to it: the move was committed a
    // row at a time while the row was also being drawn offset from its slot, so the list
    // flickered under the finger and a long drag stopped short. A drag now carries the port to
    // wherever it is held and lands it when the finger leaves, pins and wires going with it.
    let (mut rig, ids) = Rig::real("module.macro", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    {
        use flexinput_core::macros as mac;
        let mk = |id: &str, name: &str, ty: flexinput_core::SignalType| mac::MacroPortDef {
            id: id.to_string(),
            name: name.to_string(),
            icon: String::new(),
            icon_svg: String::new(),
            signal_type: ty,
        };
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            n.params.insert(
                mac::MACRO_PORTS_PARAM.to_string(),
                mac::ports_to_value(&[
                    mk("aa11bb22", "Left", flexinput_core::SignalType::Bool),
                    mk("cc33dd44", "Right", flexinput_core::SignalType::Float),
                    mk("ee55ff66", "Up", flexinput_core::SignalType::Vec2),
                ]),
            );
            n.outputs = vec![
                PinDescriptor::new("Left", flexinput_core::SignalType::Bool),
                PinDescriptor::new("Right", flexinput_core::SignalType::Float),
                PinDescriptor::new("Up", flexinput_core::SignalType::Vec2),
            ];
            n.params.insert(
                "output_pin_ids".into(),
                serde_json::Value::Array(vec![
                    serde_json::Value::String("macro:aa11bb22".into()),
                    serde_json::Value::String("macro:cc33dd44".into()),
                    serde_json::Value::String("macro:ee55ff66".into()),
                ]),
            );
        }
    }
    rig.idle(8);
    let row = |rig: &Rig, i: usize| -> egui::Rect {
        rig.traced_rect(node, &format!("macro_row_{i}")).unwrap_or(egui::Rect::NOTHING)
    };
    let (top, second) = (row(&rig, 0), row(&rig, 1));
    assert!(top.is_positive() && second.is_positive(), "the port list drew no rows: {top:?} {second:?}");
    let pitch = second.top() - top.top();
    rig.drag(egui::pos2(top.left() + 12.0, top.center().y), egui::vec2(0.0, 2.5 * pitch));
    let names: Vec<String> = rig
        .canvas
        .snarl
        .get_node(node)
        .map(|n| n.outputs.iter().map(|p| p.name.clone()).collect())
        .unwrap_or_default();
    assert_eq!(
        &names[..],
        &["Right".to_string(), "Up".to_string(), "Left".to_string()],
        "the dragged port did not travel the length of the drag: {names:?}",
    );
}


#[test]
fn the_macro_ports_line_up_with_the_strips_that_name_them() {
    // The rows were a list of their own, at a pitch of the body's choosing, with the port's
    // name written twice - once in the row's field, once beside the socket. They are a strip
    // column now, like a Split's: one row per port at the pitch the pin column allocates, the
    // row naming the port so the socket needs no label, and the rows running the body's width.
    let (mut rig, ids) = Rig::real_no_snap("module.macro", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    {
        use flexinput_core::macros as mac;
        let mk = |id: &str, name: &str| mac::MacroPortDef {
            id: id.to_string(),
            name: name.to_string(),
            icon: String::new(),
            icon_svg: String::new(),
            signal_type: flexinput_core::SignalType::Float,
        };
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            n.params.insert(
                mac::MACRO_PORTS_PARAM.to_string(),
                mac::ports_to_value(&[mk("aa11bb22", "Left"), mk("cc33dd44", "Right"), mk("ee55ff66", "Up")]),
            );
            n.outputs = vec![
                PinDescriptor::new("Left", flexinput_core::SignalType::Float),
                PinDescriptor::new("Right", flexinput_core::SignalType::Float),
                PinDescriptor::new("Up", flexinput_core::SignalType::Float),
            ];
        }
    }
    rig.idle(6);
    let frame = rig.frame_rect(node);
    let rows = [
        rig.traced(node, "strip_0"),
        rig.traced(node, "strip_1"),
        rig.traced(node, "strip_2"),
    ];
    for i in 0..2 {
        let (a, b) = (rows[i], rows[i + 1]);
        let pitch = b.top() - a.top();
        assert!(
            pitch > 20.0 && pitch < 34.0,
            "Macro rows {a:?} {b:?} aren't one row apart: {pitch:.1}"
        );
        assert!(
            (a.left() - b.left()).abs() < 0.6 && (a.right() - b.right()).abs() < 0.6,
            "Macro rows aren't flush: {a:?} {b:?}"
        );
    }
    // The row runs the whole body: the socket hangs off the end of the strip that names the
    // port, instead of a label beside it holding a column of dead margin open.
    let column = rig.traced_rect(node, "body_column").unwrap_or(egui::Rect::NOTHING);
    assert!(
        column.is_positive() && (rows[2].right() - column.right()).abs() < 1.5,
        "the last Macro row {:?} stops short of the body {column:?}",
        rows[2],
    );
    assert!(
        frame.width() < 240.0,
        "a three-port Macro is {frame:?} wide: something besides its rows is holding it out"
    );
}

/// Give a Macro Output `n` ports, the way its body writes them: the param, the pins and the
/// pin ids from one list.
fn set_macro_ports(rig: &mut Rig, node: NodeId, n: usize) {
    use flexinput_core::macros as mac;
    let ports: Vec<mac::MacroPortDef> = (0..n)
        .map(|i| mac::MacroPortDef {
            id: format!("{:08x}", 0xbb00_0000u32 + i as u32),
            name: format!("P{i}"),
            icon: String::new(),
            icon_svg: String::new(),
            signal_type: flexinput_core::SignalType::Bool,
        })
        .collect();
    if let Some(nd) = rig.canvas.snarl.get_node_mut(node) {
        nd.params.insert(mac::MACRO_PORTS_PARAM.to_string(), mac::ports_to_value(&ports));
        nd.outputs = ports.iter().map(|p| PinDescriptor::new(p.name.clone(), p.signal_type)).collect();
    }
}

#[test]
fn every_port_strip_is_level_with_its_socket() {
    // The row that names a port and the socket that carries it are one row, on every module
    // that draws its ports as a list, at any length of list and with or without the grid's
    // rounding of the frame. The Macro once laid its rows at the height a pin row is
    // *allocated* and centred the list in the body, while the sockets advance by that height
    // plus the column's spacing from the body's top: by the seventh port the row and its
    // socket were most of a row apart.
    for (module, side, pin_from) in [
        ("module.selector", "in", 1usize),
        ("module.split", "out", 0),
        ("module.macro", "out", 0),
    ] {
        for snapped in [false, true] {
            for ports in [2usize, 3, 7, 10] {
                let at = [egui::pos2(100.0, 300.0)];
                let (mut rig, ids) = if snapped { Rig::real(module, &at) } else { Rig::real_no_snap(module, &at) };
                let node = ids[0];
                if module == "module.macro" {
                    set_macro_ports(&mut rig, node, ports);
                } else if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
                    let pins = if side == "in" { &mut n.inputs } else { &mut n.outputs };
                    while pins.len() < pin_from + ports {
                        pins.push(PinDescriptor::new(format!("p{}", pins.len()), SignalType::Any));
                    }
                }
                rig.idle(8);
                let frame = rig.frame_rect(node);
                for i in 0..ports {
                    let strip = rig.traced(node, &format!("strip_{i}"));
                    let socket = rig.traced(node, &format!("pin_row_{side}_{}", pin_from + i));
                    assert!(
                        (strip.center().y - socket.center().y).abs() <= 1.0,
                        "{module} ({ports} ports, snapped {snapped}): row {i} {strip:?} is not level with its socket's row {socket:?}",
                    );
                    assert!(
                        strip.top() >= frame.top() - 0.5 && strip.bottom() <= frame.bottom() + 0.5,
                        "{module} ({ports} ports, snapped {snapped}): row {i} {strip:?} is outside the frame {frame:?}",
                    );
                }
            }
        }
    }
}

#[test]
fn the_automap_splitter_and_collector_ports_sit_on_their_sockets() {
    // They were a list flowing down the body under the sockets, each socket with its own small
    // name beside it: the rows and the sockets they named drifted apart, and the name was said
    // twice. Now they are strips like a Split's: the bus pin's row holds `add pin`, each port
    // is on its own socket's row, and the socket carries no word.
    for (module, side, param_key) in [
        ("module.automap_split", "out", "output_pin_ids"),
        ("module.automap_collect", "in", "collect_input_pin_ids"),
    ] {
        for snapped in [false, true] {
            for ports in [2usize, 5] {
                let at = [egui::pos2(100.0, 300.0)];
                let (mut rig, ids) = if snapped { Rig::real(module, &at) } else { Rig::real_no_snap(module, &at) };
                let node = ids[0];
                let names = ["Left Stick", "Right Stick", "South", "East", "West"];
                let keys = ["left_stick", "right_stick", "south", "east", "west"];
                if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
                    let mut id_list: Vec<serde_json::Value> = Vec::new();
                    if side == "out" {
                        id_list.push(serde_json::Value::String("automap_pass".into()));
                    }
                    for k in 0..ports {
                        if side == "out" {
                            n.outputs.push(PinDescriptor::new(names[k], SignalType::Any));
                        } else {
                            n.inputs.push(PinDescriptor::new(names[k], SignalType::Any));
                        }
                        id_list.push(serde_json::Value::String(keys[k].into()));
                    }
                    n.params.insert(param_key.into(), serde_json::Value::Array(id_list));
                }
                rig.idle(8);
                let frame = rig.frame_rect(node);
                for i in 0..ports {
                    let strip = rig.traced(node, &format!("strip_{i}"));
                    let socket = rig.traced(node, &format!("pin_row_{side}_{}", i + 1));
                    assert!(
                        (strip.center().y - socket.center().y).abs() <= 1.0,
                        "{module} ({ports} ports, snapped {snapped}): row {i} {strip:?} is not level with its socket's row {socket:?}",
                    );
                    assert!(
                        frame.contains_rect(strip.expand(-0.5)),
                        "{module} ({ports} ports, snapped {snapped}): row {i} {strip:?} is outside the frame {frame:?}",
                    );
                }
                // No word beside the sockets: the strips say the names, once each.
                for name in ["Left Stick", "Right Stick"] {
                    let times = rig.painted_all(name).into_iter().filter(|(t, _)| t == name).count();
                    assert_eq!(times, 1, "{module}: \"{name}\" painted {times} times - on its strip and again by its socket?");
                }
            }
        }
    }
}

#[test]
fn a_macro_port_name_stays_in_the_cell_it_was_given() {
    // The name used to be a text field open in every row, and an open field lays its text out
    // at whatever width the text wants: a port called "Right Trigger, half pressed" painted
    // across the type dropdown, the `×` and the module's own edge. The cell's clamp only ever
    // bounded what the row *asked for*, never what was drawn inside it.
    let (mut rig, ids) = Rig::real("module.macro", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    {
        use flexinput_core::macros as mac;
        let mk = |id: &str, name: &str| mac::MacroPortDef {
            id: id.to_string(),
            name: name.to_string(),
            icon: String::new(),
            icon_svg: String::new(),
            signal_type: flexinput_core::SignalType::Float,
        };
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            let ports = vec![
                mk("aa11bb22", "Aim down"),
                mk("cc33dd44", "Right Trigger, half pressed, learned from the raw pad and held there for longer than anyone would"),
            ];
            n.params.insert(mac::MACRO_PORTS_PARAM.to_string(), mac::ports_to_value(&ports));
            n.outputs = ports
                .iter()
                .map(|p| PinDescriptor::new(p.name.clone(), p.signal_type))
                .collect();
        }
    }
    rig.idle(8);
    let frame = rig.frame_rect(node);
    for i in 0..2 {
        let row = rig.traced(node, &format!("strip_{i}"));
        let name = rig.traced(node, &format!("macro_name_{i}"));
        let ty = rig.traced(node, &format!("macro_type_{i}"));
        let rm = rig.traced(node, &format!("macro_remove_{i}"));
        assert!(
            name.right() <= ty.left() + 0.5,
            "port {i}'s name {name:?} runs into its type dropdown {ty:?}"
        );
        assert!(
            ty.right() <= rm.left() + 0.5,
            "port {i}'s type {ty:?} runs into its remove button {rm:?}"
        );
        assert!(
            rm.right() <= row.right() + 0.5 && rm.right() > row.right() - 8.0,
            "port {i}'s `×` {rm:?} is not at the right edge of its row {row:?}"
        );
        assert!(
            frame.contains_rect(name.expand(1.0)),
            "port {i}'s name {name:?} is outside {frame:?}"
        );
    }
    // The cells of a column line up whatever their names say: the short port's cell is the
    // same box as the long one's, not a measurement of its own word.
    let n0 = rig.traced(node, "macro_name_0");
    let n1 = rig.traced(node, "macro_name_1");
    assert!(
        (n0.left() - n1.left()).abs() < 0.6 && (n0.width() - n1.width()).abs() < 0.6,
        "the two name cells disagree: {n0:?} {n1:?}"
    );
    // And so do the words inside them. The first attempt at this put a centred `Label` in each
    // cell, which is what a left-to-right row with `Align::Center` does to whatever it is
    // handed - a short name started a dozen points right of a long one, and the list stopped
    // reading as a column.
    let (short_s, short) = rig
        .painted_text("Aim down")
        .expect("the short port's name was never painted");
    let (long_s, long) = rig
        .painted_text("Right Trigger")
        .expect("the long port's name never appeared at all");
    assert!(
        (short.left() - long.left()).abs() < 1.0,
        "the two names start at different places: {short:?} {long:?}"
    );
    assert!(
        long.right() <= n1.right() + 0.5,
        "the name {long_s:?} {long:?} reaches past its cell {n1:?}"
    );
    assert!(
        long_s.ends_with('\u{2026}'),
        "a name twice its cell's width was drawn whole rather than cut: {long_s:?}"
    );
    let _ = short_s;
}

#[test]
fn a_macro_pays_width_for_its_port_names_up_to_a_point() {
    // A Macro's rows *are* its pins, so a name the user typed is worth a block of module: the
    // list asks for the widest name it has rather than cutting at a constant cell. The other
    // half of the rule is the clamp - past a point a name is not worth an unbounded module, and
    // the list stops growing and cuts it the way every other list does.
    let widths = |names: &[&str]| -> f32 {
        let (mut rig, ids) = Rig::real("module.macro", &[egui::pos2(100.0, 300.0)]);
        let node = ids[0];
        {
            use flexinput_core::macros as mac;
            let ports: Vec<mac::MacroPortDef> = names
                .iter()
                .enumerate()
                .map(|(i, n)| mac::MacroPortDef {
                    id: format!("{:08x}", 0xaa000000 + i),
                    name: n.to_string(),
                    icon: String::new(),
                    icon_svg: String::new(),
                    signal_type: flexinput_core::SignalType::Bool,
                })
                .collect();
            if let Some(nd) = rig.canvas.snarl.get_node_mut(node) {
                nd.params.insert(mac::MACRO_PORTS_PARAM.to_string(), mac::ports_to_value(&ports));
                nd.outputs = ports
                    .iter()
                    .map(|p| PinDescriptor::new(p.name.clone(), p.signal_type))
                    .collect();
            }
        }
        rig.idle(8);
        rig.frame_rect(node).width()
    };
    let short = widths(&["On", "Off"]);
    let long = widths(&["Aim down", "Right Trigger, half pressed"]);
    let absurd = widths(&["Right Trigger, half pressed, learned from the raw pad and held a while"]);
    let more = widths(&[
        "Right Trigger, half pressed, learned from the raw pad and held a while, with a suffix",
        "and another name longer than anyone would ever call a port",
    ]);
    assert!(
        whole_blocks(short) && whole_blocks(long) && whole_blocks(absurd) && whole_blocks(more),
        "off the grid: {short}, {long}, {absurd}, {more}"
    );
    assert!(
        short <= 7.0 * B + 0.5,
        "an ordinary two-port Macro is {short} wide: its rows ask for less than that"
    );
    assert!(long > short, "a long port name didn't widen the module: {short} -> {long}");
    assert!(
        more <= absurd + 0.5,
        "a name past the clamp kept widening the module: {absurd} -> {more}"
    );
}

#[test]
fn a_macro_port_renames_from_its_word_and_is_grabbed_by_it() {
    // Two things the always-open field stood in for: a name is a grab like the dots beside it,
    // and a click on the word opens the field in place - so a finger that lands on the name
    // reorders the port instead of doing nothing, and a finger that means to rename still can.
    let (mut rig, ids) = Rig::real("module.macro", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    {
        use flexinput_core::macros as mac;
        let mk = |id: &str, name: &str| mac::MacroPortDef {
            id: id.to_string(),
            name: name.to_string(),
            icon: String::new(),
            icon_svg: String::new(),
            signal_type: flexinput_core::SignalType::Bool,
        };
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            let ports = vec![mk("aa11bb22", "Left"), mk("cc33dd44", "Right")];
            n.params.insert(mac::MACRO_PORTS_PARAM.to_string(), mac::ports_to_value(&ports));
            n.outputs = ports
                .iter()
                .map(|p| PinDescriptor::new(p.name.clone(), p.signal_type))
                .collect();
        }
    }
    rig.idle(8);
    let names = |r: &Rig| -> Vec<String> {
        r.canvas
            .snarl
            .get_node(node)
            .map(|n| n.outputs.iter().map(|d| d.name.clone()).collect())
            .unwrap_or_default()
    };
    assert_eq!(names(&rig), ["Left", "Right"], "the ports as listed");
    rig.click(rig.traced(node, "macro_name_0").center());
    rig.type_text("Kick");
    rig.idle(4);
    assert_eq!(
        names(&rig),
        ["Kick", "Right"],
        "clicking the name and typing didn't rename the port"
    );
    let up = rig.traced(node, "strip_0").center().y - rig.traced(node, "strip_1").center().y;
    rig.drag(rig.traced(node, "macro_name_1").center(), egui::vec2(0.0, up));
    assert_eq!(
        names(&rig),
        ["Right", "Kick"],
        "dragging a port by its name didn't move it"
    );
}

#[test]
fn the_envelope_graph_reads_out_under_the_pointer_with_its_labels() {
    // The Response Curve's reading: with the labels on, the pointer over the graph says when
    // that is and what level comes out then. It goes with the labels, so off means off.
    let (mut rig, ids) = Rig::real("generator.envelope", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(6);
    let graph = rig.traced(node, "env_curve");
    // The curve it ships with peaks at 30% and falls to nothing: 65% of the way along its
    // 500 ms is 325 ms, half way down the fall.
    let over = rig.to_global * egui::pos2(graph.left() + graph.width() * 0.65, graph.center().y);
    rig.frame(vec![egui::Event::PointerMoved(over)]);
    rig.idle(2);
    assert!(rig.painted_all("325 ms").is_empty(), "the reading is drawn with the labels off");
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.params.insert("show_grid_labels".into(), serde_json::Value::Bool(true));
    }
    rig.idle(3);
    let read = rig.painted_all("325 ms");
    assert_eq!(read.len(), 1, "one reading under the pointer: {read:?}");
    assert!(read[0].0.ends_with("0.50"), "it reads {:?}", read[0].0);
    assert!(
        graph.expand(0.5).contains_rect(read[0].1),
        "the reading {:?} left the graph {graph:?}",
        read[0].1
    );
}

#[test]
fn the_envelope_rows_spend_their_width_on_controls_not_planks() {
    // A setting row that spans the module is right; a control stretched to fill it is not. The
    // mode buttons used to divide their cell by three (69 points of button around a 16-point
    // glyph), the snap took whatever was left over (111 points of plank at the widest frame
    // measured here), and the sustain rail stopped 79 points short of the reading at its own
    // row's end because its width came from a guessed label width.
    let (mut rig, ids) = Rig::real("generator.envelope", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    let check = |rig: &Rig, at: &str| {
        let ctrl = B - 4.0;
        for i in 0..3 {
            let b = rig.traced(node, &format!("env_mode_{i}"));
            assert!(
                (b.width() - ctrl).abs() < 0.6 && (b.height() - ctrl).abs() < 0.6,
                "the {at} mode button {i} is {b:?}, not a {ctrl:.0}-point square"
            );
        }
        let row = rig.traced(node, "env_mode");
        let first = rig.traced(node, "env_mode_0");
        let last = rig.traced(node, "env_mode_2");
        assert!(
            first.left() >= row.left() - 0.5 && last.right() <= row.right() + 0.5,
            "the {at} mode buttons {first:?} {last:?} left their row {row:?}"
        );
        let grid = rig.traced(node, "env_grid");
        let snap = rig.traced(node, "env_snap");
        assert!(
            (snap.width() - ctrl).abs() < 0.6 && (snap.height() - ctrl).abs() < 0.6,
            "the {at} snap button is {snap:?}, not a {ctrl:.0}-point square"
        );
        assert!(
            (snap.right() - grid.right()).abs() < 0.6,
            "the {at} snap square {snap:?} isn't at the right edge of its row {grid:?}"
        );
        let hx = rig.traced(node, "env_grid_x");
        let vy = rig.traced(node, "env_grid_y");
        assert!(
            hx.right() <= vy.left() + 0.5 && vy.right() <= snap.left() + 0.5,
            "the {at} grid pills are out of order: {hx:?} {vy:?} {snap:?}"
        );
        assert!(
            hx.width() >= 40.0 && vy.width() >= 40.0,
            "the {at} grid pills were squeezed to nothing: {hx:?} {vy:?}"
        );
        let rail = rig.traced(node, "env_sustain_slider");
        let value = rig.traced(node, "env_sustain_value");
        let srow = rig.traced(node, "env_sustain");
        assert!(
            value.right() <= srow.right() + 1.0,
            "the {at} sustain reading {value:?} is past its row {srow:?}"
        );
        assert!(
            rail.right() >= value.left() - 10.0,
            "the {at} sustain rail {rail:?} stops short of its own reading {value:?}"
        );
    };
    rig.idle(6);
    check(&rig, "default frame");
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.params.insert(kit_width_param().into(), serde_json::json!(700.0f32));
    }
    rig.idle(6);
    check(&rig, "700-point frame");
}

#[test]
fn a_long_macro_list_keeps_every_rows_controls() {
    // A nine-port Macro keeps the icon, the signal type and the `×` of every row. This was
    // filed as a fault of the module - "past eight ports the bottom rows lose their controls" -
    // and it was the rig: its canvas opens at twice the size, centred on the node, on a
    // 1000-pixel screen, so only 250 graph points are visible below the node's top. The ninth
    // row was off the bottom of the screen, where egui rightly paints no widget (the handle
    // and the name are painted shapes, which egui doesn't cull, and that is the whole of why
    // they "survived"). Moved up so the whole module is on screen, nothing is lost.
    let (mut rig, ids) = Rig::real("module.macro", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.drag(rig.header(node), egui::vec2(0.0, -140.0));
    set_macro_ports(&mut rig, node, 9);
    rig.idle(8);
    let shown = rig.to_global.inverse() * egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0));
    assert!(
        shown.contains_rect(rig.frame_rect(node)),
        "the rig has to show the whole module to say anything about it: {:?} in {shown:?}",
        rig.frame_rect(node),
    );
    let f = rig.frame_rect(node);
    let crosses = rig.painted_all("×");
    assert_eq!(crosses.len(), 9, "a nine-port Macro drew {} `×`s: {crosses:?}", crosses.len());
    let lefts: Vec<f32> = crosses.iter().map(|(_, r)| r.left() - f.left()).collect();
    let spread = lefts.iter().fold(0.0f32, |m, x| m.max((x - lefts[0]).abs()));
    assert!(spread < 1.0, "the column of `×`s is a column only by accident: {lefts:?}");
    let types = rig.painted_all("Bool");
    assert_eq!(types.len(), 9, "a nine-port Macro drew {} type words: {types:?}", types.len());
}

#[test]
fn a_folded_module_keeps_its_frame_and_paints_nothing_over_its_name() {
    // Snarl runs the body of a collapsed node and shifts it up under the header's clip. The
    // bodies that draw rows paid for that: a folded Macro put its port - icon, name, type, `×` -
    // across the header's own name, a folded Oscillator put its phase number under "Oscillator",
    // and a folded Dropdown an option over "Dropdown". Anything drawn is also clicked, so the
    // handles of a folded Envelope's graph sat in the space above the frame and answered the
    // pointer there. A folded module keeps the blocks it had and draws nothing but its header.
    for (id, title) in [
        ("module.macro", "Macro Output"),
        ("generator.envelope", "Envelope"),
        ("generator.oscillator", "Oscillator"),
        ("module.selector", "Selector"),
        ("module.split", "Split"),
        ("module.dropdown", "Dropdown"),
    ] {
        let (mut rig, ids) = Rig::real(id, &[egui::pos2(100.0, 300.0)]);
        let node = ids[0];
        rig.idle(6);
        let open = rig.frame_rect(node);
        rig.canvas.snarl.open_node(node, false);
        rig.idle(6);
        let f = rig.frame_rect(node);
        assert!(
            whole_blocks(f.width()) && whole_blocks(f.height()),
            "{id} folded is off the grid: {f:?}"
        );
        // The open module's width is its rows plus the words beside its sockets. Folded, the
        // words go - they were painted in a column clipped away above the header - so a frame may
        // lose that one block and no more. It is the body's ask, reserved before the fold guard,
        // that keeps a folded module as wide as its own name instead of shrunk to its controls.
        assert!(
            open.width() - f.width() <= B + 0.5,
            "{id} folded lost more than the width of its pin labels: {open:?} -> {f:?}"
        );
        let strays = rig.painted_above(f);
        assert!(strays.is_empty(), "{id} folded painted over its own header: {strays:?}");
        let (says, at) = rig
            .painted_text(&title.chars().take(5).collect::<String>())
            .unwrap_or_else(|| panic!("{id} folded lost its name in {f:?}"));
        assert!(
            f.contains_rect(at),
            "{id} folded drew its name {says:?} at {at:?}, outside {f:?}"
        );
    }
}

/// What a body paints, read off the paint list instead of off the rects the body chose to
/// report about itself: every bit of text in the frame, where it landed, whether it crosses
/// the frame's edge, whether the clip cuts it off anyway, and whether it sits on top of
/// another word. A layout audit, not an assertion вЂ” run with `-- --ignored --nocapture`.
#[test]
#[ignore = "prints a layout audit of the Macro and Generator batch"]
fn audit_the_macro_and_generator_bodies() {
    struct Bit {
        top: f32,
        left: f32,
        right: f32,
        bottom: f32,
        text: String,
    }
    fn audit(rig: &Rig, node: NodeId, label: &str, raw: bool) {
        let f = rig.frame_rect(node);
        println!(
            "\n=== {label}: {:.0} x {:.0} ({:.1} x {:.1} blocks), frame x {:.1}..{:.1} y {:.1}..{:.1}",
            f.width(),
            f.height(),
            f.width() / B,
            f.height() / B,
            f.left(),
            f.right(),
            f.top(),
            f.bottom()
        );
        let mut bits: Vec<Bit> = Vec::new();
        let mut clips: Vec<(String, f32, f32)> = Vec::new();
        // The paint list is in screen points; the frame and the traces are in graph points.
        let back = rig.to_global.inverse();
        for cs in &rig.shapes {
            let clip = back * cs.clip_rect;
            if !raw && !clip.intersects(f) {
                continue;
            }
            if let egui::epaint::Shape::Text(t) = &cs.shape {
                let r = back * t.galley.rect.translate(t.pos.to_vec2());
                if !raw
                    && (r.right() < f.left() || r.left() > f.right() || r.bottom() < f.top() || r.top() > f.bottom())
                {
                    continue;
                }
                let text: String = t.galley.text().chars().filter(|c| *c != '\n').collect();
                if text.trim().is_empty() {
                    continue;
                }
                bits.push(Bit { top: r.top(), left: r.left(), right: r.right(), bottom: r.bottom(), text });
            }
        }
        // Anything painted wider than its own clip is cut off: on screen it reads as a word
        // that stops mid-glyph, and nothing in the layout reports it.
        for b in &bits {
            if b.right > f.right() + 0.5 || b.left < f.left() - 0.5 {
                clips.push((b.text.clone(), b.left, b.right));
            }
        }
        bits.sort_by(|a, b| {
            a.top
                .partial_cmp(&b.top)
                .unwrap()
                .then(a.left.partial_cmp(&b.left).unwrap())
        });
        // Rows: bits whose vertical extent overlaps are one line of the layout.
        let mut rows: Vec<Vec<usize>> = Vec::new();
        for (i, b) in bits.iter().enumerate() {
            let hit = rows.iter().position(|row| {
                row.iter().any(|&j| {
                    let o = &bits[j];
                    b.top < o.bottom - 0.5 && b.bottom > o.top + 0.5
                })
            });
            match hit {
                Some(r) => rows[r].push(i),
                None => rows.push(vec![i]),
            }
        }
        for row in &rows {
            let tops: Vec<String> = row
                .iter()
                .map(|&i| {
                    let b = &bits[i];
                    let outside = if b.right > f.right() + 0.5 {
                        "В«past edge"
                    } else if b.left < f.left() - 0.5 {
                        "В«before edge"
                    } else {
                        ""
                    };
                    format!(
                        "\"{}\" [{:.1}..{:.1}]{}",
                        b.text,
                        b.left - f.left(),
                        b.right - f.left(),
                        outside
                    )
                })
                .collect();
            let y = bits[row[0]].top - f.top();
            println!("  y{y:>5.1}  {}", tops.join("  "));
        }
        // Words sitting on one another.
        for (i, a) in bits.iter().enumerate() {
            for b in bits.iter().skip(i + 1) {
                if a.left < b.right - 0.5
                    && a.right > b.left + 0.5
                    && a.top < b.bottom - 0.5
                    && a.bottom > b.top + 0.5
                {
                    println!(
                        "  OVERLAP: \"{}\" [{:.1}..{:.1}] y{:.1}..{:.1}  vs  \"{}\" [{:.1}..{:.1}] y{:.1}..{:.1}",
                        a.text,
                        a.left - f.left(),
                        a.right - f.left(),
                        a.top - f.top(),
                        a.bottom - f.top(),
                        b.text,
                        b.left - f.left(),
                        b.right - f.left(),
                        b.top - f.top(),
                        b.bottom - f.top()
                    );
                }
            }
        }
        for (text, l, r) in clips {
            println!("  OUTSIDE FRAME: \"{text}\" at {l:.1}..{r:.1} against {:.1}..{:.1}", f.left(), f.right());
        }
        for what in ["body_column", "env_curve", "env_sustain", "env_time", "env_mode", "env_grid", "strip_0", "strip_1", "strip_2", "strip_8", "osc_shape", "osc_preview", "osc_freq_pill", "osc_unit", "osc_phase_pill", "osc_bipolar", "macro_row_0", "macro_row_1", "macro_row_8", "macro_type_0", "macro_type_8", "macro_remove_0", "macro_remove_8", "macro_name_8", "macro_icon_8"] {
            if let Some(r) = rig.traced_rect(node, what) {
                println!(
                    "  trace {what:<16} x {:.1}..{:.1} (frame {:.1}..{:.1})  y {:.1}..{:.1}",
                    r.left() - f.left(),
                    r.right() - f.left(),
                    0.0,
                    f.width(),
                    r.top() - f.top(),
                    r.bottom() - f.top()
                );
            }
        }
    }

    // Macro: one port, three ports, and a long name вЂ” the list is what decides the width.
    for n in [1usize, 3, 5] {
        let (mut rig, ids) = Rig::real("module.macro", &[egui::pos2(200.0, 200.0)]);
        let node = ids[0];
        {
            use flexinput_core::macros as mac;
            if let Some(nd) = rig.canvas.snarl.get_node_mut(node) {
                let mk = |i: usize| mac::MacroPortDef {
                    id: format!("{:08x}", 0xaa000000 + i),
                    name: format!("Port {i}"),
                    icon: String::new(),
                    icon_svg: String::new(),
                    signal_type: [
                        flexinput_core::SignalType::Bool,
                        flexinput_core::SignalType::Float,
                        flexinput_core::SignalType::Vec2,
                        flexinput_core::SignalType::Any,
                    ][i % 4],
                };
                let ports: Vec<_> = (0..n).map(mk).collect();
                nd.params.insert(mac::MACRO_PORTS_PARAM.to_string(), mac::ports_to_value(&ports));
                nd.outputs = ports.iter().map(|p| PinDescriptor::new(p.name.clone(), p.signal_type)).collect();
            }
        }
        rig.idle(8);
        audit(&rig, node, &format!("module.macro, {n} ports, default frame"), false);
    }
    {
        let (mut rig, ids) = Rig::real("module.macro", &[egui::pos2(200.0, 200.0)]);
        let node = ids[0];
        {
            use flexinput_core::macros as mac;
            if let Some(nd) = rig.canvas.snarl.get_node_mut(node) {
                let mk = |name: &str| mac::MacroPortDef {
                    id: format!("{:08x}", name.len()),
                    name: name.to_string(),
                    icon: String::new(),
                    icon_svg: String::new(),
                    signal_type: flexinput_core::SignalType::Float,
                };
                let ports = vec![mk("Aim down"), mk("Right Trigger, half pressed")];
                nd.params.insert(mac::MACRO_PORTS_PARAM.to_string(), mac::ports_to_value(&ports));
                nd.outputs = ports.iter().map(|p| PinDescriptor::new(p.name.clone(), p.signal_type)).collect();
            }
        }
        rig.idle(8);
        audit(&rig, node, "module.macro, a long port name", false);
    }

    // Envelope: its default frame, a wide one, a narrow one and a short one.
    let (mut rig, ids) = Rig::real("generator.envelope", &[egui::pos2(200.0, 200.0)]);
    let node = ids[0];
    audit(&rig, node, "generator.envelope, as spawned", false);
    for (w, h) in [(700.0, 0.0f32), (196.0, 0.0), (252.0, 112.0), (140.0, 0.0)] {
        if let Some(nd) = rig.canvas.snarl.get_node_mut(node) {
            nd.params.insert(kit_width_param().into(), serde_json::json!(w));
            if h > 0.0 {
                nd.params.insert("ui_h".into(), serde_json::json!(h));
            }
        }
        rig.idle(6);
        audit(&rig, node, &format!("generator.envelope, asked {w:.0} x {h:.0}"), w > 500.0);
    }

    // Oscillator, as spawned and at its narrowest.
    let (rig, ids) = Rig::real("generator.oscillator", &[egui::pos2(200.0, 200.0)]);
    let node = ids[0];
    audit(&rig, node, "generator.oscillator, as spawned", false);
    let (mut rig, ids) = Rig::real("generator.oscillator", &[egui::pos2(700.0, 200.0)]);
    let node = ids[0];
    if let Some(nd) = rig.canvas.snarl.get_node_mut(node) {
        nd.params.insert(kit_width_param().into(), serde_json::json!(112.0f32));
    }
    rig.idle(6);
    audit(&rig, node, "generator.oscillator, asked 112 wide", false);

    // A tall frame: every block the grip gives goes to the graph, and the rows stay one block.
    {
        let (mut rig, ids) = Rig::real("generator.envelope", &[egui::pos2(200.0, 200.0)]);
        let node = ids[0];
        if let Some(nd) = rig.canvas.snarl.get_node_mut(node) {
            nd.params.insert(kit_width_param().into(), serde_json::json!(420.0f32));
            nd.params.insert("ui_h".into(), serde_json::json!(560.0f32));
        }
        rig.idle(6);
        audit(&rig, node, "generator.envelope, 420 x 560", false);
    }
    // Many ports: the list is what sets the height.
    {
        let (mut rig, ids) = Rig::real("module.macro", &[egui::pos2(200.0, 200.0)]);
        let node = ids[0];
        {
            use flexinput_core::macros as mac;
            if let Some(nd) = rig.canvas.snarl.get_node_mut(node) {
                let ports: Vec<mac::MacroPortDef> = (0..9)
                    .map(|i| mac::MacroPortDef {
                        id: format!("{:08x}", 0xbb00_0000u32 + i as u32),
                        name: format!("P{i}"),
                        icon: String::new(),
                        icon_svg: String::new(),
                        signal_type: flexinput_core::SignalType::Bool,
                    })
                    .collect();
                nd.params.insert(mac::MACRO_PORTS_PARAM.to_string(), mac::ports_to_value(&ports));
                nd.outputs = ports
                    .iter()
                    .map(|p| PinDescriptor::new(p.name.clone(), p.signal_type))
                    .collect();
            }
        }
        rig.idle(8);
        audit(&rig, node, "module.macro, nine ports", false);
    }
    // Folded: the sockets come up onto the header row, which is where the `+` and the Envelope's
    // two graph switches already live.
    for id in [
        "module.macro",
        "generator.envelope",
        "generator.oscillator",
        "module.selector",
        "module.split",
        "module.dropdown",
        "module.automap_combiner",
    ] {
        let (mut rig, ids) = Rig::real(id, &[egui::pos2(200.0, 200.0)]);
        let node = ids[0];
        rig.idle(4);
        audit(&rig, node, &format!("{id}, open"), false);
        rig.canvas.snarl.open_node(node, false);
        rig.idle(6);
        audit(&rig, node, &format!("{id}, folded"), false);
    }
}

#[test]
fn no_folded_module_draws_its_body() {
    // Every module there is, folded: nothing of its body is drawn above the frame, which is
    // where snarl lays a collapsed node's body out. The guard is in `show_body`, once, for all
    // of them. With `--nocapture` this also lists what each module folds to.
    let mut strays: Vec<String> = Vec::new();
    for r in flexinput_modules::all_modules() {
        let (mut rig, ids) = Rig::build(r.descriptor, &[egui::pos2(100.0, 300.0)]);
        let node = ids[0];
        rig.idle(8);
        rig.canvas.snarl.open_node(node, false);
        rig.idle(8);
        let id = rig.canvas.snarl.get_node(node).map(|n| n.module_id.clone()).unwrap_or_default();
        let f = rig.frame_rect(node);
        println!("FOLDED {id} {:.0}x{:.0}", f.width(), f.height());
        let says: Vec<String> = rig.painted_above(f).into_iter().map(|(s, _)| s).collect();
        if !says.is_empty() {
            strays.push(format!("{id}: {}", says.join(" | ")));
        }
    }
    assert!(strays.is_empty(), "folded, these still draw their bodies:\n{}", strays.join("\n"));
}

#[test]
fn a_folded_knob_constant_and_readout_still_show_their_number() {
    // Folded, a frame on the grid is two blocks and its header one: the band under the header
    // is there and empty, and for these three the number is the reason to keep the module on
    // the canvas at all. Everything else about a folded body stays undrawn.
    for id in ["module.knob", "module.constant", "display.readout"] {
        let (mut rig, ids) = Rig::real(id, &[egui::pos2(100.0, 300.0)]);
        let node = ids[0];
        rig.idle(8);
        rig.canvas.snarl.open_node(node, false);
        rig.idle(8);
        let f = rig.frame_rect(node);
        let number = rig.traced_rect(node, "folded_reading").unwrap_or(egui::Rect::NOTHING);
        assert!(number.is_positive(), "{id}: folded, it shows no number");
        assert!(f.contains_rect(number), "{id}: the number {number:?} is outside the frame {f:?}");
        assert!(
            number.top() > f.top() + 26.0,
            "{id}: the number {number:?} is in the header's block of {f:?}"
        );
        assert!(rig.painted_above(f).is_empty(), "{id}: folded, it still draws its body");
    }
}

#[test]
fn a_port_list_sits_in_the_middle_of_its_frame_and_steps_a_block_a_port() {
    // Rows are a block each and the frame's trim is not, so the grid's rounding adds the same
    // slack under every list. Half of it goes above the rows: all of it at the bottom read as
    // a module with a row missing. And one more port is one more block, at every count - the
    // Combiner's rows had a pitch of their own and its height stepped by one block or two.
    for (id, inputs) in [
        ("module.selector", true),
        ("module.split", false),
        ("module.macro", false),
        ("module.automap_combiner", true),
    ] {
        let (mut rig, ids) = Rig::real(id, &[egui::pos2(100.0, 150.0)]);
        let node = ids[0];
        let mut last_h: Option<f32> = None;
        for n in 2..=6usize {
            if id == "module.macro" {
                set_macro_ports(&mut rig, node, n);
            } else if inputs {
                let has = rig.inputs(node);
                rig.set_inputs(node, has.max(n + usize::from(id == "module.selector")));
            } else {
                rig.set_outputs(node, n);
            }
            rig.idle(10);
            let f = rig.frame_rect(node);
            let count = if inputs { rig.inputs(node) } else { rig.outputs(node) };
            // A Selector's first input is its `select` pin, a row of its own above the strips.
            let lead = usize::from(id == "module.selector");
            let first = rig.traced(node, "strip_0").translate(egui::vec2(0.0, -28.0 * lead as f32));
            let last = rig.traced(node, &format!("strip_{}", count - 1 - lead));
            // What is under the header: its block, then the rows. The Combiner has one more
            // row under its ports, the head of its resolution tree.
            let bottom = rig
                .traced_rect(node, "combiner_resolution")
                .map_or(last.bottom(), |r| r.bottom().max(last.bottom()));
            let above = first.top() - (f.top() + 28.0);
            let below = f.bottom() - bottom;
            assert!(
                (above - below).abs() <= 7.0,
                "{id} with {count} ports: {above:.1} above its rows, {below:.1} below ({f:?})"
            );
            if let Some(prev) = last_h {
                assert!(
                    (f.height() - prev - 28.0f32).abs() < 0.5,
                    "{id}: port {count} took the frame from {prev} to {}",
                    f.height()
                );
            }
            last_h = Some(f.height());
        }
    }
}

#[test]
fn a_macro_port_name_opens_on_what_it_says() {
    // Clicking a name opens it on the text it has, selected: typing replaces it, and a key
    // that moves the cursor leaves the text there to be changed. It used to open empty, so
    // fixing one letter meant typing the name again.
    let key = |key: egui::Key| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    };
    let name_of = |rig: &Rig, node: NodeId| {
        let n = rig.canvas.snarl.get_node(node).expect("the node");
        flexinput_core::macros::ports_from_params(&n.params)[0].name.clone()
    };
    let (mut rig, ids) = Rig::real("module.macro", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    set_macro_ports(&mut rig, node, 2);
    rig.idle(8);
    // End, then a letter: the name it had, and the letter after it.
    rig.click(rig.traced(node, "macro_name_0").center());
    rig.frame(vec![key(egui::Key::End)]);
    rig.type_text("x");
    rig.frame(vec![key(egui::Key::Enter)]);
    rig.idle(4);
    assert_eq!(name_of(&rig, node), "P0x", "the open field didn't hold the name to edit");
    // Straight to typing: the selection goes, the new word stays.
    rig.click(rig.traced(node, "macro_name_0").center());
    rig.type_text("Kick");
    rig.frame(vec![key(egui::Key::Enter)]);
    rig.idle(4);
    assert_eq!(name_of(&rig, node), "Kick", "typing over an opened name didn't replace it");
    // And the module makes room for a long one while it is being typed.
    let before = rig.frame_rect(node).width();
    rig.click(rig.traced(node, "macro_name_0").center());
    rig.type_text("Right trigger, half pressed, then held");
    rig.idle(6);
    let during = rig.frame_rect(node).width();
    assert!(during > before + 27.0, "the module stayed {before} wide while a long name was typed ({during})");
}

#[test]
fn no_header_name_runs_under_the_control_beside_it() {
    // Every module with something at its header's right edge - a stepper, a `+`, switches,
    // file buttons - keeps its name out from under it: whole if it fits, cut if it nearly
    // does, gone if it doesn't. Checked for all of them rather than module by module, which
    // is how the Selector's name came to end under its `+`.
    let mut seen = 0;
    for r in flexinput_modules::all_modules() {
        let (mut rig, ids) = Rig::build(r.descriptor, &[egui::pos2(100.0, 300.0)]);
        let node = ids[0];
        rig.idle(8);
        let id = rig.canvas.snarl.get_node(node).map(|n| n.module_id.clone()).unwrap_or_default();
        let Some(ctrl) = rig.traced_rect(node, "header_ctrl").filter(|r| r.is_positive()) else { continue };
        let f = rig.frame_rect(node);
        assert!(f.expand(0.5).contains_rect(ctrl), "{id}: the header control {ctrl:?} left the frame {f:?}");
        let Some(name) = rig.traced_rect(node, "header_title").filter(|r| r.is_positive()) else { continue };
        seen += 1;
        assert!(
            name.right() <= ctrl.left() + 0.5,
            "{id}: the name {name:?} runs under the header control {ctrl:?}"
        );
    }
    assert!(seen >= 15, "only {seen} headers with a control and a name were measured");
}

#[test]
fn a_response_curve_keeps_its_switches_and_file_buttons_in_its_header() {
    // The `grid` and `labels` switches and Save / Load / Reset sit at the header's right
    // edge, and `grid` takes the lines off the graph without touching anything else.
    let (mut rig, ids) = Rig::real("module.response_curve", &[egui::pos2(100.0, 150.0)]);
    let node = ids[0];
    rig.idle(8);
    let f = rig.frame_rect(node);
    let opts = rig.traced(node, "curve_opts");
    assert!(
        // Left of the channel stepper, which has the corner.
        opts.top() < f.top() + 28.0 && (f.right() - opts.right()) < 70.0,
        "the controls {opts:?} aren't at the header's right edge of {f:?}"
    );
    let param = |rig: &Rig, k: &str| {
        rig.canvas.snarl.get_node(node).and_then(|n| n.params.get(k).and_then(|v| v.as_bool()))
    };
    assert_eq!(param(&rig, "show_grid"), None, "the grid is on until it is turned off");
    rig.click(egui::pos2(opts.left() + 8.0, opts.center().y));
    assert_eq!(param(&rig, "show_grid"), Some(false), "the grid switch didn't switch");
    // `scaled` is the other half of the same pill, `labels` a switch of its own after it.
    rig.click(egui::pos2(opts.left() + 34.0 + 8.0, opts.center().y));
    assert_eq!(param(&rig, "show_scaled_grid"), Some(true), "the scaled half didn't switch");
    rig.click(egui::pos2(opts.left() + 34.0 + 44.0 + 3.0 + 8.0, opts.center().y));
    assert_eq!(param(&rig, "show_grid_labels"), Some(true), "the labels switch didn't switch");
}

#[test]
fn a_surface_module_is_sized_by_the_modules_grip_and_gives_the_room_back() {
    // The graphs, scopes and pads: one corner grip, the module's. Whatever it is dragged to
    // is the frame, the surface inside takes what the rows leave, and dragging it back gives
    // the room back - none of them keeps a size of its own any more, in the patch or out.
    use crate::canvas::viewer::kit::{surface_spec, HEIGHT_PARAM, WIDTH_PARAM};
    let mut seen = 0;
    for r in flexinput_modules::all_modules() {
        let id = r.descriptor.id;
        if surface_spec(id).is_none() {
            continue;
        }
        seen += 1;
        let (mut rig, ids) = Rig::build(r.descriptor, &[egui::pos2(100.0, 120.0)]);
        let node = ids[0];
        rig.screen = egui::vec2(3200.0, 2800.0);
        // RWS Aim opens with its view folded - nothing for the grip's height to go to -
        // so unfold it: the surface is what this is about.
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            n.params.insert("fold_view".into(), serde_json::json!(false));
        }
        rig.idle(10);
        let opened = rig.frame_rect(node).size();
        let set = |rig: &mut Rig, w: f32, h: f32| {
            if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
                n.params.insert(WIDTH_PARAM.into(), serde_json::json!(w));
                n.params.insert(HEIGHT_PARAM.into(), serde_json::json!(h));
            }
            rig.idle(12);
        };
        // Dragged large: 26 blocks by 30. Nothing in any of them needs more than that.
        set(&mut rig, 26.0 * B, 30.0 * B);
        let large = rig.frame_rect(node).size();
        if crate::canvas::viewer::kit::keeps_aspect(id) {
            // One size, set by the width: the height is whatever keeps the shape.
            assert_eq!(large.x, 26.0 * B, "{id}: the frame didn't take the width the grip gave it");
            assert!(large.y <= 30.0 * B, "{id}: a square scope padded itself out to {large:?}");
        } else {
            assert_eq!(large, egui::vec2(26.0 * B, 30.0 * B), "{id}: the frame didn't go where the grip put it");
        }
        for _ in 0..6 {
            rig.idle(1);
            assert_eq!(rig.frame_rect(node).size(), large, "{id}: the frame doesn't hold still");
        }
        // And back to where it opened.
        set(&mut rig, opened.x, opened.y);
        let back = rig.frame_rect(node).size();
        assert!(
            back.x <= opened.x + 0.5 && back.y <= opened.y + 0.5,
            "{id}: opened at {opened:?}, came back from {large:?} to {back:?}"
        );
    }
    assert!(seen >= 12, "only {seen} surface modules were measured");
}

#[test]
fn a_scopes_channels_come_from_its_header() {
    // The `Ch + -` row under a scope's plot is the header's stepper now, as on the filters.
    let (mut rig, ids) = Rig::real("display.oscilloscope", &[egui::pos2(100.0, 150.0)]);
    let node = ids[0];
    // A scope is wider than what the rig's own screen shows to the right of where it is put,
    // and its stepper is at its right edge.
    rig.screen = egui::vec2(3200.0, 2800.0);
    rig.idle(6);
    let before = rig.inputs(node);
    let (_, plus) = stepper_points(&rig, node);
    rig.click(plus);
    assert_eq!(rig.inputs(node), before + 1, "the + added no channel");
    assert_eq!(rig.outputs(node), 0, "a scope's channel is an input and nothing else");
    let name = rig.canvas.snarl.get_node(node).map(|n| n.inputs[before].name.clone()).unwrap_or_default();
    assert_eq!(name, format!("ch{}", before + 1), "the new channel is misnamed");
    let (minus, _) = stepper_points(&rig, node);
    rig.click(minus);
    assert_eq!(rig.inputs(node), before, "the - dropped no channel");
}

#[test]
fn a_header_name_can_be_hovered_for_what_the_module_is() {
    // The name is painted, not laid out, so the slot it was given is nothing wide: the
    // tooltip - name, the module's own name if it was renamed, what it does - had nothing
    // to hover. The name's whole room answers.
    let (mut rig, ids) = Rig::real("module.average", &[egui::pos2(100.0, 300.0)]);
    let node = ids[0];
    rig.idle(6);
    let f = rig.frame_rect(node);
    let hover = rig.traced(node, "header_hover");
    assert!(hover.width() >= 20.0 && hover.height() >= 12.0, "nothing to hover: {hover:?}");
    assert!(f.expand(0.5).contains_rect(hover), "the hover area {hover:?} left the frame {f:?}");
    assert_eq!(crate::canvas::viewer::kit::shipped_name("module.average"), Some("Average"));
}

#[test]
fn an_old_automap_fork_is_turned_round_when_it_loads() {
    // `select` is the Fork's first input now; a patch saved with the bus first has the two
    // swapped on load, each keeping the wires it had.
    use egui_snarl::{InPinId, OutPinId};
    let find = |id: &str| {
        flexinput_modules::all_modules().into_iter().map(|r| r.descriptor).find(|d| d.id == id).expect("registered")
    };
    let fork_desc = find("module.automap_fork");
    assert_eq!(fork_desc.inputs[0].name, "select", "the Fork ships with select first");
    let mut snarl: Snarl<NodeData> = Snarl::new();
    let knob = snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&find("module.knob")));
    let other = snarl.insert_node(egui::pos2(0.0, 100.0), NodeData::from(&find("module.constant")));
    let mut old = NodeData::from(&fork_desc);
    old.inputs.swap(0, 1); // as it was saved: bus, then select
    let fork = snarl.insert_node(egui::pos2(200.0, 0.0), old);
    snarl.connect(OutPinId { node: other, output: 0 }, InPinId { node: fork, input: 0 });
    snarl.connect(OutPinId { node: knob, output: 0 }, InPinId { node: fork, input: 1 });
    super::migrate_loaded_snarl(&mut snarl);
    let n = snarl.get_node(fork).expect("the fork");
    assert_eq!((n.inputs[0].name.as_str(), n.inputs[1].name.as_str()), ("select", "in"));
    let wired = |snarl: &Snarl<NodeData>, input: usize| {
        snarl.in_pin(InPinId { node: fork, input }).remotes.first().map(|r| r.node)
    };
    assert_eq!(wired(&snarl, 0), Some(knob), "select lost the wire it had");
    assert_eq!(wired(&snarl, 1), Some(other), "the bus lost the wire it had");
    // Loaded again, it is left alone.
    super::migrate_loaded_snarl(&mut snarl);
    assert_eq!(wired(&snarl, 0), Some(knob), "a Fork already turned round was turned again");
}

#[test]
fn map_range_says_which_end_of_which_range() {
    use crate::canvas::viewer::kit::pin_short;
    for (name, short) in [("in_min", "in↓"), ("in_max", "in↑"), ("out_min", "out↓"), ("out_max", "out↑")] {
        assert_eq!(pin_short(name), (short.to_string(), true), "{name}");
    }
}

#[test]
fn a_body_fills_the_frame_its_header_holds() {
    // A Response Curve's header carries switches and buttons, which hold the frame wider
    // than the graph's own floor. Asked for less than that, the body used to follow the
    // number down and stand narrow inside a frame that could not: half a module of graph.
    use crate::canvas::viewer::kit::WIDTH_PARAM;
    for id in ["module.response_curve", "module.vec_response_curve", "module.remapper"] {
        let (mut rig, ids) = Rig::real(id, &[egui::pos2(100.0, 120.0)]);
        let node = ids[0];
        rig.screen = egui::vec2(3200.0, 2800.0);
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            n.params.insert(WIDTH_PARAM.into(), serde_json::json!(84.0));
        }
        rig.idle(12);
        let f = rig.frame_rect(node);
        let body = rig.traced_rect(node, "body_fill").unwrap_or(egui::Rect::NOTHING);
        assert!(body.is_positive(), "{id}: the body's width is not on record");
        // What is left of the frame once the pin columns have theirs: a block at most.
        assert!(
            f.right() - body.right() < 40.0,
            "{id}: the body ends at {} in a frame that ends at {} ({f:?})",
            body.right(),
            f.right()
        );
    }
}

#[test]
fn an_auto_map_pin_has_no_word_beside_it_except_on_a_device() {
    use crate::canvas::viewer::kit::unlabelled_bus_pin;
    use flexinput_core::SignalType;
    assert!(unlabelled_bus_pin("module.automap_response_curve", SignalType::AutoMap));
    assert!(unlabelled_bus_pin("module.remapper", SignalType::AutoMap));
    assert!(!unlabelled_bus_pin("module.remapper", SignalType::Float), "only the bus pins");
    assert!(!unlabelled_bus_pin("device.source", SignalType::AutoMap), "a device keeps its word");
    assert!(!unlabelled_bus_pin("subpatch", SignalType::AutoMap), "a sub-patch's ports are named by the user");
    // And on the canvas: the AutoMap curve draws neither "Device" nor "AutoMap".
    let (mut rig, _ids) = Rig::real("module.automap_response_curve", &[egui::pos2(100.0, 120.0)]);
    rig.idle(6);
    let words: Vec<String> = ["Device", "AutoMap"]
        .iter()
        .flat_map(|w| rig.painted_all(w))
        .map(|(s, _)| s)
        .filter(|s| s == "Device" || s == "AutoMap")
        .collect();
    assert!(words.is_empty(), "a bus pin is still labelled: {words:?}");
}

/// Pictures of modules, for looking at: `cargo test -p flexinput-ui --lib shoot_modules -- --ignored`
/// writes one PNG per case to `target/module_shots/`. Not an assertion of anything - the layout
/// tests measure rects, and this is for what a rect can't say (see [`super::shot`]). With
/// `SHOOT=<module id>[,<module id>...]` it shoots those modules as they come out of the box
/// instead of the cases below.
#[test]
#[ignore = "writes pictures, asserts nothing"]
fn shoot_modules() {
    // With the fonts the app draws with: egui's own have no arrows, grips or crosses, and a
    // picture full of boxes where those go says nothing about how the module looks.
    fn pictured(id: &'static str, at: &[egui::Pos2]) -> (Rig, Vec<NodeId>) {
        // The control font is global: back to the plain one before a new rig draws its first
        // frame, or a bold one set for the last picture names a family this rig's fonts don't
        // have yet.
        crate::canvas::viewer::simple_bodies::set_control_font(false, false, 15.0);
        let (mut rig, ids) = Rig::real(id, at);
        crate::app::fonts::setup_fonts(&rig.ctx);
        // `SHOOT_FONT=console,bold,18` draws the controls in that control font (Settings →
        // Display) - the face, the weight, the size.
        if let Ok(f) = std::env::var("SHOOT_FONT") {
            let parts: Vec<&str> = f.split(',').collect();
            let size = parts.get(2).and_then(|v| v.parse().ok()).unwrap_or(15.0);
            crate::canvas::viewer::simple_bodies::set_control_font(parts.contains(&"console"), parts.contains(&"bold"), size);
            rig.idle(2);
            crate::canvas::viewer::simple_bodies::calibrate_control_fonts(&rig.ctx);
            let mut style = (*rig.ctx.style()).clone();
            style.text_styles.insert(
                egui::TextStyle::Button,
                crate::canvas::viewer::simple_bodies::control_font_at(size.min(crate::canvas::viewer::simple_bodies::CONTROL_FONT_MAX_PX)),
            );
            rig.ctx.set_style(style);
        }
        // A screen with room under the node: the rig's own shows 500 graph points, and a
        // scope or a curve is taller than what that leaves below where it is put.
        rig.screen = egui::vec2(3200.0, 2800.0);
        rig.idle(4);
        (rig, ids)
    }
    let at = egui::pos2(100.0, 300.0);
    if let Ok(list) = std::env::var("SHOOT") {
        // `SHOOT=all` is every module there is; `SHOOT_FOLD=1` shoots them folded.
        let all: Vec<String> = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor.id.to_string()).collect();
        let wanted: Vec<String> = if list.trim() == "all" {
            all
        } else {
            list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
        };
        let fold = std::env::var("SHOOT_FOLD").is_ok();
        let mut shot_paths = Vec::new();
        for id in wanted {
            let id: &'static str = Box::leak(id.into_boxed_str());
            let (mut rig, ids) = pictured(id, &[at]);
            // `SHOOT_PARAMS={"jsm_show_knobs":true}` merges settings into the module first.
            if let Some(serde_json::Value::Object(extra)) =
                std::env::var("SHOOT_PARAMS").ok().and_then(|p| serde_json::from_str(&p).ok())
            {
                if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
                    n.params.extend(extra);
                }
            }
            // `SHOOT_INPUTS=5` / `SHOOT_OUTPUTS=5` set how many pins a list module has.
            if let Some(n) = std::env::var("SHOOT_INPUTS").ok().and_then(|n| n.parse().ok()) {
                rig.set_inputs(ids[0], n);
            }
            if let Some(n) = std::env::var("SHOOT_OUTPUTS").ok().and_then(|n| n.parse().ok()) {
                rig.set_outputs(ids[0], n);
            }
            // `SHOOT_PINS=left_stick:Left Stick,south:South` - the ports an AutoMap Splitter or
            // Collector takes off its bus, by pin id and name.
            if let (Ok(pins), true) = (
                std::env::var("SHOOT_PINS"),
                matches!(id, "module.automap_split" | "module.automap_collect"),
            ) {
                let split = id == "module.automap_split";
                if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
                    let mut id_list: Vec<serde_json::Value> = Vec::new();
                    if split {
                        id_list.push(serde_json::Value::String("automap_pass".into()));
                    }
                    for p in pins.split(',') {
                        let (key, name) = p.split_once(':').unwrap_or((p, p));
                        let desc = PinDescriptor::new(name.trim(), SignalType::Any);
                        if split { n.outputs.push(desc) } else { n.inputs.push(desc) }
                        id_list.push(serde_json::Value::String(key.trim().into()));
                    }
                    let key = if split { "output_pin_ids" } else { "collect_input_pin_ids" };
                    n.params.insert(key.into(), serde_json::Value::Array(id_list));
                }
            }
            // A 3DOF module with no Orientation reading draws the emptiest picture it can:
            // give it a pose and the outputs that go with it, so the viewport can be looked
            // at as it looks with a pad wired.
            if id == "processing.gyro_3dof" {
                if let Some(n) = rig.canvas.snarl.get_node_mut(ids[0]) {
                    // `SHOOT_POSE=yaw,pitch,roll` (degrees, right / up / right-grip-down) for
                    // another pose than the default one.
                    let pose: Vec<f32> = std::env::var("SHOOT_POSE")
                        .ok()
                        .map(|p| p.split(',').filter_map(|v| v.trim().parse().ok()).collect())
                        .unwrap_or_default();
                    let q = if let [yaw, pitch, roll] = pose[..] {
                        glam::Quat::from_rotation_z(-yaw.to_radians())
                            * glam::Quat::from_rotation_y(-pitch.to_radians())
                            * glam::Quat::from_rotation_x(roll.to_radians())
                    } else {
                        glam::Quat::from_rotation_z(35.0f32.to_radians())
                            * glam::Quat::from_rotation_y(15.0f32.to_radians())
                    };
                    let f = |s: f32| Some(flexinput_core::Signal::Float(s));
                    n.extra.last_out = vec![
                        None, f(0.42), f(-0.18), f(0.62), None,
                        Some(flexinput_core::Signal::Vec4(glam::Vec4::new(q.x, q.y, q.z, q.w))),
                    ];
                }
            }
            rig.idle(8);
            if fold {
                rig.canvas.snarl.open_node(ids[0], false);
                rig.idle(8);
            }
            let name = format!("{}{}", id.replace('.', "_"), if fold { "_folded" } else { "" });
            let path = rig.snapshot(&name, ids[0]);
            println!("{}", path.display());
            shot_paths.push(path);
        }
        // `SHOOT_SHEET=1` also packs them onto contact sheets at half size (one canvas point
        // to a pixel), for looking over the whole set.
        if std::env::var("SHOOT_SHEET").is_ok() {
            for sheet in super::shot::sheets(&shot_paths, 0.5, 1560) {
                println!("{}", sheet.display());
            }
        }
        return;
    }
    // A list of ports, seven long: the rows against their sockets.
    for (id, n) in [("module.macro", 7usize), ("module.split", 5)] {
        let (mut rig, ids) = pictured(id, &[at]);
        if id == "module.macro" {
            set_macro_ports(&mut rig, ids[0], n);
        } else {
            rig.set_outputs(ids[0], n);
        }
        rig.idle(8);
        println!("{}", rig.snapshot(&id.replace('.', "_"), ids[0]).display());
    }
    // A Dropdown set to its first option and stepped two along by its pin.
    {
        let (mut rig, ids) = pictured("module.dropdown", &[at, egui::pos2(-150.0, 300.0)]);
        let node = ids[0];
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            n.params.insert(
                "options".into(),
                serde_json::Value::Array(
                    ["Alpha", "Beta", "Gamma", "Delta"].into_iter().map(|s| serde_json::Value::String(s.into())).collect(),
                ),
            );
            n.params.insert("selected_index".into(), serde_json::Value::from(0_u64));
            n.extra.last_out = vec![Some(flexinput_core::Signal::Float(0.625)), Some(flexinput_core::Signal::Int(2))];
        }
        rig.canvas.snarl.connect(
            egui_snarl::OutPinId { node: ids[1], output: 0 },
            egui_snarl::InPinId { node, input: 0 },
        );
        rig.idle(8);
        println!("{}", rig.snapshot("module_dropdown_shifted", node).display());
    }
    // The Envelope as it ships, then with everything switched on and the pointer on its graph.
    {
        let (mut rig, ids) = pictured("generator.envelope", &[at]);
        let node = ids[0];
        rig.idle(8);
        println!("{}", rig.snapshot("generator_envelope", node).display());
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            for key in ["hold", "loop", "snap", "show_grid_labels"] {
                n.params.insert(key.into(), serde_json::Value::Bool(true));
            }
        }
        rig.idle(4);
        let graph = rig.traced(node, "env_curve");
        let over = rig.to_global * egui::pos2(graph.left() + graph.width() * 0.62, graph.center().y);
        rig.frame(vec![egui::Event::PointerMoved(over)]);
        rig.idle(3);
        println!("{}", rig.snapshot("generator_envelope_on_hover", node).display());
    }
}

// The 3DOF's mode rows answer a click in the module itself: Steering's World cell, clicked,
// makes the module a Steering one in World mode.
#[test]
fn a_3dof_mode_row_answers_a_click_in_the_module() {
    let (mut rig, ids) = Rig::real("processing.gyro_3dof", &[egui::pos2(100.0, 120.0)]);
    let node = ids[0];
    rig.screen = egui::vec2(3200.0, 2800.0);
    rig.idle(8);
    let id = egui::Id::new(("segmented", ("gyro_mode_row", "steering", node.0), 3usize));
    let cell = rig.ctx.read_response(id).expect("the Steering row's World cell was drawn").rect;
    // A response's rect is in its layer's coordinates: the canvas's graph space here.
    let at = cell.center();
    rig.click(at);
    let p = |k: &str| rig.canvas.snarl.get_node(node).and_then(|n| n.params.get(k).and_then(|v| v.as_str()).map(str::to_string));
    assert_eq!(p("family").as_deref(), Some("steering"), "the click didn't land (cell {cell:?})");
    assert_eq!(p("axis").as_deref(), Some("world"));
}

// The Area Mapper, Touch Zones and Virtual Menu bodies draw their settings as joined pills
// (`curve_rows::segmented`) where they had checkboxes and selectable words: with two of each
// on one canvas, a click on either module's pill sets that module, and only that one.
#[test]
fn the_zone_modules_pills_answer_a_click_each_in_their_own_module() {
    let cases: [(&str, &str, &str, usize, &str); 3] = [
        ("module.area_mapper", "area_shape_seg", "area_shape", 1, "rect"),
        ("module.touch_zones", "tz_mode_seg", "zone_mode", 1, "mapping"),
        ("module.menu", "vm_mode_seg", "zone_mode", 0, "ports"),
    ];
    for (module, salt, key, cell_i, want) in cases {
        let (mut rig, ids) = Rig::real(module, &[egui::pos2(100.0, 120.0), egui::pos2(1500.0, 120.0)]);
        rig.screen = egui::vec2(3200.0, 2800.0);
        rig.idle(8);
        for (k, &node) in ids.iter().enumerate() {
            let id = egui::Id::new(("segmented", (salt, node.0), cell_i));
            let cell = rig.ctx.read_response(id).unwrap_or_else(|| panic!("{module}: node {k}'s {salt} wasn't drawn")).rect;
            rig.click(cell.center());
            let p = |rig: &Rig, n: egui_snarl::NodeId| rig.canvas.snarl.get_node(n).and_then(|n| n.params.get(key).and_then(|v| v.as_str()).map(str::to_string));
            assert_eq!(p(&rig, node).as_deref(), Some(want), "{module}: node {k}'s click didn't land (cell {cell:?})");
            if k == 0 {
                assert_ne!(p(&rig, ids[1]).as_deref(), Some(want), "{module}: the click on one module set the other too");
            }
        }
    }
}

// Two of them on one canvas: each one's rows are its own. The rows' ids were the same for
// every 3DOF, so with two on the canvas a click on one went nowhere.
#[test]
fn two_3dof_modules_each_answer_their_own_mode_rows() {
    let (mut rig, ids) = Rig::real("processing.gyro_3dof", &[egui::pos2(100.0, 120.0), egui::pos2(900.0, 120.0)]);
    rig.screen = egui::vec2(3200.0, 2800.0);
    rig.idle(8);
    for &node in &ids {
        let id = egui::Id::new(("segmented", ("gyro_mode_row", "steering", node.0), 3usize));
        let resp = rig.ctx.read_response(id);
        let cell = resp.expect("this module's Steering row was drawn").rect;
        rig.click(cell.center());
        let fam = rig.canvas.snarl.get_node(node).and_then(|n| n.params.get("family").and_then(|v| v.as_str()).map(str::to_string));
        assert_eq!(fam.as_deref(), Some("steering"), "node {node:?}: the click didn't land");
    }
}

// A Two-Way curve's rows pinned on a sub-patch's layout answer there as they do on the
// overlay: the lane toggle, clicked on the layout, sets the lane of the curve inside the
// sub-patch - for the plain curve and the AutoMap one, in either pin style.
#[test]
fn a_two_way_curves_pinned_rows_answer_on_a_sub_patch_layout() {
    for module in ["module.twoway_response_curve", "module.automap_twoway_response_curve"] {
        for style in [1u8, 2] {
            let (mut rig, ids) = Rig::with_pins("subpatch", &[], &[], &[egui::pos2(100.0, 120.0)]);
            rig.screen = egui::vec2(3200.0, 2800.0);
            let host = ids[0];
            let desc = flexinput_modules::all_modules()
                .into_iter()
                .map(|r| r.descriptor)
                .find(|d| d.id == module)
                .expect("registered");
            let mut inner: Snarl<NodeData> = Snarl::new();
            // A sub-patch's nodes are numbered on their own: the curve inside has the same
            // id as a curve of the same kind on the tab canvas beside the sub-patch. Each
            // draws its rows; the pinned one's are still its own.
            inner.insert_node(egui::pos2(-500.0, 0.0), NodeData::from(&ModuleDescriptor {
                id: "test.module", display_name: "Test", category: "Test", inputs: vec![], outputs: vec![],
            }));
            let inner_id = inner.insert_node(egui::Pos2::ZERO, NodeData::from(&desc));
            let beside = rig.canvas.snarl.insert_node(egui::pos2(1400.0, 120.0), NodeData::from(&desc));
            assert_eq!(beside, inner_id, "the test needs the two ids equal");
            let mut sp = crate::canvas::node::UiSubPatch::default();
            sp.snarl = Box::new(inner);
            // The layout this was found on: the graph, and its rows packed under it.
            let pins: [(&str, [f32; 2], [f32; 2]); 8] = [
                ("curve", [8.0, 392.0], [296.0, 232.0]),
                ("lane_toggle", [8.0, 656.0], [140.0, 18.0]),
                ("scale_row", [8.0, 628.0], [296.0, 20.0]),
                ("hyst_row", [8.0, 680.0], [224.0, 20.7]),
                ("interp_row", [8.0, 728.0], [296.0, 18.0]),
                ("range_row", [152.0, 656.0], [152.0, 18.0]),
                ("grid_row", [8.0, 704.0], [296.0, 20.0]),
                ("grid_options_row", [176.0, 760.0], [128.0, 18.0]),
            ];
            for (element, pos, size) in pins {
                let pin: crate::canvas::node::ExposedModule = serde_json::from_str(&format!(
                    r#"{{"inner_node_id":{},"element_id":"{element}","pos":[{},{}],"size":[{},{}],"pin_style":{style}}}"#,
                    inner_id.0, pos[0], pos[1], size[0], size[1],
                ))
                .expect("a pin");
                sp.items.push(crate::canvas::node::LayoutItem::Module(pin));
            }
            rig.canvas.snarl.get_node_mut(host).expect("host").subpatch = Some(Box::new(sp));
            rig.idle(10);
            // egui's own warning, painted where two widgets of one pass share an id: the
            // second takes the clicks meant for the first.
            let clashes = rig.painted_all("🔥");
            assert!(clashes.is_empty(), "{module} style {style}: widgets share ids: {clashes:?}");
            let lane = |rig: &Rig| {
                rig.canvas.snarl.get_node(host)
                    .and_then(|n| n.subpatch.as_ref())
                    .and_then(|sp| sp.snarl.get_node(inner_id))
                    .and_then(|n| n.params.get("active_lane").and_then(|v| v.as_str()).map(str::to_string))
            };
            let target = if style == 2 {
                rig.ctx
                    .read_response(egui::Id::new(("segmented", ("ps2_field", inner_id.0, 0usize), 1usize)))
                    .map(|r| r.rect.center())
            } else {
                rig.painted_text("↓ Down").map(|(_, r)| r.center())
            };
            let at = target.unwrap_or_else(|| panic!("{module} style {style}: the lane toggle wasn't drawn"));
            rig.click(at);
            assert_eq!(lane(&rig).as_deref(), Some("dn"), "{module} style {style}: the click at {at:?} on the layout didn't set the lane");
            if style == 2 {
                continue;
            }
            // The Snap switch of the pinned scale row: the curve beside it has one with the
            // same node id.
            let frame = rig.frame_rect(host);
            let snap_at = rig
                .painted_all("Snap")
                .into_iter()
                .chain(rig.painted_all("snap"))
                .map(|(_, r)| r)
                .find(|r| frame.contains(r.center()))
                .unwrap_or_else(|| panic!("{module} style {style}: no Snap on the layout: {:?}", rig.painted_all("").into_iter().filter(|(_, r)| frame.contains(r.center())).collect::<Vec<_>>()))
                .center();
            let snap = |rig: &Rig, sp: bool| {
                let n = if sp {
                    rig.canvas.snarl.get_node(host).and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.snarl.get_node(inner_id))
                } else {
                    rig.canvas.snarl.get_node(beside)
                };
                n.and_then(|n| n.params.get("snap").and_then(|v| v.as_bool())).unwrap_or(false)
            };
            rig.click(snap_at);
            assert!(snap(&rig, true), "{module} style {style}: the pinned Snap at {snap_at:?} didn't answer");
            assert!(!snap(&rig, false), "{module} style {style}: the pinned Snap switched the curve beside the sub-patch");
        }
    }
}

// The Combiner's Resolution tree, open on a category, at a large control font: the module
// keeps its width. Its rows put their dropdowns against the edge the port strips end at,
// and that edge followed the frame - a dropdown a little wider than it said (its text past
// what fits at the font) ended past it, the frame grew to hold it, the edge moved out with
// the frame, and the row went past it again, every frame, without end.
#[test]
fn the_combiners_resolution_tree_keeps_the_modules_width() {
    // The dropdowns' text is the Button style, which follows the control font's size: set
    // here on this rig's own context - the control font itself is the whole process's, and
    // changing it would change it under every other test running beside this one.
    for size in [15.0f32, 24.0] {
        let (mut rig, ids) = Rig::real("module.automap_combiner", &[egui::pos2(100.0, 150.0)]);
        crate::app::fonts::setup_fonts(&rig.ctx);
        rig.idle(2);
        let mut style = (*rig.ctx.style()).clone();
        style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(size));
        rig.ctx.set_style(style);
        rig.screen = egui::vec2(3200.0, 2800.0);
        let node = ids[0];
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            n.params.insert(
                "combiner_pin_policy".into(),
                serde_json::json!({ "btn_south": "OR", "left_stick": "ADD", "gyro_x": "ADD", "mouse_move": "ADD" }),
            );
        }
        rig.idle(8);
        let head = rig.traced(node, "combiner_resolution");
        rig.click(head.center());
        for cat in ["Sticks", "Buttons", "Motion", "KB/M"] {
            let at = rig
                .painted_text(cat)
                .unwrap_or_else(|| panic!("at {size}: no {cat} under Resolution"))
                .1
                .center();
            rig.click(at);
        }
        rig.idle(6);
        let settled = rig.frame_rect(node).width();
        rig.idle(30);
        let later = rig.frame_rect(node).width();
        assert!(
            (later - settled).abs() < 0.5,
            "at {size}: the Combiner went from {settled} wide to {later} in thirty frames",
        );
    }
}

// The SVG module is a picture its corner grip sizes, down to its header's controls: it
// wouldn't come smaller than where it opened, and its name was laid out beside the buttons,
// so a renamed module was as wide as its name. The name is painted in the room the controls
// leave, cut with dots, as every other module's.
#[test]
fn the_svg_module_shrinks_to_its_controls_and_cuts_its_name() {
    let (mut rig, ids) = Rig::real("module.svg", &[egui::pos2(100.0, 150.0)]);
    let node = ids[0];
    if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
        n.params.insert(
            "svg_data".into(),
            serde_json::json!(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 20"><rect width="10" height="20" fill="#fff"/></svg>"##),
        );
        n.display_name = "A picture with a long name nobody would cut by hand".into();
    }
    rig.idle(8);
    let open = rig.frame_rect(node);
    rig.drag(open.max - egui::vec2(3.0, 3.0), egui::vec2(-600.0, -600.0));
    rig.idle(4);
    let small = rig.frame_rect(node);
    let ctrl = rig.traced(node, "header_ctrl");
    assert!(small.width() < open.width(), "the grip didn't narrow it: {open:?} -> {small:?}");
    assert!(small.height() < open.height(), "the grip didn't shorten it: {open:?} -> {small:?}");
    assert!(
        small.width() <= ctrl.width() + 4.0 * B,
        "at its narrowest it is {} wide for {} of controls",
        small.width(),
        ctrl.width(),
    );
    assert!(small.contains_rect(ctrl), "the controls {ctrl:?} are outside the frame {small:?}");
    // The name didn't hold it open: what is painted of it ends before the controls.
    if let Some((t, r)) = rig.painted_text("A picture") {
        assert!(r.right() <= ctrl.left() + 1.0, "the name {t:?} at {r:?} runs under the controls {ctrl:?}");
    }
}

#[cfg(test)]
fn midi_rig(source: bool, automap_first: bool) -> (Rig, NodeId) {
    let (mut rig, _) = Rig::new(&[]);
    let mut pins = vec![
        PinDescriptor::new("CC 7", SignalType::Float),
        PinDescriptor::new("Note C4", SignalType::Bool),
        PinDescriptor::new("Pitch bend", SignalType::Float),
    ];
    let mut ids = vec![
        serde_json::json!("midi:cc:1:7"),
        serde_json::json!("midi:note:1:60"),
        serde_json::json!("midi:pb:1"),
    ];
    let am = PinDescriptor::new("Auto-Map", SignalType::AutoMap);
    let am_id = serde_json::json!(if source { "automap_out" } else { "automap_in" });
    if automap_first {
        pins.insert(0, am);
        ids.insert(0, am_id);
    } else {
        pins.push(am);
        ids.push(am_id);
    }
    let mut params = std::collections::HashMap::new();
    params.insert("device_id".to_string(), serde_json::json!(if source { "midi_in:0" } else { "midi_out:0" }));
    params.insert(if source { "output_pin_ids" } else { "input_pin_ids" }.to_string(), serde_json::Value::Array(ids));
    let node = NodeData {
        module_id: if source { "device.source" } else { "device.sink" }.to_string(),
        display_name: if source { "MIDI In" } else { "MIDI Out" }.to_string(),
        category: "Device".to_string(),
        inputs: if source { vec![] } else { pins.clone() },
        outputs: if source { pins } else { vec![] },
        params,
        subpatch: None,
        extra: Default::default(),
    };
    let id = rig.canvas.snarl.insert_node(egui::pos2(100.0, 150.0), node);
    rig.idle(8);
    (rig, id)
}

// A MIDI port's messages are strips on their sockets' rows, as the AutoMap Collector's
// ports are, with no second name beside the sockets: on a node made since (its Auto-Map port
// first) and on one migrated (last).
#[test]
fn a_midi_ports_messages_sit_on_their_sockets() {
    for source in [true, false] {
        for am_first in [true, false] {
            let (rig, id) = midi_rig(source, am_first);
            let side = if source { "out" } else { "in" };
            for k in 0..3 {
                let strip = rig.traced(id, &format!("strip_{k}"));
                let pin = if am_first { k + 1 } else { k };
                let socket = rig.traced(id, &format!("pin_row_{side}_{pin}"));
                assert!(
                    (strip.center().y - socket.center().y).abs() <= 1.0,
                    "MIDI {} (Auto-Map first {am_first}): message {k} {strip:?} is off its socket {socket:?}",
                    if source { "In" } else { "Out" },
                );
            }
            let times = rig.painted_all("CC 7").len();
            assert_eq!(times, 1, "the message's name is painted {times} times - on its strip and by its socket?");
        }
    }
}

// Dragging the Log/Exp slider moves a curve's points with the new scale (so the response
// they stand for holds): on a Two-way curve's lanes as on a Response Curve's.
#[test]
fn the_log_exp_slider_rescales_every_curves_points() {
    for module in ["module.response_curve", "module.twoway_response_curve", "module.automap_twoway_response_curve"] {
        let (mut rig, ids) = Rig::real(module, &[egui::pos2(100.0, 150.0)]);
        rig.screen = egui::vec2(3200.0, 2800.0);
        let node = ids[0];
        if let Some(n) = rig.canvas.snarl.get_node_mut(node) {
            for k in ["points", "points_dn"] {
                n.params.insert(k.into(), serde_json::json!([[0.0, 0.0], [0.5, 0.2], [1.0, 1.0]]));
            }
            n.params.insert("biases".into(), serde_json::json!([0.0, 0.0]));
            n.params.insert("biases_dn".into(), serde_json::json!([0.0, 0.0]));
            n.params.insert("scale_t".into(), serde_json::json!(0.0));
        }
        rig.idle(8);
        let slider = rig
            .ctx
            .memory(|_| ())
            .eq(&())
            .then(|| {
                // The slider's knob is in the middle of its track at 0.
                let (_, log) = rig.painted_text("Log").expect("Log");
                let (_, exp) = rig.painted_text("Exp").expect("Exp");
                egui::pos2((log.right() + exp.left()) * 0.5, log.center().y)
            })
            .unwrap();
        rig.drag(slider, egui::vec2(40.0, 0.0));
        let n = rig.canvas.snarl.get_node(node).unwrap();
        let t = n.params.get("scale_t").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!(t.abs() > 0.05, "{module}: the drag didn't move the slider ({t})");
        for k in ["points", "points_dn"] {
            if module == "module.response_curve" && k == "points_dn" {
                continue;
            }
            let x = n.params[k][1][0].as_f64().unwrap();
            assert!((x - 0.5).abs() > 0.01, "{module}: {k}'s middle point stayed at {x} under scale {t}");
        }
    }
}

// 3DOF to 2D's Lean lists each scroll in a window of their own: with Lean Left's height
// set, more cards in it scroll there, and neither the module nor Lean Right under it moves.
// The two lists shared one scroll, and the first one's cards pushed the second out of view.
#[test]
fn a_lean_lists_cards_scroll_in_their_own_window() {
    let mut heights = Vec::new();
    for n in [2usize, 8] {
        let (mut rig, ids) = Rig::real("processing.gyro_3dof", &[egui::pos2(100.0, 120.0)]);
        rig.screen = egui::vec2(3200.0, 2800.0);
        let node = ids[0];
        if let Some(nd) = rig.canvas.snarl.get_node_mut(node) {
            let cards: Vec<serde_json::Value> = (0..n).map(|k| serde_json::json!({ "out": [format!("key_{}", (b'a' + k as u8) as char)] })).collect();
            nd.params.insert("lean_left".into(), serde_json::Value::Array(cards));
            nd.params.insert("lean_right".into(), serde_json::json!([{ "out": ["key_z"] }]));
            nd.params.insert("lean_l_h".into(), serde_json::json!(170.0));
        }
        rig.idle(12);
        let frame = rig.frame_rect(node);
        let (_, right) = rig.painted_text("Lean Right").expect("Lean Right's title is drawn");
        assert!(frame.contains_rect(right), "{n} cards: Lean Right {right:?} is out of the frame {frame:?}");
        heights.push((frame.height(), right.top()));
    }
    assert!(
        (heights[0].0 - heights[1].0).abs() < 0.5 && (heights[0].1 - heights[1].1).abs() < 0.5,
        "more cards in Lean Left moved the module or Lean Right: {heights:?}",
    );
}
