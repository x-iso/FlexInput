mod util;
pub mod logic;
pub mod math;

use flexinput_core::ModuleRegistration;

/// Returns every built-in module registration.
/// The UI populates the module picker from this list automatically.
/// Community crates can expose their own `registrations()` and have the user
/// add them here — no core changes needed.
pub fn all_modules() -> Vec<ModuleRegistration> {
    let mut modules = Vec::new();
    modules.extend(math::registrations());
    modules.extend(logic::registrations());
    modules
}
