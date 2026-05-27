//! Easy-mode UI — preset-driven two-panel view that replaces the
//! advanced snarl canvas + side panels. See PLAN: §4–§7 in
//! `C:\Users\xiso\.claude\plans\ui-that-flexinput-currently-snazzy-otter.md`.
//!
//! Easy mode reads & mutates the same `PatchTab.canvas` snarl as
//! Advanced mode — the source of truth is unchanged. What differs is
//! what's drawn and what snarl mutations the UI emits when the user
//! picks a device / preset / output.
//!
//! Layout: a single left panel hosts both input and output pickers
//! (scrollable gamepad list on top, fixed output section on bottom);
//! the central panel renders the active sub-patch preset.

pub mod center_panel;
pub mod io_panel;
pub mod layout;
pub mod presets;
pub mod wiring;
