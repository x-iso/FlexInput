use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![reg::<TouchZonesModule>()]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

// ── Touch Zones ────────────────────────────────────────────────────────────────
//
// Divides a controller touchpad into arbitrary rectangular zones (draggable V/H
// dividers) and treats each zone as its own mini-pad. Single AutoMap in, with an
// AutoMap passthrough out at slot 0 so the touch source keeps flowing downstream.
//
// `zone_mode` param selects the output stage:
//   "ports"   — dynamic typed outputs per zone (X / Y / Active) for wiring into
//               patch logic. Pins appended to `outputs` from the node body; ids
//               tracked in `output_pin_ids` (slot 0 = "automap_pass"), parsed via
//               `flexinput_core::touchzones`.
//   "automap" — (future) inject mapped pins back onto the AutoMap out, Remapper-
//               style. Not built in this first slice.
//
// State lives in `node.params` (persists with the patch):
//   zone_mode:  "ports" | "automap"
//   col_edges:  [f32]   interior vertical divider positions, sorted, (0,1)
//   row_edges:  [f32]   interior horizontal divider positions, sorted, (0,1)
//   output_pin_ids: ["automap_pass", "z0_x", "z0_y", "z0_act", "z1_x", ...]
//
// process() returns empty — per-zone values are produced in eval.rs
// (`module.touch_zones`), matching the AutoMap Splitter pattern.
#[derive(Default)]
pub struct TouchZonesModule;

impl Module for TouchZonesModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.touch_zones",
            display_name: "Touch Zones",
            category: "AutoMap",
            inputs: vec![PinDescriptor::new("Device", SignalType::AutoMap)],
            outputs: vec![PinDescriptor::new("AutoMap", SignalType::AutoMap)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}
