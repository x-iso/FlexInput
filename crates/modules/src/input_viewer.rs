use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![reg::<InputViewerModule>()]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

// ── Input Viewer ──────────────────────────────────────────────────────────────
//
// Live controller visualization: a schematic board of the wired device's
// buttons / sticks / triggers lighting up from live signals. Designed for the
// info overlay (the body is pinnable as a whole-container element) — the 2D
// precursor of the 3D controller view.
//
// Single AutoMap in, AutoMap passthrough out at slot 0 so the device keeps
// flowing downstream (same convention as Touch Zones / Splitter). The board
// renders entirely from `live_signals` on the UI thread; eval publishes
// nothing (the passthrough is resolved by `find_automap_device_rec`).
//
// State in `node.params`:
//   skin: "auto" | "xbox" | "playstation" | "switchpro" — glyph set override
#[derive(Default)]
pub struct InputViewerModule;

impl Module for InputViewerModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.input_viewer",
            display_name: "Input Viewer",
            category: "AutoMap",
            inputs: vec![PinDescriptor::new("Device", SignalType::AutoMap)],
            outputs: vec![PinDescriptor::new("AutoMap", SignalType::AutoMap)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}
