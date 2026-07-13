mod util;
pub mod controls;
pub mod display;
pub mod generator;
pub mod input_viewer;
pub mod logic;
pub mod math;
pub mod menu;
pub mod network;
pub mod processing;
pub mod subpatch;
pub mod touch;

// `macro` is a reserved keyword, hence the `_module` suffix on the file.
pub mod macro_module;

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
    modules.extend(network::registrations());
    modules.extend(touch::registrations());
    modules.extend(input_viewer::registrations());
    modules.extend(menu::registrations());
    modules.extend(macro_module::registrations());
    modules.extend(subpatch::registrations());
    modules
}
