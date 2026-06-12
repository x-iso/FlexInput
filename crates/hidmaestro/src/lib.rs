//! Pure-Rust client for HIDMaestro's user-mode virtual-controller driver.
//!
//! HIDMaestro (MIT, hifihedgehog/HIDMaestro) is a UMDF2-based virtual game
//! controller platform. Its C# SDK talks to the driver entirely through named
//! shared-memory sections + a named auto-reset event — there is nothing
//! .NET-specific on the wire. This crate ports that protocol to Rust so
//! FlexInput can drive HIDMaestro devices without a .NET runtime.
//!
//! Phase 1 (this module set) is the **shared-memory hot path only**:
//! - [`shm::InputSection`] — per-frame seqlock writer for input reports.
//! - [`shm::OutputSection`] — ring-buffer reader for rumble/FFB passthrough.
//!
//! Device *creation* (the SetupAPI/CfgMgr32/INF machinery) is a later phase;
//! for the Phase-1 verification gate the section is created by HIDMaestro's own
//! C# test app and this crate merely OPENS it.
//!
//! All protocol constants and offsets are transcribed verbatim from
//! `sdk/HIDMaestro.Core/Internal/SharedMemoryIO.cs` (which itself must match
//! `driver/driver.h`). Pin to a known HIDMaestro commit — the layout is
//! version-coupled.

#![cfg(windows)]

pub mod deploy;
pub mod descriptor;
pub mod encode;
pub mod helper_ipc;
pub mod install;
pub mod orchestrator;
pub mod profile;
pub mod shm;

pub use descriptor::{parse_descriptor, InputField, InputReport};
pub use encode::{encode_report, encode_report_into, GamepadState, Hat, PinState};
pub use install::{hidmaestro_available, installed_inf_path};
pub use orchestrator::{create_device_node, remove_device_node, CreatedDevice, OrchestratorError};
pub use profile::{Profile, ProfileError};
pub use shm::{InputSection, OutputFrame, OutputSection, ShmError};
