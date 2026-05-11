use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

use crate::util::get_float;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        reg::<ConstantModule>(),
        reg::<SwitchModule>(),
        reg::<KnobModule>(),
        reg::<SelectorModule>(),
        reg::<LabelModule>(),
        reg::<SvgModule>(),
        ModuleRegistration {
            descriptor: SplitModule::descriptor(),
            factory: || Box::new(SplitModule::default()),
        },
    ]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

// ── Constant ─────────────────────────────────────────────────────────────────

/// Outputs a fixed Float value set via the body UI.
#[derive(Default)]
pub struct ConstantModule;

impl Module for ConstantModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.constant",
            display_name: "Constant",
            category: "Utility",
            inputs: vec![],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // Value resolved from params by the router; this path only runs in the engine.
        SmallVec::new()
    }
}

// ── Switch ────────────────────────────────────────────────────────────────────

/// Toggle that outputs a Bool (true/false) set via a checkbox in the body UI.
#[derive(Default)]
pub struct SwitchModule;

impl Module for SwitchModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.switch",
            display_name: "Switch",
            category: "Utility",
            inputs: vec![],
            outputs: vec![PinDescriptor::new("out", SignalType::Bool)],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        SmallVec::new()
    }
}

// ── Label / Text ──────────────────────────────────────────────────────────────

/// Static text label for the canvas. No inputs, no outputs — purely visual,
/// used as documentation/annotation on top-level canvases or sub-patch bodies.
/// Body UI exposes a multiline text edit and a font-size slider.
#[derive(Default)]
pub struct LabelModule;

impl Module for LabelModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.label",
            display_name: "Text",
            category: "Utility",
            inputs: vec![],
            outputs: vec![],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        SmallVec::new()
    }
}

// ── SVG ───────────────────────────────────────────────────────────────────────

/// Static SVG image for the canvas. No inputs, no outputs — purely visual.
/// SVG source is stored verbatim in the patch (`svg_data` param) so patches
/// stay self-contained without external icon files. The body shows the image
/// scaled to a resizable area; an optional RGBA tint blends a color over the
/// glyph (alpha = blend amount, 0 = no tint).
#[derive(Default)]
pub struct SvgModule;

impl Module for SvgModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.svg",
            display_name: "SVG",
            category: "Utility",
            inputs: vec![],
            outputs: vec![],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        SmallVec::new()
    }
}

// ── Knob ──────────────────────────────────────────────────────────────────────

/// Outputs a Float in [0, 1] set via a slider in the body UI.
#[derive(Default)]
pub struct KnobModule;

impl Module for KnobModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.knob",
            display_name: "Knob",
            category: "Utility",
            inputs: vec![],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        SmallVec::new()
    }
}

// ── Selector ──────────────────────────────────────────────────────────────────

/// Routes one of N value inputs to `out` based on `select` (Float 0..1, quantized to N slots).
#[derive(Default)]
pub struct SelectorModule;

impl Module for SelectorModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.selector",
            display_name: "Selector",
            category: "Utility",
            inputs: vec![
                PinDescriptor::new("select", SignalType::Float),
                PinDescriptor::new("in_0",   SignalType::Any),
                PinDescriptor::new("in_1",   SignalType::Any),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Any)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        if inputs.len() < 2 { return SmallVec::new(); }
        let n = (inputs.len() - 1) as f32;
        let sel = get_float(inputs, 0, 0.0);
        let idx = (sel.clamp(0.0, 1.0) * n).floor() as usize;
        let idx = idx.min(inputs.len() - 2);
        match inputs.get(idx + 1).and_then(|s| *s) {
            Some(sig) => { let mut r = SmallVec::new(); r.push(sig); r }
            None => SmallVec::new(),
        }
    }
}

// ── Split ─────────────────────────────────────────────────────────────────────

/// Routes `in` to one of N outputs based on `select` (Float 0..1); unselected outputs emit 0.0.
pub struct SplitModule {
    pub n_outputs: usize,
}

impl Default for SplitModule {
    fn default() -> Self { Self { n_outputs: 2 } }
}

impl Module for SplitModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.split",
            display_name: "Split",
            category: "Utility",
            inputs: vec![
                PinDescriptor::new("select", SignalType::Float),
                PinDescriptor::new("in",     SignalType::Any),
            ],
            outputs: vec![
                PinDescriptor::new("out_0", SignalType::Any),
                PinDescriptor::new("out_1", SignalType::Any),
            ],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        let n = self.n_outputs.max(1);
        let sel = get_float(inputs, 0, 0.0);
        let val = inputs.get(1).and_then(|s| *s);
        let idx = (sel.clamp(0.0, 1.0) * n as f32).floor() as usize;
        let idx = idx.min(n - 1);
        let mut r = SmallVec::new();
        for i in 0..n {
            r.push(if i == idx { val.unwrap_or(Signal::Float(0.0)) } else { Signal::Float(0.0) });
        }
        r
    }
}
