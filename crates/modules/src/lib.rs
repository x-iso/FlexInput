mod util;
pub mod controls;
pub mod display;
pub mod generator;
pub mod logic;
pub mod math;
pub mod processing;
pub mod subpatch;

use flexinput_core::ModuleRegistration;

/// Returns every built-in module registration.
pub fn all_modules() -> Vec<ModuleRegistration> {
    let mut modules = Vec::new();
    modules.extend(controls::registrations());
    modules.extend(math::registrations());
    modules.extend(logic::registrations());
    modules.extend(display::registrations());
    modules.extend(processing::registrations());
    modules.extend(generator::registrations());
    modules.extend(subpatch::registrations());
    modules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_modules_includes_feedback_sinks() {
        let modules = all_modules();
        let ids: Vec<&str> = modules.iter().map(|r| r.descriptor.id).collect();
        assert!(
            ids.contains(&"module.rumble_output"),
            "all_modules() must include module.rumble_output; found: {:?}", ids
        );
        assert!(
            ids.contains(&"module.rgb_output"),
            "all_modules() must include module.rgb_output; found: {:?}", ids
        );
        assert!(
            ids.contains(&"module.adaptive_trigger_output"),
            "all_modules() must include module.adaptive_trigger_output; found: {:?}", ids
        );
    }
}
