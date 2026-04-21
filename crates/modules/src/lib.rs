mod util;
pub mod controls;
pub mod logic;
pub mod math;

use flexinput_core::ModuleRegistration;

/// Returns every built-in module registration.
pub fn all_modules() -> Vec<ModuleRegistration> {
    let mut modules = Vec::new();
    modules.extend(controls::registrations());
    modules.extend(math::registrations());
    modules.extend(logic::registrations());
    modules
}
