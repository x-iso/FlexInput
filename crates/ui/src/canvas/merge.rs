//! A wire dropped onto an input that already has one: rather than a second wire the engine
//! ignores (or, on an Auto-Map port, the old one silently dropped), the canvas asks what to
//! do - leave it, replace the old wire, or merge the two through a module that combines them
//! in a way that means something for their signal. The merging module goes beside the input,
//! where there's room for it.

use egui_snarl::{InPinId, NodeId, OutPinId, Snarl};
use flexinput_core::{ModuleDescriptor, SignalType};

use super::node::NodeData;

/// A wire dropped onto an input that has one, waiting on what to do with the two: the new
/// wire's output, the input, where it was dropped (screen), and - when it is a wire being
/// moved off another input - that input, which it leaves once it lands.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingMerge {
    pub new: OutPinId,
    pub target: InPinId,
    pub pos: egui::Pos2,
    pub moved_from: Option<InPinId>,
}

/// What a wire dropped from `from` onto `to` does: connect, where the input is free; ask what
/// to do, where it has a wire from somewhere else; nothing, where it's already this wire. The
/// wire being moved, when its output end is the one carried (`moving`), is the one on the
/// input, not another.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Drop {
    Connect,
    Ask,
    Nothing,
}

pub(crate) fn on_drop(snarl: &Snarl<NodeData>, from: OutPinId, to: InPinId, moving: Option<OutPinId>) -> Drop {
    let remotes = &snarl.in_pin(to).remotes;
    if remotes.contains(&from) {
        Drop::Nothing
    } else if remotes.iter().all(|r| Some(*r) == moving) {
        Drop::Connect
    } else {
        Drop::Ask
    }
}

/// One way to merge two wires: the module, the inputs the old and the new wire go into, and
/// the output that goes on to the input they both wanted.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MergeWay {
    pub module: &'static str,
    pub label: &'static str,
    pub hover: &'static str,
    pub inputs: (usize, usize),
    pub output: usize,
}

const NUMBER_WAYS: &[MergeWay] = &[
    MergeWay { module: "math.add", label: "Add", hover: "The two added together.", inputs: (0, 1), output: 0 },
    MergeWay { module: "math.min_max", label: "Max", hover: "Whichever is larger.", inputs: (0, 1), output: 0 },
    MergeWay { module: "math.min_max", label: "Min", hover: "Whichever is smaller.", inputs: (0, 1), output: 1 },
    MergeWay { module: "math.multiply", label: "Multiply", hover: "One scaled by the other.", inputs: (0, 1), output: 0 },
    MergeWay {
        module: "module.selector",
        label: "Selector",
        hover: "One or the other, picked by the Selector's select pin (or its own switch).",
        inputs: (1, 2),
        output: 0,
    },
];

const BOOL_WAYS: &[MergeWay] = &[
    MergeWay { module: "logic.or", label: "Either (OR)", hover: "On while either is on.", inputs: (0, 1), output: 0 },
    MergeWay { module: "logic.and", label: "Both (AND)", hover: "On while both are on.", inputs: (0, 1), output: 0 },
    MergeWay { module: "logic.xor", label: "One of them (XOR)", hover: "On while exactly one is on.", inputs: (0, 1), output: 0 },
    MergeWay {
        module: "module.selector",
        label: "Selector",
        hover: "One or the other, picked by the Selector's select pin (or its own switch).",
        inputs: (1, 2),
        output: 0,
    },
];

const BUS_WAYS: &[MergeWay] = &[
    MergeWay {
        module: "module.automap_combiner",
        label: "AutoMap Combiner",
        hover: "Both buses as one: each signal from whichever carries it.",
        inputs: (0, 1),
        output: 0,
    },
    MergeWay {
        module: "module.automap_selector",
        label: "AutoMap Selector",
        hover: "One bus or the other, picked by the selector's select pin.",
        inputs: (1, 2),
        output: 0,
    },
];

/// The ways two wires can merge into an input of type `input`, the wires carrying `old` and
/// `new` (an `Any` input takes the kind of what it's sent).
pub(crate) fn merge_ways(input: SignalType, old: Option<SignalType>, new: Option<SignalType>) -> &'static [MergeWay] {
    let kinds = [input, old.unwrap_or(SignalType::Any), new.unwrap_or(SignalType::Any)];
    if kinds.contains(&SignalType::AutoMap) {
        BUS_WAYS
    } else if kinds.contains(&SignalType::Bool) && !kinds.iter().any(|k| matches!(k, SignalType::Float | SignalType::Vec2 | SignalType::Int)) {
        BOOL_WAYS
    } else {
        NUMBER_WAYS
    }
}

/// Merge the wire from `new` into `target`, which already has one from `old`, through `way`:
/// the old wire into the module's first input, the new into its second, the module's output
/// into `target`. With no `new` wire - one drawn out of the input to somewhere empty - the
/// second input is left free for whatever comes next. The module goes at `pos`. Returns the
/// new node.
pub(crate) fn merge_wires(
    snarl: &mut Snarl<NodeData>,
    desc: &ModuleDescriptor,
    way: &MergeWay,
    old: OutPinId,
    new: Option<OutPinId>,
    target: InPinId,
    pos: egui::Pos2,
) -> NodeId {
    snarl.disconnect(old, target);
    let id = snarl.insert_node_placed(pos, NodeData::from(desc));
    snarl.connect(old, InPinId { node: id, input: way.inputs.0 });
    if let Some(new) = new {
        snarl.connect(new, InPinId { node: id, input: way.inputs.1 });
    }
    snarl.connect(OutPinId { node: id, output: way.output }, target);
    id
}

/// Where a module about `size` big goes beside `target`'s frame so its first input lands near
/// `pin_y`: to its left, level with the input, else up or down from there a block at a time,
/// else above or below the target - the first place no other frame (in `frames`, a block's
/// gap kept round each) is in. Left of the target, level, when nowhere is free: placing it
/// then moves what's in the way, as any new module does. A frame's top-left.
pub(crate) fn merge_spot(target: egui::Rect, pin_y: f32, size: egui::Vec2, frames: &[egui::Rect], block: f32) -> egui::Pos2 {
    let gap = 2.0 * block;
    let left = egui::pos2(target.left() - gap - size.x, pin_y - size.y * 0.5);
    let free = |at: egui::Pos2| {
        let r = egui::Rect::from_min_size(at, size).expand(block * 0.5);
        !r.intersects(target) && frames.iter().all(|f| !r.intersects(*f))
    };
    let mut tries = vec![left];
    for k in 1..=6 {
        let d = k as f32 * block;
        tries.push(left + egui::vec2(0.0, d));
        tries.push(left - egui::vec2(0.0, d));
    }
    tries.push(egui::pos2(target.left(), target.top() - gap - size.y));
    tries.push(egui::pos2(target.left(), target.bottom() + gap));
    tries.into_iter().find(|&at| free(at)).unwrap_or(left)
}

/// About how big a module comes out before it has been drawn, for finding it room.
pub(crate) fn merge_size_guess(module: &str, block: f32) -> egui::Vec2 {
    match module {
        "module.selector" | "module.automap_selector" | "module.automap_combiner" => egui::vec2(5.0, 4.0) * block,
        _ => egui::vec2(4.0, 3.0) * block,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A drop onto a free input connects; onto one with another wire it asks, every type of
    // input alike (an Auto-Map port used to drop its old wire, others stacked a second one
    // the engine never reads); onto the same wire again, nothing.
    #[test]
    fn a_drop_asks_only_where_the_input_is_taken() {
        let all: Vec<ModuleDescriptor> = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor).collect();
        let desc = |id: &str| all.iter().find(|d| d.id == id).expect("registered");
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let a = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(desc("module.knob")));
        let b = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(desc("module.knob")));
        let r = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(desc("display.readout")));
        let (ao, bo, ri) = (OutPinId { node: a, output: 0 }, OutPinId { node: b, output: 0 }, InPinId { node: r, input: 0 });
        assert_eq!(on_drop(&snarl, ao, ri, None), Drop::Connect);
        snarl.connect(ao, ri);
        assert_eq!(on_drop(&snarl, ao, ri, None), Drop::Nothing);
        assert_eq!(on_drop(&snarl, bo, ri, None), Drop::Ask);
        // The wire on the input picked up by its output end and put on another output: a
        // move, not a second wire.
        assert_eq!(on_drop(&snarl, bo, ri, Some(ao)), Drop::Connect);
    }

    #[test]
    fn the_ways_fit_what_the_wires_carry() {
        assert_eq!(merge_ways(SignalType::AutoMap, None, None)[0].module, "module.automap_combiner");
        assert_eq!(merge_ways(SignalType::Bool, Some(SignalType::Bool), Some(SignalType::Bool))[0].module, "logic.or");
        assert_eq!(merge_ways(SignalType::Any, Some(SignalType::Float), Some(SignalType::Bool))[0].module, "math.add");
        assert_eq!(merge_ways(SignalType::Any, Some(SignalType::Bool), Some(SignalType::Bool))[0].module, "logic.or");
        // A selector takes the two wires on its signal inputs, never on select.
        for ways in [NUMBER_WAYS, BOOL_WAYS, BUS_WAYS] {
            for w in ways.iter().filter(|w| w.module.ends_with("selector")) {
                assert_eq!(w.inputs, (1, 2), "{}", w.module);
            }
        }
    }

    // Every way names a module that exists, with the inputs and output it says.
    #[test]
    fn every_way_is_a_real_module_with_those_pins() {
        let all: Vec<ModuleDescriptor> = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor).collect();
        for w in NUMBER_WAYS.iter().chain(BOOL_WAYS).chain(BUS_WAYS) {
            let d = all.iter().find(|d| d.id == w.module).unwrap_or_else(|| panic!("{} isn't a module", w.module));
            assert!(w.inputs.0 < d.inputs.len() && w.inputs.1 < d.inputs.len(), "{}", w.module);
            assert!(w.output < d.outputs.len(), "{}", w.module);
            if w.module.ends_with("selector") {
                assert_eq!(d.inputs[0].name, "select", "{}: select stays free", w.module);
            }
        }
    }

    // The module goes left of the input where that's free, and steps aside of a module that's
    // in the way rather than landing on it.
    #[test]
    fn the_merge_module_finds_room_beside_the_input() {
        let target = egui::Rect::from_min_size(egui::pos2(400.0, 200.0), egui::vec2(112.0, 112.0));
        let size = egui::vec2(112.0, 84.0);
        let at = merge_spot(target, 240.0, size, &[], 28.0);
        assert_eq!(at, egui::pos2(400.0 - 56.0 - 112.0, 240.0 - 42.0));
        let blocker = egui::Rect::from_min_size(egui::pos2(200.0, 190.0), egui::vec2(112.0, 84.0));
        let at = merge_spot(target, 240.0, size, &[blocker], 28.0);
        let placed = egui::Rect::from_min_size(at, size);
        assert!(!placed.intersects(blocker) && !placed.intersects(target), "{placed:?}");
        assert!(placed.right() <= target.left(), "still on the input's side: {placed:?}");
    }
}
