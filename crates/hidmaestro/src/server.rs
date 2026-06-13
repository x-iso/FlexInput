//! Elevated HIDMaestro helper — named-pipe server (library form).
//!
//! This is the privileged worker the unelevated app talks to. It used to live
//! only in the `hidmaestro_helper` binary; it now lives here as
//! [`run_helper_server`] so the **main app** can be its own helper: launched
//! with `--hidmaestro-helper` it re-execs elevated and calls this function,
//! instead of shipping a separate exe. The standalone bin is kept as a thin
//! wrapper for tests.
//!
//! Responsibilities the unelevated app can't do itself:
//! - driver install (`SeLoadDriverPrivilege`),
//! - `Global\` section creation (`SeCreateGlobalPrivilege`),
//! - device-node create/teardown (admin SetupAPI),
//! - **lifecycle**: watch the parent app and, when persistence is off, tear
//!   down every device if the parent dies (so virtual pads never outlive the
//!   app) and clean up orphans left by a previous crash.
//!
//! Wire protocol: `helper_ipc` newline-JSON over a named pipe.

#![cfg(windows)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::deploy::ensure_driver_installed;
use crate::helper_ipc::{DeviceInfo, Request, Response, PIPE_NAME};
use crate::install::{hidmaestro_available, installed_inf_path};
use crate::orchestrator::{
    create_device_node, list_hidmaestro_devices, remove_all_hidmaestro_devices, remove_device_node,
};
use crate::shm::{InputSection, OutputSection};
use crate::Profile;

/// Append a one-line diagnostic to `flexinput-hidmaestro.log` next to the exe.
/// Temporary instrumentation for the input-path investigation. The helper runs
/// from the same exe path as the app, so both write the same file.
fn diag_log(line: &str) {
    use std::io::Write;
    let path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("flexinput-hidmaestro.log")));
    if let Some(path) = path {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
    eprintln!("{line}");
}

/// A live device the helper is keeping alive: its sections (mapped) + index +
/// the FlexInput device id that owns it (for in-session reclaim).
struct LiveDevice {
    _input: InputSection,
    _output: Option<OutputSection>,
    index: u32,
    device_id: String,
}

/// Process-wide helper state shared between the client loop and the
/// parent-watch thread.
struct HelperState {
    devices: Mutex<HashMap<String, LiveDevice>>,
    /// True once persistence has been resolved (via `Hello`). When false the
    /// helper removes all devices on parent death / shutdown.
    persist: AtomicBool,
    /// Set when the parent process dies; the accept loop notices and exits.
    parent_gone: AtomicBool,
    /// True once the helper has run its one-time startup orphan cleanup. The app
    /// reconnects (and re-sends `Hello`) several times per session — the pipe is
    /// per-connection but this helper process persists — so the cleanup must run
    /// only on the FIRST hello, never on reconnects, or it would wipe the very
    /// devices the app just created earlier in the session.
    did_startup_cleanup: AtomicBool,
}

impl HelperState {
    fn new() -> Self {
        HelperState {
            devices: Mutex::new(HashMap::new()),
            persist: AtomicBool::new(false),
            parent_gone: AtomicBool::new(false),
            did_startup_cleanup: AtomicBool::new(false),
        }
    }

    /// Tear down every device this helper created (used on shutdown when
    /// persistence is off). Best-effort.
    fn teardown_tracked(&self) {
        if let Ok(mut devs) = self.devices.lock() {
            for (id, _) in devs.drain() {
                let _ = remove_device_node(&id);
            }
        }
    }
}

/// Run the elevated helper server loop. Blocks until the client requests
/// shutdown, the parent process (`parent_pid`, when `Some`) dies, or the pipe
/// breaks irrecoverably.
///
/// `initial_persist` seeds the persistence flag for the window before the app's
/// `Hello` arrives; the `Hello` message is authoritative thereafter.
pub fn run_helper_server(parent_pid: Option<u32>, initial_persist: bool) {
    eprintln!(
        "[hidmaestro-helper] starting; pipe={PIPE_NAME} parent={parent_pid:?} persist={initial_persist}"
    );
    let state = Arc::new(HelperState::new());
    state.persist.store(initial_persist, Ordering::SeqCst);

    // Watch the parent: if it exits and persistence is off, tear everything
    // down and flip `parent_gone` so the accept loop unblocks and exits.
    if let Some(pid) = parent_pid {
        let st = state.clone();
        std::thread::spawn(move || watch_parent(pid, st));
    }

    loop {
        if state.parent_gone.load(Ordering::SeqCst) {
            break;
        }
        let pipe = match NamedPipeServer::create_and_wait(PIPE_NAME, &state) {
            Ok(Some(p)) => p,
            Ok(None) => break, // parent gone while waiting
            Err(e) => {
                eprintln!("[hidmaestro-helper] pipe error: {e}; exiting");
                break;
            }
        };
        if handle_client(pipe, &state) {
            eprintln!("[hidmaestro-helper] shutdown requested");
            break;
        }
    }

    // Exit cleanup: if persistence is off, ensure nothing is left behind —
    // remove everything HIDMaestro-owned, not just what we tracked (covers
    // sections the app reopened across reconnects).
    if !state.persist.load(Ordering::SeqCst) {
        state.teardown_tracked();
        let n = remove_all_hidmaestro_devices();
        if n > 0 {
            eprintln!("[hidmaestro-helper] cleaned up {n} device(s) on exit (persist off)");
        }
    }
    eprintln!("[hidmaestro-helper] stopped");
}

/// Poll the parent process handle; when it exits, react per persistence and
/// signal the accept loop to stop.
fn watch_parent(parent_pid: u32, state: Arc<HelperState>) {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0;
    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, parent_pid) };
    if handle.is_null() {
        // Can't observe the parent (already gone, or access denied). If we
        // can't watch it, fail safe: when persistence is off, don't strand
        // devices — but we also can't loop forever, so just return and rely on
        // explicit Shutdown / pipe-break.
        eprintln!("[hidmaestro-helper] could not open parent pid {parent_pid} to watch");
        return;
    }
    loop {
        let r = unsafe { WaitForSingleObject(handle, 1000) };
        if r == WAIT_OBJECT_0 {
            // Parent exited.
            eprintln!("[hidmaestro-helper] parent {parent_pid} exited");
            if !state.persist.load(Ordering::SeqCst) {
                state.teardown_tracked();
                let n = remove_all_hidmaestro_devices();
                eprintln!("[hidmaestro-helper] removed {n} device(s) after parent death");
            }
            state.parent_gone.store(true, Ordering::SeqCst);
            // Nudge the accept loop: connect to our own pipe so a blocked
            // ConnectNamedPipe returns and the loop re-checks `parent_gone`.
            wake_accept_loop();
            unsafe { CloseHandle(handle) };
            return;
        }
    }
}

/// Connect-then-close our own pipe to unblock a `ConnectNamedPipe` that's
/// waiting for a client, so the accept loop can notice `parent_gone`.
fn wake_accept_loop() {
    use std::os::windows::ffi::OsStrExt;
    let w: Vec<u16> = std::ffi::OsStr::new(PIPE_NAME)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    const GENERIC_RW: u32 = 0xC000_0000;
    const OPEN_EXISTING: u32 = 3;
    let h = unsafe {
        CreateFileW(w.as_ptr(), GENERIC_RW, 0, std::ptr::null_mut(), OPEN_EXISTING, 0, std::ptr::null_mut())
    };
    if h as isize != -1 {
        unsafe { CloseHandle(h) };
    }
}

/// Returns true if the client requested shutdown.
fn handle_client(pipe: NamedPipeServer, state: &Arc<HelperState>) -> bool {
    let reader_pipe = match pipe.try_clone() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let mut writer = pipe;
    let mut reader = BufReader::new(reader_pipe);

    loop {
        if state.parent_gone.load(Ordering::SeqCst) {
            return true;
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return false, // client disconnected
            Ok(_) => {}
            Err(_) => return false,
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                let _ = write_response(&mut writer, &Response::err(format!("bad request: {e}")));
                continue;
            }
        };

        let (resp, shutdown) = handle_request(req, state);
        let _ = write_response(&mut writer, &resp);
        if shutdown {
            return true;
        }
    }
}

fn handle_request(req: Request, state: &Arc<HelperState>) -> (Response, bool) {
    match req {
        Request::Hello { parent_pid: _, persist } => {
            let was = state.persist.swap(persist, Ordering::SeqCst);
            // When persistence is off, remove any leftovers present right now —
            // orphans from a previous crashed run, AND devices left behind by a
            // prior persist-ON run (the persist→off transition). Drop our held
            // section handles first so the removal isn't blocked by mapped views.
            //
            // CRITICAL: only clean up on the FIRST hello of this helper's life.
            // The app reconnects + re-greets several times per session (pipe is
            // per-connection, helper persists), and a per-hello wipe would delete
            // the devices created earlier this session — observed as "DS4 created
            // then vanished, only DualSense survived". Reconnect hellos just
            // refresh the persist flag.
            let first = !state.did_startup_cleanup.swap(true, Ordering::SeqCst);
            if first && !persist {
                let n = remove_all_hidmaestro_devices();
                diag_log(&format!(
                    "[helper] hello (startup): persist off; cleaned up {n} leftover device(s)/orphan child(ren)"
                ));
            } else if !persist && was != persist {
                // Persist toggled on→off mid-session (settings change): drop our
                // tracked sections and remove everything so the policy takes hold.
                state.teardown_tracked();
                let n = remove_all_hidmaestro_devices();
                diag_log(&format!(
                    "[helper] hello (persist on->off): cleaned up {n} device(s)"
                ));
            }
            (Response::ok(), false)
        }
        Request::Ping => (
            Response::Status {
                driver_installed: hidmaestro_available(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            false,
        ),
        Request::EnsureDriver => match ensure_driver_installed() {
            Ok(fresh) => (
                Response::Ok {
                    detail: Some(if fresh {
                        "driver installed".into()
                    } else {
                        "driver already present".into()
                    }),
                },
                false,
            ),
            Err(e) => (Response::err(format!("driver install failed: {e}")), false),
        },
        Request::Create { device_id, profile_json, index_hint } => {
            (handle_create(&device_id, &profile_json, index_hint, state), false)
        }
        Request::Destroy { instance_id } => (handle_destroy(&instance_id, state), false),
        Request::ListDevices => {
            let devices = list_hidmaestro_devices()
                .into_iter()
                .map(|d| DeviceInfo {
                    instance_id: d.instance_id,
                    index: d.index,
                    vid: d.vid,
                    pid: d.pid,
                    device_id: d.device_id,
                })
                .collect();
            (Response::Devices { devices }, false)
        }
        Request::Shutdown => (Response::ok(), true),
    }
}

fn handle_create(
    device_id: &str,
    profile_json: &str,
    index_hint: u32,
    state: &Arc<HelperState>,
) -> Response {
    let profile = match Profile::from_json(profile_json) {
        Ok(p) => p,
        Err(e) => return Response::err(format!("bad profile: {e}")),
    };
    if let Err(e) = ensure_driver_installed() {
        return Response::err(format!("driver not available: {e}"));
    }
    let inf = match installed_inf_path() {
        Some(p) => p,
        None => return Response::err("HIDMaestro INF not found after install"),
    };

    // RECLAIM (in-session): if we already track a device for this device_id,
    // return it — never create a duplicate.
    if let Ok(devs) = state.devices.lock() {
        if let Some((inst, live)) = devs.iter().find(|(_, d)| d.device_id == device_id) {
            diag_log(&format!(
                "[helper] reclaim (session) device_id={device_id} idx={} instance={inst}",
                live.index
            ));
            return Response::Created { instance_id: inst.clone(), index: live.index };
        }
    }

    // RECLAIM (cross-run): a persisted node in the system already owns this
    // device_id. Re-attach by mapping its sections at the recorded index — don't
    // create a second node.
    let existing = list_hidmaestro_devices();
    if let Some(found) = existing.iter().find(|d| d.device_id == device_id && !device_id.is_empty())
    {
        let input = match InputSection::create(found.index).or_else(|_| InputSection::open(found.index)) {
            Ok(s) => s,
            Err(e) => return Response::err(format!("reclaim input section idx={}: {e}", found.index)),
        };
        let output = OutputSection::create(found.index)
            .or_else(|_| OutputSection::open(found.index))
            .ok();
        if let Ok(mut devs) = state.devices.lock() {
            devs.insert(
                found.instance_id.clone(),
                LiveDevice { _input: input, _output: output, index: found.index, device_id: device_id.to_string() },
            );
        }
        diag_log(&format!(
            "[helper] reclaim (cross-run) device_id={device_id} idx={} instance={}",
            found.index, found.instance_id
        ));
        return Response::Created { instance_id: found.instance_id.clone(), index: found.index };
    }

    // ALLOCATE a globally-unique index: lowest free, considering both nodes
    // present in the system and indices we already hold this session.
    let index = allocate_index(&existing, state, index_hint);

    let input = match InputSection::create(index) {
        Ok(s) => s,
        Err(e) => return Response::err(format!("create input section idx={index}: {e}")),
    };
    let output = OutputSection::create(index).ok();

    match create_device_node(&profile, &inf.display().to_string(), index, device_id) {
        Ok(dev) => {
            if let Ok(mut devs) = state.devices.lock() {
                devs.insert(
                    dev.instance_id.clone(),
                    LiveDevice { _input: input, _output: output, index, device_id: device_id.to_string() },
                );
            }
            diag_log(&format!(
                "[helper] created device_id={device_id} vid={:04x} pid={:04x} idx={index} instance={}",
                profile.vid, profile.pid, dev.instance_id
            ));
            Response::Created { instance_id: dev.instance_id, index }
        }
        Err(e) => {
            diag_log(&format!("[helper] create FAILED device_id={device_id} idx={index}: {e}"));
            Response::err(format!("create device node: {e}"))
        }
    }
}

/// Pick the lowest controller index not in use — neither present in the system
/// (`existing`) nor held by us this session. `index_hint` is the app's legacy
/// per-kind number, used only to bias toward a stable choice when free.
fn allocate_index(
    existing: &[crate::orchestrator::ExistingDevice],
    state: &Arc<HelperState>,
    index_hint: u32,
) -> u32 {
    use std::collections::HashSet;
    let mut used: HashSet<u32> = existing.iter().map(|d| d.index).collect();
    if let Ok(devs) = state.devices.lock() {
        for d in devs.values() {
            used.insert(d.index);
        }
    }
    // Prefer the hint if it happens to be free (keeps single-device patches at 0).
    if !used.contains(&index_hint) {
        return index_hint;
    }
    (0u32..).find(|i| !used.contains(i)).unwrap_or(index_hint)
}

fn handle_destroy(instance_id: &str, state: &Arc<HelperState>) -> Response {
    let removed = match remove_device_node(instance_id) {
        Ok(g) => g,
        Err(e) => return Response::err(format!("remove device: {e}")),
    };
    if let Ok(mut devs) = state.devices.lock() {
        if let Some(dev) = devs.remove(instance_id) {
            let _ = dev.index;
        }
    }
    Response::Ok { detail: Some(format!("removed={removed}")) }
}

fn write_response(pipe: &mut NamedPipeServer, resp: &Response) -> std::io::Result<()> {
    let line = crate::helper_ipc::encode_line(resp);
    pipe.write_all(line.as_bytes())?;
    pipe.flush()
}

// ── Minimal blocking named-pipe SERVER (Win32) ──────────────────────────────
struct NamedPipeServer {
    handle: *mut c_void,
}
unsafe impl Send for NamedPipeServer {}

const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
const PIPE_WAIT: u32 = 0x0000_0000;
const PIPE_UNLIMITED_INSTANCES: u32 = 255;
const INVALID_HANDLE_VALUE: isize = -1;

#[link(name = "kernel32")]
extern "system" {
    fn CreateNamedPipeW(
        name: *const u16, open_mode: u32, pipe_mode: u32, max_instances: u32,
        out_buf: u32, in_buf: u32, default_timeout: u32, sec: *mut c_void,
    ) -> *mut c_void;
    fn ConnectNamedPipe(h: *mut c_void, ovl: *mut c_void) -> i32;
    fn DisconnectNamedPipe(h: *mut c_void) -> i32;
    fn ReadFile(h: *mut c_void, buf: *mut u8, len: u32, read: *mut u32, ovl: *mut c_void) -> i32;
    fn WriteFile(h: *mut c_void, buf: *const u8, len: u32, written: *mut u32, ovl: *mut c_void) -> i32;
    fn CloseHandle(h: *mut c_void) -> i32;
    fn DuplicateHandle(
        sp: *mut c_void, s: *mut c_void, tp: *mut c_void, t: *mut *mut c_void,
        a: u32, inh: i32, opt: u32,
    ) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn GetLastError() -> u32;
    fn CreateFileW(
        name: *const u16, access: u32, share: u32, sec: *mut c_void,
        disposition: u32, flags: u32, template: *mut c_void,
    ) -> *mut c_void;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn WaitForSingleObject(h: *mut c_void, ms: u32) -> u32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl NamedPipeServer {
    /// Create a pipe instance and block until a client connects. Returns
    /// `Ok(None)` if the parent went away while we were waiting (so the caller
    /// exits the loop instead of handling a phantom client).
    fn create_and_wait(name: &str, state: &Arc<HelperState>) -> std::io::Result<Option<Self>> {
        let w = wide(name);
        let h = unsafe {
            CreateNamedPipeW(
                w.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                std::ptr::null_mut(),
            )
        };
        if h as isize == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32));
        }
        let server = NamedPipeServer { handle: h };
        let ok = unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
        if ok == 0 {
            let e = unsafe { GetLastError() };
            // ERROR_PIPE_CONNECTED (535) — a client connected between create and
            // ConnectNamedPipe — is success.
            if e != 535 {
                return Err(std::io::Error::from_raw_os_error(e as i32));
            }
        }
        // If the parent died (the watch thread woke us via a throwaway connect),
        // report no client so the loop exits.
        if state.parent_gone.load(Ordering::SeqCst) {
            return Ok(None);
        }
        Ok(Some(server))
    }

    fn try_clone(&self) -> std::io::Result<NamedPipeServer> {
        const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;
        let mut dup: *mut c_void = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(), self.handle, GetCurrentProcess(),
                &mut dup, 0, 0, DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32));
        }
        Ok(NamedPipeServer { handle: dup })
    }
}

impl std::io::Read for NamedPipeServer {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0u32;
        let ok = unsafe { ReadFile(self.handle, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut()) };
        if ok == 0 {
            let e = unsafe { GetLastError() };
            if e == 109 || e == 233 {
                // BROKEN_PIPE / PIPE_NOT_CONNECTED → EOF.
                return Ok(0);
            }
            return Err(std::io::Error::from_raw_os_error(e as i32));
        }
        Ok(n as usize)
    }
}

impl std::io::Write for NamedPipeServer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut n = 0u32;
        let ok = unsafe { WriteFile(self.handle, buf.as_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut()) };
        if ok == 0 {
            return Err(std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32));
        }
        Ok(n as usize)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for NamedPipeServer {
    fn drop(&mut self) {
        unsafe {
            DisconnectNamedPipe(self.handle);
            CloseHandle(self.handle);
        }
    }
}
