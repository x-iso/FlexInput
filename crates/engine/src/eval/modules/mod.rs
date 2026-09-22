//! Per-module evaluators — one file per FlexInput module that needs graph
//! state, plus the helpers they share.
//!
//! Each is called from BOTH `eval_graph_tick` and `eval_subgraph`, which is
//! why they live here rather than inside either: the two dispatches must stay
//! identical, and a shared callee is what enforces that.

use super::*;

mod gyro3dof;
mod jsm;
mod lean;
mod map_action;
mod menu;
mod remapper;
mod remapper_hold_back;
mod rws;
mod shared;
mod touch_zones;

pub(crate) use gyro3dof::*;
pub(crate) use jsm::*;
// Read by the UI to compile the config text it is showing.
pub use jsm::{jsm_catalogue, jsm_compile, jsm_cursor_clamped, jsm_cursor_delete,
    jsm_insert_slot, JSM_SLOT,
    jsm_cursor_moved, jsm_cursor_replace, jsm_editor_focus, jsm_feel_of, jsm_knobs,
    jsm_line_count, jsm_selection, jsm_tokens_at, JsmCursor, JsmToken, JsmTokenKind,
    jsm_note_missing_inputs,
    jsm_sens_curve, jsm_sens_curve_warped, jsm_set_knob, set_jsm_editor_focus, JsmConfig, JsmCurvePoint, JsmFeel,
    JsmHand, JsmItem, JsmKind, JsmKnob, JsmLineInfo, JsmLineStatus, JsmSupportState};
pub(crate) use lean::*;
pub(crate) use map_action::*;
pub(crate) use menu::*;
pub(crate) use remapper::*;
pub(crate) use remapper_hold_back::*;
// Read by the UI to decide when a card offers its "in order" toggle.
pub use remapper_hold_back::in_order_applies;
pub(crate) use rws::*;
pub(crate) use touch_zones::*;
// `shared` carries `pin_is_analog_input`, which the UI reads through
// `flexinput_engine::eval::` — so this glob stays public.
pub use shared::*;
