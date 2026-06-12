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
    /// Override for the helper exe path (tests / standalone-helper layouts). When
    /// `None` (the shipping default) the app re-execs **itself** with
    /// `--hidmaestro-helper` so a single binary ships — no sibling exe.
    exe_override: Option<PathBuf>,
    /// Whether virtual devices should persist beyond the app's lifetime. Sent to
    /// the helper in `Hello`; drives parent-death teardown + orphan cleanup.
    persist: bool,
    /// True once `Hello` has been delivered on the current connection.
    greeted: bool,
}

fn manager() -> &'static Mutex<Manager> {
    static M: OnceLock<Mutex<Manager>> = OnceLock::new();
    M.get_or_init(|| {
        Mutex::new(Manager { client: None, exe_override: None, persist: false, greeted: false })
    })
}

/// Point the manager at a specific standalone `hidmaestro_helper.exe` instead of
/// re-execing the current binary. For tests / the Phase-4 gate only.
pub fn set_helper_exe(path: PathBuf) {
    if let Ok(mut m) = manager().lock() {
        m.exe_override = Some(path);
    }
}

/// Set whether virtual devices persist beyond the app's lifetime. Call at
/// startup (from the persistence setting) before creating devices. Re-sends
/// `Hello` on the next call if already connected.
pub fn set_persist(persist: bool) {
    if let Ok(mut m) = manager().lock() {
        if m.persist != persist {
            m.persist = persist;
            m.greeted = false; // force a fresh Hello to update the helper
        }
    }
}

/// CLI flag the app re-execs itself with to become the elevated helper.
pub const HELPER_FLAG: &str = "--hidmaestro-helper";

/// Resolve the command (exe + args) to spawn elevated. Default: the current
/// executable with `--hidmaestro-helper --parent-pid <us>`. An `exe_override`
/// runs that standalone helper exe instead (still passing the parent pid).
fn helper_command(m: &Manager) -> Result<(PathBuf, String), HelperError> {
    let pid = std::process::id();
    if let Some(p) = &m.exe_override {
        return Ok((p.clone(), format!("--parent-pid {pid}")));
    }
    let exe = std::env::current_exe().map_err(|e| HelperError::Io(e.to_string()))?;
    Ok((exe, format!("{HELPER_FLAG} --parent-pid {pid}")))
}

/// Ensure the helper is spawned (elevated) and connected, returning a live
/// client guard. Spawns + connects on first use; reuses thereafter. If a prior
/// connection died, it re-spawns. Sends `Hello` (parent pid + persistence) once
/// per connection.
fn ensure_connected(m: &mut Manager) -> Result<(), HelperError> {
    // Fast path: an existing client that still answers Ping.
    if let Some(client) = m.client.as_mut() {
        if client.call(&Request::Ping).is_ok() {
            send_hello_if_needed(m)?;
            return Ok(());
        }
        m.client = None; // stale; fall through to respawn
        m.greeted = false;
    }

    let (exe, args) = helper_command(m)?;
    crate::helper_ipc::spawn_elevated_helper_with_args(&exe, &args)
        .map_err(|e| HelperError::Spawn(e.to_string()))?;
    // The helper takes a moment to accept UAC + start listening.
    let client = HelperClient::connect(60_000).map_err(|e| HelperError::Connect(e.to_string()))?;
    m.client = Some(client);
    m.greeted = false;
    send_hello_if_needed(m)?;
    Ok(())
}

/// Deliver the `Hello` handshake (parent pid + persistence) once per connection.
fn send_hello_if_needed(m: &mut Manager) -> Result<(), HelperError> {
    if m.greeted {
        return Ok(());
    }
    let persist = m.persist;
    let client = m.client.as_mut().expect("connected");
    let hello = Request::Hello { parent_pid: std::process::id(), persist };
    match client.call(&hello).map_err(|e| HelperError::Io(e.to_string()))? {
        Response::Ok { .. } => {
            m.greeted = true;
            Ok(())
        }
        Response::Error { message } => Err(HelperError::Helper(message)),
        _ => Err(HelperError::Helper("unexpected response to Hello".into())),
    }
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

/// Create (or reclaim) a device for `device_id` from `profile_json` via the
/// helper. The helper allocates a globally-unique controller index (or returns
/// the existing one if `device_id` is already present). `index_hint` is the
/// app's legacy per-kind instance number, used only as a tiebreaker.
///
/// Returns `(instance_id, allocated_index)`. The app must open its SHM sections
/// at `allocated_index` — NOT at `index_hint` (that mismatch was the
/// devices-collide-on-index-0 bug).
pub fn create(
    device_id: &str,
    profile_json: &str,
    index_hint: u32,
) -> Result<(String, u32), HelperError> {
    let mut m = manager().lock().map_err(|_| HelperError::Io("poisoned".into()))?;
    let req = Request::Create {
        device_id: device_id.to_string(),
        profile_json: profile_json.to_string(),
        index_hint,
    };
    match call(&mut m, &req)? {
        Response::Created { instance_id, index } => Ok((instance_id, index)),
        Response::Error { message } => Err(HelperError::Helper(message)),
        _ => Err(HelperError::Helper("unexpected response to Create".into())),
    }
}

/// Enumerate HIDMaestro devices currently present (for reclaim-on-startup).
/// Spawns the helper if needed. Returns `(instance_id, index, vid, pid)`.
pub fn list_devices() -> Result<Vec<crate::helper_ipc::DeviceInfo>, HelperError> {
    let mut m = manager().lock().map_err(|_| HelperError::Io("poisoned".into()))?;
    match call(&mut m, &Request::ListDevices)? {
        Response::Devices { devices } => Ok(devices),
        Response::Error { message } => Err(HelperError::Helper(message)),
        _ => Err(HelperError::Helper("unexpected response to ListDevices".into())),
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
