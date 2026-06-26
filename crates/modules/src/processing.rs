use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        reg::<DelayModule>(),
        reg::<AverageModule>(),
        reg::<DcFilterModule>(),
        reg::<ResponseCurveModule>(),
        reg::<VecResponseCurveModule>(),
        reg::<TwowayResponseCurveModule>(),
        reg::<VecToAxisModule>(),
        reg::<AxisToVecModule>(),
        reg::<Gyro3DOFModule>(),
        reg::<AutoMapSplitModule>(),
        reg::<AutoMapCollectModule>(),
        reg::<AutoMapFork>(),
        reg::<AutoMapSelector>(),
        reg::<AutoMapCombiner>(),
        reg::<RemapperModule>(),
        reg::<MapActionModule>(),
        reg::<FeedbackControlModule>(),
        reg::<AudioStreamHapticsModule>(),
    ]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

// ── Delay ─────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct DelayModule;

impl Module for DelayModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.delay",
            display_name: "Delay",
            category: "Processing",
            inputs: vec![PinDescriptor::new("In", SignalType::Float)],
            outputs: vec![PinDescriptor::new("Out", SignalType::Float)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Moving Average ────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct AverageModule;

impl Module for AverageModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.average",
            display_name: "Average",
            category: "Processing",
            inputs: vec![PinDescriptor::new("In", SignalType::Any)],
            outputs: vec![PinDescriptor::new("Out", SignalType::Any)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── DC Filter ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct DcFilterModule;

impl Module for DcFilterModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.dc_filter",
            display_name: "DC Filter",
            category: "Processing",
            inputs: vec![PinDescriptor::new("In", SignalType::Float)],
            outputs: vec![PinDescriptor::new("Out", SignalType::Float)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Response Curve ────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct ResponseCurveModule;

#[derive(Default)]
pub struct VecResponseCurveModule;

impl Module for ResponseCurveModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.response_curve",
            display_name: "Response Curve",
            category: "Processing",
            inputs: vec![
                PinDescriptor::new("In 1", SignalType::Float),
                PinDescriptor::new("In 2", SignalType::Float),
                PinDescriptor::new("In 3", SignalType::Float),
            ],
            outputs: vec![
                PinDescriptor::new("Out 1", SignalType::Float),
                PinDescriptor::new("Out 2", SignalType::Float),
                PinDescriptor::new("Out 3", SignalType::Float),
            ],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Vec Response Curve ────────────────────────────────────────────────────────

impl Module for VecResponseCurveModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.vec_response_curve",
            display_name: "Vec Response Curve",
            category: "Processing",
            inputs: vec![PinDescriptor::new("In", SignalType::Vec2)],
            outputs: vec![PinDescriptor::new("Out", SignalType::Vec2)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Two-way Response Curve ────────────────────────────────────────────────────

#[derive(Default)]
pub struct TwowayResponseCurveModule;

impl Module for TwowayResponseCurveModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.twoway_response_curve",
            display_name: "Two-way Response Curve",
            category: "Processing",
            inputs:  vec![PinDescriptor::new("In 1",  SignalType::Float)],
            outputs: vec![PinDescriptor::new("Out 1", SignalType::Float)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Vec to Axis ───────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct VecToAxisModule;

impl Module for VecToAxisModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.vec_to_axis",
            display_name: "Vec to Axis",
            category: "Converters",
            inputs: vec![PinDescriptor::new("In", SignalType::Vec2)],
            outputs: vec![
                PinDescriptor::new("X", SignalType::Float),
                PinDescriptor::new("Y", SignalType::Float),
            ],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Axis to Vec ───────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct AxisToVecModule;

impl Module for AxisToVecModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.axis_to_vec",
            display_name: "Axis to Vec",
            category: "Converters",
            inputs: vec![
                PinDescriptor::new("X", SignalType::Float),
                PinDescriptor::new("Y", SignalType::Float),
            ],
            outputs: vec![PinDescriptor::new("Out", SignalType::Vec2)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── AutoMap Splitter ──────────────────────────────────────────────────────────

/// Accepts an AutoMap input and exposes individual device signals as typed outputs,
/// plus an AutoMap passthrough output for downstream routing.
/// User adds outputs via the node body UI; `output_pin_ids[0]` = "automap_pass" (passthrough),
/// `output_pin_ids[1..]` = selected device pin IDs.
#[derive(Default)]
pub struct AutoMapSplitModule;

impl Module for AutoMapSplitModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.automap_split",
            display_name: "AutoMap Splitter",
            category: "AutoMap",
            inputs: vec![PinDescriptor::new("Device", SignalType::AutoMap)],
            outputs: vec![PinDescriptor::new("AutoMap", SignalType::AutoMap)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // Signals injected by compute_node in eval.rs via dev_sigs lookup.
        SmallVec::new()
    }
}

// ── AutoMap Collector ─────────────────────────────────────────────────────────

/// Accepts an AutoMap input and individual signal inlets (user-added from canonical list),
/// then merges them into an AutoMap output. Individual inlets override specific signals;
/// non-overridden signals pass through from the upstream source device.
#[derive(Default)]
pub struct AutoMapCollectModule;

impl Module for AutoMapCollectModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.automap_collect",
            display_name: "AutoMap Collector",
            category: "AutoMap",
            inputs: vec![PinDescriptor::new("Device", SignalType::AutoMap)],
            outputs: vec![PinDescriptor::new("AutoMap", SignalType::AutoMap)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // Signals injected into collector_sigs by eval_graph_tick in eval.rs.
        SmallVec::new()
    }
}

// ── Audio Stream Haptics ───────────────────────────────────────────────────────

/// Derives rumble from an application's (or the system's) audio and injects it
/// into the AutoMap bus passing through it, so a gamepad wired in→out feels the
/// game's audio as haptics — no driver required (WASAPI loopback).
///
/// A gamepad routes THROUGH this node (AutoMap in → AutoMap out). The proc thread
/// runs a WASAPI loopback capture for the configured target (a picked process,
/// the focused app, or the system mix) and the eval block blends the resulting
/// per-side (amp, freq) with any standard rumble already on the bus per the
/// `asth_modulator` slider, then injects HD-rumble feedback into the outgoing bus.
/// All real work is in eval.rs (`module.audio_stream_haptics`); params:
///   `asth_mode` (process|focused|system), `asth_target_name`, `asth_include_tree`,
///   `asth_modulator` (0=gate by std rumble, 0.5=boost, 1=replace).
#[derive(Default)]
pub struct AudioStreamHapticsModule;

impl Module for AudioStreamHapticsModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.audio_stream_haptics",
            display_name: "Audio Stream Haptics",
            category: "AutoMap",
            inputs: vec![PinDescriptor::new("Device", SignalType::AutoMap)],
            outputs: vec![PinDescriptor::new("AutoMap", SignalType::AutoMap)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // Haptics injected into the AutoMap bus by eval_graph_tick in eval.rs;
        // the capture itself is owned by the processing thread's LoopbackManager.
        SmallVec::new()
    }
}

// ── AutoMap Fork ─────────────────────────────────────────────────────────────

/// Routes one AutoMap bus to one of two AutoMap outputs based on a Bool/Float select pin.
/// select = false/0.0 → out_0 receives the bus, out_1 is disconnected.
/// select = true/1.0  → out_1 receives the bus, out_0 is disconnected.
/// Named "Fork" to distinguish from the existing Split (scalar) module.
#[derive(Default)]
pub struct AutoMapFork;

impl Module for AutoMapFork {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.automap_fork",
            display_name: "AutoMap Fork",
            category: "AutoMap",
            inputs: vec![
                PinDescriptor::new("in", SignalType::AutoMap),
                PinDescriptor::new("select", SignalType::Float),
            ],
            outputs: vec![
                PinDescriptor::new("out_0", SignalType::AutoMap),
                PinDescriptor::new("out_1", SignalType::AutoMap),
            ],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // AutoMap bus routing: the eval graph follows wires to the upstream AutoMap source.
        // The select pin controls which output wire is "active" at the UI level.
        let bus = inputs.get(0).and_then(|s| *s).unwrap_or(Signal::Float(0.0));
        let select = match inputs.get(1).and_then(|s| *s) {
            Some(Signal::Float(f)) => f > 0.5,
            Some(Signal::Bool(b)) => b,
            _ => false,
        };
        let mut r = SmallVec::new();
        r.push(if !select { bus } else { Signal::Float(0.0) });
        r.push(if select { bus } else { Signal::Float(0.0) });
        r
    }
}

// ── AutoMap Selector ──────────────────────────────────────────────────────────

/// Selects one of N AutoMap bus inputs and routes it to a single AutoMap output.
/// select pin (Float 0..1) is quantized to N slots, same as the scalar Selector module.
/// Default: 2 inputs (in_0, in_1); additional inputs added via params["n_inputs"].
#[derive(Default)]
pub struct AutoMapSelector;

impl Module for AutoMapSelector {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.automap_selector",
            display_name: "AutoMap Selector",
            category: "AutoMap",
            inputs: vec![
                PinDescriptor::new("select", SignalType::Float),
                PinDescriptor::new("in_0", SignalType::AutoMap),
                PinDescriptor::new("in_1", SignalType::AutoMap),
            ],
            outputs: vec![
                PinDescriptor::new("out", SignalType::AutoMap),
            ],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // inputs[0] = select (Float 0..1), inputs[1..] = AutoMap buses
        let n = inputs.len().saturating_sub(1).max(1);
        let select = match inputs.get(0).and_then(|s| *s) {
            Some(Signal::Float(f)) => {
                let slot = (f.clamp(0.0, 1.0) * (n as f32 - 1.0 + 0.5)).floor() as usize;
                slot.min(n - 1)
            }
            Some(Signal::Bool(b)) => if b { 1 } else { 0 },
            _ => 0,
        };
        let selected_bus = inputs.get(select + 1).and_then(|s| *s).unwrap_or(Signal::Float(0.0));
        let mut r = SmallVec::new();
        r.push(selected_bus);
        r
    }
}

// ── AutoMap Combiner ──────────────────────────────────────────────────────────

/// Merges multiple AutoMap bus inputs into one. Priority by port order: the
/// uppermost input that has a pin "asserted" (Bool true, non-zero Float) wins
/// that pin for this tick. Other inputs contribute pins they alone assert.
/// Default: 2 inputs (in_0, in_1); additional inputs added by extending the
/// node's input vec from the canvas body.
#[derive(Default)]
pub struct AutoMapCombiner;

impl Module for AutoMapCombiner {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.automap_combiner",
            display_name: "AutoMap Combiner",
            category: "AutoMap",
            inputs: vec![
                PinDescriptor::new("in_0", SignalType::AutoMap),
                PinDescriptor::new("in_1", SignalType::AutoMap),
            ],
            outputs: vec![
                PinDescriptor::new("out", SignalType::AutoMap),
            ],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // Bus value is irrelevant — eval reads `combiner:{uid}` collector_sigs
        // entries written by the dedicated branch in `eval::tick`.
        let mut r = SmallVec::new();
        r.push(Signal::Float(0.0));
        r
    }
}

// ── Gyro 3DOF to 2D ───────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Gyro3DOFModule;

impl Module for Gyro3DOFModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "processing.gyro_3dof",
            display_name: "3DOF to 2D",
            category: "Processing",
            inputs: vec![
                PinDescriptor::new("Device",  SignalType::AutoMap),
                PinDescriptor::new("Reset",   SignalType::Bool).optional(),
                PinDescriptor::new("Gyro X",  SignalType::Float).optional(),
                PinDescriptor::new("Gyro Y",  SignalType::Float).optional(),
                PinDescriptor::new("Gyro Z",  SignalType::Float).optional(),
                PinDescriptor::new("Accel X", SignalType::Float).optional(),
                PinDescriptor::new("Accel Y", SignalType::Float).optional(),
                PinDescriptor::new("Accel Z", SignalType::Float).optional(),
            ],
            outputs: vec![
                PinDescriptor::new("Out",   SignalType::Vec2),
                PinDescriptor::new("X",     SignalType::Float),
                PinDescriptor::new("Y",     SignalType::Float),
                PinDescriptor::new("Lean",  SignalType::Float),
                PinDescriptor::new("Lean!", SignalType::Bool),
                // AutoMap-out carries downstream per-pin signals produced
                // by the module's lean_left / lean_right mapping arrays.
                // Same contract as Remapper: pin injection happens in
                // eval.rs.
                PinDescriptor::new("Map",   SignalType::AutoMap),
            ],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Remapper ─────────────────────────────────────────────────────────────────
//
// Single AutoMap in, single AutoMap out. Captures button presses live from the
// input wire, lets the user define mappings via a Learn UI, and (in a future
// phase) injects per-pin overrides at eval time.
//
// All state lives in `node.params` so it persists with the patch:
//   ui_phase:      "idle" | "capturing" | "ready_to_learn" | "learning"
//   draft_input:   [pin_id, ...]
//   draft_output:  [pin_id, ...]
//   mappings:      [{in: [pin_id, ...], out: [pin_id, ...]}, ...]
//   skin:          "auto" | "xbox" | "playstation" | "switchpro" | "kbm"
//
// The process() body returns empty — the engine will inject Remapper output
// signals in eval.rs (future phase), matching the AutoMap Splitter/Collector
// pattern.
#[derive(Default)]
pub struct RemapperModule;

impl Module for RemapperModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.remapper",
            display_name: "Remapper",
            category: "AutoMap",
            inputs:  vec![PinDescriptor::new("in",  SignalType::AutoMap)],
            outputs: vec![PinDescriptor::new("out", SignalType::AutoMap)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Feedback Control ──────────────────────────────────────────────────────────
//
// Surfaces the otherwise-hidden AutoMap feedback channel as real node pins so
// rumble/light effects can be injected (or the game's feedback request tapped)
// from inside a sub-patch, where device pins aren't reachable.
//
//   • input 0  "Device" (AutoMap) — sits inline on the bus; passes straight to
//              output 0. Used to trace the upstream physical source pad (inject
//              target) and the downstream virtual destination (tap source).
//   • inputs 1..N  — universal haptic union (FEEDBACK_INLET_PINS). Wired effect
//              signals here inject backward along the bus into the physical pad;
//              ports the connected pad lacks are silently dropped.
//   • output 0  "AutoMap" — bus pass-through.
//   • outputs 1..M  — basic rumble/light taps (FEEDBACK_OUTLET_PINS) read from
//              the downstream virtual destination's feedback source-pins.
//
// process() returns empty — eval.rs injects inlets into collector_sigs and reads
// outlet taps from dev_sigs, matching the AutoMap Splitter/Collector pattern.
#[derive(Default)]
pub struct FeedbackControlModule;

impl Module for FeedbackControlModule {
    fn descriptor() -> ModuleDescriptor {
        use flexinput_core::automap::{FEEDBACK_INLET_PINS, FEEDBACK_OUTLET_PINS};
        let mut inputs = vec![PinDescriptor::new("Device", SignalType::AutoMap)];
        inputs.extend(FEEDBACK_INLET_PINS.iter()
            .map(|p| PinDescriptor::new(p.display_name, p.signal_type).optional()));
        let mut outputs = vec![PinDescriptor::new("AutoMap", SignalType::AutoMap)];
        outputs.extend(FEEDBACK_OUTLET_PINS.iter()
            .map(|p| PinDescriptor::new(p.display_name, p.signal_type)));
        ModuleDescriptor {
            id: "module.feedback_control",
            display_name: "Feedback Control",
            category: "AutoMap",
            inputs,
            outputs,
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Map Action ────────────────────────────────────────────────────────────
//
// Simplified Remapper-derived module: accepts an AutoMap input and exposes a
// single Bool output that is true when any configured mapping (an AND of
// listed pins) is currently held on the upstream bus. Mappings are stored
// in `node.params["mappings"]` as `Array<Array<String>>`.
#[derive(Default)]
pub struct MapActionModule;

impl Module for MapActionModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.map_action",
            display_name: "Map Action",
            category: "AutoMap",
            inputs:  vec![PinDescriptor::new("in",  SignalType::AutoMap)],
            // Two separate outputs:
            //   out        — Bool: "any mapping active". In analog mode this
            //                is a freq-modulated tap train (Hold → PWM,
            //                Turbo → ×2 frequency) so a digital destination
            //                reflects how far the analog input is pushed.
            //   out_analog — Float 0..1: the pure max magnitude across active
            //                mappings (digital-active contributes 1.0). Drives
            //                continuous destinations (axes / triggers).
            outputs: vec![
                PinDescriptor::new("Gate",   SignalType::Bool),
                PinDescriptor::new("Analog", SignalType::Float),
            ],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}
