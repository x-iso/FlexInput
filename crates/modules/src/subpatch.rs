use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        reg::<SubPatchModule>(),
        reg::<SubPatchInlet>(),
        reg::<SubPatchOutlet>(),
    ]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

// ── Sub-patch meta-module ─────────────────────────────────────────────────────

/// Placeholder descriptor for sub-patch nodes. Actual pins are dynamic,
/// managed by NodeData.subpatch.pins_in/pins_out and kept in sync by the editor.
#[derive(Default)]
pub struct SubPatchModule;

impl Module for SubPatchModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "subpatch",
            display_name: "Sub-patch",
            category: "Utility",
            inputs: vec![],
            outputs: vec![],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // Evaluated by inline_subgraph in eval.rs; this path is never called.
        SmallVec::new()
    }
}

// ── Inlet ─────────────────────────────────────────────────────────────────────

/// Represents one external input to a sub-patch. Produces the injected signal.
/// `params["pin_index"]` = which input pin of the outer meta-module this is.
#[derive(Default)]
pub struct SubPatchInlet;

impl Module for SubPatchInlet {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "subpatch.inlet",
            display_name: "Inlet",
            category: "SubPatch",
            inputs: vec![],
            outputs: vec![PinDescriptor::new("out", SignalType::Any)],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // Signal is injected by eval_subgraph; this path is never called during normal eval.
        SmallVec::new()
    }
}

// ── Outlet ────────────────────────────────────────────────────────────────────

/// Represents one external output from a sub-patch. Pass-through of its single input.
/// `params["pin_index"]` = which output pin of the outer meta-module this drives.
/// No output pin *inside* the sub-patch: the value is forwarded directly to the
/// outer meta-module's output via `InlineSubgraph::outlet_locs` during eval.
#[derive(Default)]
pub struct SubPatchOutlet;

impl Module for SubPatchOutlet {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "subpatch.outlet",
            display_name: "Outlet",
            category: "SubPatch",
            inputs: vec![PinDescriptor::new("in", SignalType::Any)],
            outputs: vec![],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        match inputs.first().copied().flatten() {
            Some(s) => smallvec::smallvec![s],
            None => SmallVec::new(),
        }
    }
}
