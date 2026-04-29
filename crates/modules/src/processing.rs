use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        reg::<DelayModule>(),
        reg::<AverageModule>(),
        reg::<DcFilterModule>(),
        reg::<ResponseCurveModule>(),
        reg::<VecResponseCurveModule>(),
        reg::<VecToAxisModule>(),
        reg::<AxisToVecModule>(),
        reg::<Gyro3DOFModule>(),
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
            inputs: vec![PinDescriptor::new("In", SignalType::Float)],
            outputs: vec![PinDescriptor::new("Out", SignalType::Float)],
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
                PinDescriptor::new("Out", SignalType::Vec2),
                PinDescriptor::new("X",   SignalType::Float),
                PinDescriptor::new("Y",   SignalType::Float),
            ],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}
