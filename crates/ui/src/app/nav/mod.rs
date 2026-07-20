//! Gamepad-navigation driving, split by the area of the UI being driven.
//!
//! Every child adds methods to `FlexInputApp` via its own `impl` block. Those
//! methods are `pub(crate)` rather than private: a private method defined in a
//! child module is invisible to `app.rs` (and to sibling nav modules), and the
//! nav clusters call across each other constantly.

use super::*;

mod curves;
