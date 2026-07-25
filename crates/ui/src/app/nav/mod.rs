//! Gamepad-navigation driving, split by the area of the UI being driven.
//!
//! Every child adds methods to `FlexInputApp` via its own `impl` block. Those
//! methods are `pub(crate)` rather than private: a private method defined in a
//! child module is invisible to `app.rs` (and to sibling nav modules), and the
//! nav clusters call across each other constantly.

use super::*;

mod config;
mod curves;
mod fields;
mod gp_settings;
mod left_panel;
mod legend;
mod pickers;
mod remap;
mod touch_zones;

// Viewport-agnostic free fns the config overlay reuses (drawn on the overlay
// viewport, not just the main window).
pub(crate) use remap::draw_remap_card_glow;
