use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![reg::<AreaMapperModule>()]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

// ── Area Mapper ───────────────────────────────────────────────────────────────
//
// One XY pair of an AutoMap bus (a stick or a touch point, picked in the
// header) laid onto cells: concentric rings cut into sectors (circle, the
// default) or rows cut into columns (rectangle). Each cell carries mapping
// cards (the shared Remapper card schema, keyed by cell id) triggered while
// the point is inside it, as it enters, or as it leaves. A border may be a
// gradient: the cells on either side crossfade across it, driving analog
// outputs proportionally and keys through the border's own PWM / tap train /
// threshold settings.
//
// Persisted params (all optional; ids are save-format):
//   area_input        — "left_stick" | "right_stick" | "touch1" | "touch2"
//   area_layout       — flexinput_core::area::AreaLayout::to_value
//   area_sym          — symmetric (quarter-mirror) editing, default on
//   area_pass_source  — keep the picked pair on the passthrough bus
//   area_touch_cells  — cell ids that only count while the input is touched
//   zone_maps         — mapping cards { z: cell id, in: [trigger], out, … }
//   zone_meta         — per-cell label / icon
//
// process() returns empty — the evaluator lives in the engine
// (`eval_area_mapper_node`), republishing the bus under `collector:{uid}`.
#[derive(Default)]
pub struct AreaMapperModule;

impl Module for AreaMapperModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.area_mapper",
            display_name: "Area Mapper",
            category: "AutoMap",
            inputs: vec![PinDescriptor::new("Device", SignalType::AutoMap)],
            outputs: vec![PinDescriptor::new("AutoMap", SignalType::AutoMap)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}
