//! JSM Config module (`module.jsm`): a JoyShockMapper config, applied to the
//! AutoMap bus with JSM's own rules.
//!
//! The config text is the only source of truth. It is compiled ([`parse`]) into
//! bindings plus a status for every line, and run by the button state machines
//! ([`bind`]); [`names`] holds JSM's vocabulary and what it is on our bus.
//!
//! Deliberately NOT a front-end for the Remapper: JSM's press rules, timings and
//! chord layering are its own, and this module implements them directly. See
//! `docs/JSM_MODULE_PLAN.md` for the scope and the phase order.

mod aim;
mod analog;
mod bind;
mod cal;
mod catalogue;
mod cc;
mod cursor;
mod eval;
mod feedback;
mod help;
mod knobs;
mod midi;
mod motion;
mod names;
mod pad;
mod parse;
mod touch;

#[cfg(test)]
mod tests;

pub(crate) use eval::{jsm_publish, JsmState};

// Read by the calibration widget: where the sweep's measurement lands.
pub use eval::{cal_deg_out as jsm_cal_deg_out, cal_peak_out as jsm_cal_peak_out,
    JSM_MACROS_PARAM, JSM_MACRO_OUTS_PARAM};

// The UI pauses the config's typing while its editor has focus.
pub use eval::{jsm_editor_focus, set_jsm_editor_focus};

// The editor compiles the text it is showing to put a status on every line, so
// the parser's surface is public — the run-time side stays crate-internal.
pub use cursor::{clamped as jsm_cursor_clamped, delete as jsm_cursor_delete,
    insert_line as jsm_insert_line, insert_slot as jsm_insert_slot, SLOT as JSM_SLOT,
    line_count as jsm_line_count, line_span as jsm_line_span,
    moved as jsm_cursor_moved, replace as jsm_cursor_replace,
    selection as jsm_selection, tokens_at as jsm_tokens_at, Cursor as JsmCursor,
    Token as JsmToken, TokenKind as JsmTokenKind};
pub use catalogue::{bindings as jsm_bindings, catalogue as jsm_catalogue, fi_tag as jsm_fi_tag,
    input_names_by_pin as jsm_input_names_by_pin, names_by_pin as jsm_names_by_pin,
    insert_pick as jsm_insert_pick, value_options as jsm_value_options,
    cycle_value as jsm_cycle_value,
    kinds_at as jsm_kinds_at, Item as JsmItem, Kind as JsmKind,
    State as JsmSupportState};
pub use knobs::{feel_of as jsm_feel_of, knobs as jsm_knobs, scrub as jsm_scrub_number,
    line_of as jsm_setting_line, set_setting as jsm_set_setting,
    set_knob_part as jsm_set_knob_part, toggle_pair as jsm_toggle_pair,
    key_setting as jsm_knob_key_setting, pairable as jsm_pairable,
    replace_word as jsm_replace_word,
    sens_curve as jsm_sens_curve,
    sens_curve_warped as jsm_sens_curve_warped,
    set_knob as jsm_set_knob, CurvePoint as JsmCurvePoint, Feel as JsmFeel, Hand as JsmHand,
    Knob as JsmKnob};
// The editor's "MIDI…" row writes what a MIDI picker built as a config name.
pub use midi::tag_for_pin as jsm_midi_tag;
pub use parse::{at_names as jsm_at_names, at_name_spans as jsm_at_name_spans, compile_full as jsm_compile_full, compile_with as jsm_compile, note_inputs_this_pad_lacks as jsm_note_missing_inputs,
    Compiled as JsmConfig, LineInfo as JsmLineInfo, LineStatus as JsmLineStatus};
