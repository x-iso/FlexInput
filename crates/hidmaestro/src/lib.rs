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

// Portable: the profiles, the report encoder and the feature answers describe
// a controller, not a driver. Linux serves the same pads through UHID with them.
pub mod answers;
pub mod descriptor;
pub mod encode;
pub mod profile;

#[cfg(windows)]
pub mod claims;
#[cfg(windows)]
pub mod deploy;
#[cfg(windows)]
pub mod helper;
#[cfg(windows)]
pub mod hidhide;
#[cfg(windows)]
pub mod helper_ipc;
#[cfg(windows)]
pub mod identity;
#[cfg(windows)]
pub mod install;
#[cfg(windows)]
pub mod orchestrator;
#[cfg(windows)]
pub mod server;
#[cfg(windows)]
pub mod shm;
#[cfg(windows)]
pub mod signing;
#[cfg(windows)]
pub mod usbip_bundled;
#[cfg(windows)]
pub mod usbip_install;

pub use descriptor::{parse_descriptor, InputField, InputReport};
pub use encode::{encode_report, encode_report_into, gip_from_state, GamepadState, Hat, PinState};
#[cfg(windows)]
pub use install::{
    driver_state, hidmaestro_available, installed_inf_names, installed_inf_path,
    installed_xusb_inf_path, DriverState,
};
#[cfg(windows)]
pub use orchestrator::{
    create_device_node, create_own_container_node, create_xusb_companion_node,
    list_hidmaestro_devices, node_diag,
    remove_all_hidmaestro_devices, remove_device_node, remove_orphan_hid_children,
    write_instance_config, CreatedDevice, ExistingDevice, NodeDiag, OrchestratorError,
    RemovalBatch, SwdHandle,
};
#[cfg(windows)]
pub use server::run_helper_server;
pub use profile::{Profile, ProfileError};
#[cfg(windows)]
pub use shm::{InputSection, OutputFrame, OutputSection, ShmError};
