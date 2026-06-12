//! Process-global manager for the elevated HIDMaestro helper.
//!
//! The main (unelevated) app spawns `hidmaestro_helper.exe` elevated **once**
//! (single UAC) and reuses the connection for every create/destroy. This module
//! owns that lifecycle behind a lazily-initialized singleton so any call site
//! (the device pool, the availability probe) can request a device without caring
//! about elevation or IPC plumbing.
//!
//! Creation is synchronous from the caller's view: `create()` blocks on the
//! helper's reply. The helper keeps the device's shared sections mapped alive;
//! the returned instance id is used later for `destroy()`.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::helper_ipc::{HelperClient, Request, Response};

/// Errors surfaced to the app when talking to the helper.
#[derive(Debug)]
pub enum HelperError {
    /// The helper executable could not be located next to the app.
    HelperMissing(PathBuf),
    /// Spawning the helper elevated failed (e.g. UAC declined).
    Spawn(String),
    /// Connecting to the helper's pipe failed.
    Connect(String),
    /// The helper returned an error response.
    Helper(String),
    /// Transport/IO error talking to the helper.
    Io(String),
}

impl std::fmt::Display for HelperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HelperError::HelperMissing(p) => write!(f, "helper exe not found at {}", p.display()),
            HelperError::Spawn(s) => write!(f, "spawn elevated helper failed: {s}"),
            HelperError::Connect(s) => write!(f, "connect to helper failed: {s}"),
            HelperError::Helper(s) => write!(f, "helper error: {s}"),
            HelperError::Io(s) => write!(f, "helper io error: {s}"),
        }
    }
}
impl std::error::Error for HelperError {}

struct Manager {
    client: Option<HelperClient>,
    /// Override for the helper exe path (tests / non-standard layouts).
    exe_override: Option<PathBuf>,
}

fn manager() -> &'static Mutex<Manager> {
    static M: OnceLock<Mutex<Manager>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Manager { client: None, exe_override: None }))
}

/// Point the manager at a specific `hidmaestro_helper.exe` (otherwise it's
/// discovered next to the current executable). Call once at startup if needed.
pub fn set_helper_exe(path: PathBuf) {
    if let Ok(mut m) = manager().lock() {
        m.exe_override = Some(path);
    }
}

/// Locate `hidmaestro_helper.exe` next to the running executable.
fn discover_helper_exe(m: &Manager) -> Result<PathBuf, HelperError> {
    if let Some(p) = &m.exe_override {
        return Ok(p.clone());
    }
    let exe = std::env::current_exe().map_err(|e| HelperError::Io(e.to_string()))?;
    let dir = exe.parent().unwrap_or_else(|| std::path::Path::new("."));
    let candidate = dir.join("hidmaestro_helper.exe");
    if candidate.exists() {
        Ok(candidate)
    } else {
        Err(HelperError::HelperMissing(candidate))
    }
}

/// Ensure the helper is spawned (elevated) and connected, returning a live
/// client guard. Spawns + connects on first use; reuses thereafter. If a prior
/// connection died, it re-spawns.
fn ensure_connected(m: &mut Manager) -> Result<(), HelperError> {
    // Fast path: an existing client that still answers Ping.
    if let Some(client) = m.client.as_mut() {
        if client.call(&Request::Ping).is_ok() {
            return Ok(());
        }
        m.client = None; // stale; fall through to respawn
    }

    let exe = discover_helper_exe(m)?;
    crate::helper_ipc::spawn_elevated_helper(&exe).map_err(|e| HelperError::Spawn(e.to_string()))?;
    // The helper takes a moment to accept UAC + start listening.
    let client = HelperClient::connect(8000).map_err(|e| HelperError::Connect(e.to_string()))?;
    m.client = Some(client);
    Ok(())
}

fn call(m: &mut Manager, req: &Request) -> Result<Response, HelperError> {
    ensure_connected(m)?;
    let client = m.client.as_mut().expect("connected");
    client.call(req).map_err(|e| HelperError::Io(e.to_string()))
}

/// Ask the helper whether the driver is installed (spawns it if needed).
pub fn status() -> Result<bool, HelperError> {
    let mut m = manager().lock().map_err(|_| HelperError::Io("poisoned".into()))?;
    match call(&mut m, &Request::Ping)? {
        Response::Status { driver_installed, .. } => Ok(driver_installed),
        Response::Error { message } => Err(HelperError::Helper(message)),
        _ => Err(HelperError::Helper("unexpected response to Ping".into())),
    }
}

/// Ensure the driver is installed via the helper (idempotent).
pub fn ensure_driver() -> Result<(), HelperError> {
    let mut m = manager().lock().map_err(|_| HelperError::Io("poisoned".into()))?;
    match call(&mut m, &Request::EnsureDriver)? {
        Response::Ok { .. } => Ok(()),
        Response::Error { message } => Err(HelperError::Helper(message)),
        _ => Err(HelperError::Helper("unexpected response to EnsureDriver".into())),
    }
}

/// Create a device for `profile_json` at `index` via the helper. Returns the
/// device instance id (used for [`destroy`]).
pub fn create(profile_json: &str, index: u32) -> Result<String, HelperError> {
    let mut m = manager().lock().map_err(|_| HelperError::Io("poisoned".into()))?;
    let req = Request::Create { profile_json: profile_json.to_string(), index };
    match call(&mut m, &req)? {
        Response::Created { instance_id, .. } => Ok(instance_id),
        Response::Error { message } => Err(HelperError::Helper(message)),
        _ => Err(HelperError::Helper("unexpected response to Create".into())),
    }
}

/// Destroy a previously-created device by instance id via the helper.
pub fn destroy(instance_id: &str) -> Result<(), HelperError> {
    let mut m = manager().lock().map_err(|_| HelperError::Io("poisoned".into()))?;
    let req = Request::Destroy { instance_id: instance_id.to_string() };
    match call(&mut m, &req)? {
        Response::Ok { .. } => Ok(()),
        Response::Error { message } => Err(HelperError::Helper(message)),
        _ => Err(HelperError::Helper("unexpected response to Destroy".into())),
    }
}
