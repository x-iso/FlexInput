use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, Signal};
use smallvec::SmallVec;

// ── Macro Output ──────────────────────────────────────────────────────────────
//
// User-defined named output ports (Bool / Float / Vec2 / Any, each with an
// icon) that mappings elsewhere in the patch target BY ID — Remapper output
// chords, Touch Zones zone cards, and 3DOF-Lean mappings all offer the
// defined macros in their shared KB/M picker, with no wire between the
// mapping module and this node.
//
// The descriptor is intentionally empty: there is no input (macros are fed by
// the mapping layer, not by wires) and no static output. The node body
// (`show_macro_body` in the UI crate) appends one typed output pin per
// defined port; ids are tracked in `output_pin_ids` (`macro:{id}`), schema
// shared via `flexinput_core::macros`.
//
// State lives in `node.params` (persists with the patch):
//   macro_ports:    [{ id, name, icon, type }]  — port definitions
//   output_pin_ids: ["macro:{id}", …]           — one per output slot
//
// process() returns empty — port values are produced in eval.rs
// (`module.macro`) from the per-tick macro signal namespace that mapping
// evaluators publish into, matching the Touch Zones ports-mode pattern.
#[derive(Default)]
pub struct MacroModule;

impl Module for MacroModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.macro",
            display_name: "Macro Output",
            category: "Utility",
            inputs: vec![],
            outputs: vec![],
        }
    }

    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        SmallVec::new()
    }
}

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![ModuleRegistration {
        descriptor: MacroModule::descriptor(),
        factory: || Box::new(MacroModule),
    }]
}
