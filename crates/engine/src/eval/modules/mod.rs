//! Per-module evaluators — one file per FlexInput module that needs graph
//! state, plus the helpers they share.
//!
//! Each is called from BOTH `eval_graph_tick` and `eval_subgraph`, which is
//! why they live here rather than inside either: the two dispatches must stay
//! identical, and a shared callee is what enforces that.

use super::*;

mod area_mapper;
mod automap_curve;
mod gyro3dof;
mod jsm;
mod lean;
mod map_action;
mod menu;
mod remapper;
mod remapper_hold_back;
mod rws;
mod shared;
mod stick_rotation;
mod touch_zones;

// The param names and helpers are read by the UI too.
pub use area_mapper::*;
pub(crate) use automap_curve::*;
pub(crate) use gyro3dof::*;
pub(crate) use jsm::*;
// Read by the UI to compile the config text it is showing.
pub use jsm::{jsm_at_names, jsm_at_name_spans, jsm_bindings, jsm_catalogue, jsm_compile, jsm_compile_full, jsm_fi_tag,
    jsm_input_names_by_pin,
    jsm_names_by_pin, jsm_cursor_clamped, jsm_cursor_delete,
    jsm_insert_line, jsm_insert_pick, jsm_insert_slot, jsm_kinds_at, JSM_SLOT,
    jsm_value_options, jsm_cycle_value,
    jsm_cursor_moved, jsm_cursor_replace, jsm_editor_focus, jsm_feel_of, jsm_rotation_of, jsm_knobs,
    jsm_line_count, jsm_line_span, jsm_scrub_number,
    jsm_selection, jsm_tokens_at, JsmCursor, JsmToken, JsmTokenKind,
    jsm_note_missing_inputs, jsm_midi_tag,
    jsm_sens_curve, jsm_sens_curve_warped, jsm_set_knob, jsm_set_setting, jsm_setting_line,
    jsm_cal_deg_out, jsm_cal_peak_out, jsm_curve_dps_out, jsm_rotation_out, JSM_MACROS_PARAM, JSM_MACRO_OUTS_PARAM,
    jsm_set_knob_part, jsm_toggle_pair, jsm_knob_key_setting, jsm_pairable, jsm_replace_word,
    set_jsm_editor_focus, JsmConfig, JsmCurvePoint, JsmFeel,
    JsmHand, JsmItem, JsmKind, JsmKnob, JsmLineInfo, JsmLineStatus, JsmSupportState};
pub(crate) use lean::*;
pub(crate) use map_action::*;
pub(crate) use menu::*;
pub(crate) use remapper::*;
pub(crate) use remapper_hold_back::*;
// Read by the UI to decide when a card offers its "in order" toggle.
pub use remapper_hold_back::in_order_applies;
pub(crate) use rws::*;
// Where the calibration readouts land, read by the UI's measure widget.
pub use rws::{rws_cal_deg_out, rws_cal_peak_out, rws_flick_mode};
// The param names and live-output slots are read by the UI too; the JSM module
// shares the rotation itself.
pub use stick_rotation::*;
pub(crate) use touch_zones::*;
// `shared` carries `pin_is_analog_input`, which the UI reads through
// `flexinput_engine::eval::` — so this glob stays public.
pub use shared::*;
