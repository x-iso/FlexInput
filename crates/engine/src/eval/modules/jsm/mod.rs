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
mod cc;
mod eval;
mod feedback;
mod knobs;
mod motion;
mod names;
mod pad;
mod parse;
mod touch;

#[cfg(test)]
mod tests;

pub(crate) use eval::{jsm_publish, JsmState};

// The UI pauses the config's typing while its editor has focus.
pub use eval::{jsm_editor_focus, set_jsm_editor_focus};

// The editor compiles the text it is showing to put a status on every line, so
// the parser's surface is public — the run-time side stays crate-internal.
pub use knobs::{knobs as jsm_knobs, sens_curve as jsm_sens_curve, set_knob as jsm_set_knob,
    CurvePoint as JsmCurvePoint, Knob as JsmKnob};
pub use parse::{compile_with as jsm_compile, note_inputs_this_pad_lacks as jsm_note_missing_inputs,
    Compiled as JsmConfig, LineInfo as JsmLineInfo, LineStatus as JsmLineStatus};
