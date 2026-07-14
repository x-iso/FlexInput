use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![reg::<VirtualMenuModule>()]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

// ── Virtual Menu ──────────────────────────────────────────────────────────────
//
// A screen-overlay menu divided into zones (same BSP tree as Touch Zones,
// single field). While the wired activation input holds it open, a pointer
// source (stick deflection or touchpad finger) highlights zones; releasing
// (or an explicit confirm) selects one. The menu renders on its OWN overlay
// viewport, independent of the info overlay's visibility.
//
// Named + iconed like a Macro port (one name/icon per module). Two output
// stages like Touch Zones (`zone_mode`):
//   "ports"   — per-zone Active/Selected pins + fixed Open/Hover pins.
//   "mapping" — internal mapping cards per zone (shared Remapper card schema)
//               with trigger tokens `menu_sel` (pulse on select) and
//               `menu_hover` (held while highlighted).
//
// Persisted params (all optional with defaults; ids are save-format):
//   menu_id                               — stable id behind the macro-style
//                                           target pins menu:{id}_show/_sel
//   menu_name, menu_icon                  — label + icon (Macro pattern)
//   col_edges, row_edges, zone_tree       — zone geometry (TZ keys, field 0 only)
//   zone_mode: "ports" | "mapping"
//   menu_maps: [..]                       — mapping cards (Remapper card schema)
//   zone_meta: [{id, label, icon}]        — per-zone display metadata
//   activation_mode: "hold" | "toggle" | "touch"
//   pointer_source: "left_stick" | "right_stick" | "touch1" | "touch2"
//   select_on: "release" | "press" | "click"
//   pointer_deadzone: f32
//   suppress_while_open: bool             — zero pointer pins on the passthrough
//   menu_rect: [x, y, w, h]               — monitor-fraction placement
//   ui_open_seq                           — monotonic body "test open" counter
//
// process() returns empty — the open/hover/select state machine lives in
// eval.rs ("module.menu"), publishing under the `menumap:{uid}` collector.
#[derive(Default)]
pub struct VirtualMenuModule;

impl Module for VirtualMenuModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.menu",
            display_name: "Virtual Menu",
            category: "AutoMap",
            inputs: vec![
                PinDescriptor::new("Device", SignalType::AutoMap),
                PinDescriptor::new("Show", SignalType::Bool).optional(),
                PinDescriptor::new("Select", SignalType::Bool).optional(),
                PinDescriptor::new("Pointer", SignalType::Vec2).optional(),
            ],
            outputs: vec![
                PinDescriptor::new("AutoMap", SignalType::AutoMap),
                PinDescriptor::new("Open", SignalType::Bool),
                PinDescriptor::new("Hover", SignalType::Float),
            ],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}
