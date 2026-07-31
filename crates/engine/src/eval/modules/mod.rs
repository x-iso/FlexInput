//! Per-module evaluators — one file per FlexInput module that needs graph
//! state, plus the helpers they share.
//!
//! Each is called from BOTH `eval_graph_tick` and `eval_subgraph`, which is
//! why they live here rather than inside either: the two dispatches must stay
//! identical, and a shared callee is what enforces that.

use super::*;

mod gyro3dof;
mod lean;
mod map_action;
mod menu;
mod remapper;
mod rws;
mod shared;
mod touch_zones;

pub(crate) use gyro3dof::*;
pub(crate) use lean::*;
pub(crate) use map_action::*;
pub(crate) use menu::*;
pub(crate) use remapper::*;
pub(crate) use rws::*;
pub(crate) use touch_zones::*;
// `shared` carries `pin_is_analog_input`, which the UI reads through
// `flexinput_engine::eval::` — so this glob stays public.
pub use shared::*;
