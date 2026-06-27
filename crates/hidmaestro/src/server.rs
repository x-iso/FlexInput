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
use crate::install::{hidmaestro_available, installed_inf_path, installed_xusb_inf_path};
use crate::orchestrator::{
    clear_hidmaestro_devices_and_wait, create_device_node, create_xusb_companion_node,
    list_hidmaestro_devices, remove_all_hidmaestro_devices, remove_device_node, RemovalBatch,
    wait_for_hid_child_started,
};
use crate::shm::{InputSection, OutputSection};
use crate::Profile;

/// One-line helper diagnostic. Fires per device create/cleanup and lifecycle
/// transition (NOT per frame), so it's safe to persist. The helper runs
/// ELEVATED in a separate process, so its stderr never reaches the app's
/// console — without a file the lifecycle is unobservable. Append to
/// `flexinput-hidmaestro.log` next to the exe (best-effort) AND echo to stderr.
fn diag_log(line: &str) {
    eprintln!("{line}");
    if let Some(path) = log_file_path() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "[{}] {line}", now_stamp());
        }
    }
}

/// `flexinput-hidmaestro.log` next to the current exe (the elevated helper).
fn log_file_path() -> Option<std::path::PathBuf> {
    let mut p = std::env::current_exe().ok()?;
    p.set_file_name("flexinput-hidmaestro.log");
    Some(p)
}

/// Coarse local timestamp (seconds since process start) for log correlation —
/// no chrono dependency; just enough to order events within a session.
fn now_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

/// A live device the helper is keeping alive: its sections (mapped) + index +
/// the FlexInput device id that owns it (for in-session reclaim).
struct LiveDevice {
    _input: InputSection,
    _output: Option<OutputSection>,
    index: u32,
    device_id: String,
    /// For Xbox360/XInput pads: the XUSB companion node's instance id (under
    /// `SWD\HIDMAESTRO`), created alongside the HID node. `None` for plain-HID pads.
    companion_instance_id: Option<String>,
    /// Owning handle for the SWD XUSB companion. Dropping it REMOVES the node
    /// (default `Handle` lifetime) — this is the reliable teardown on Win10 19045
    /// (the reconnect-and-downgrade path is a cosmetic no-op there). A helper crash
    /// drops it too, so nodes never leak as un-removable ParentPresent orphans.
    _companion_handle: Option<crate::orchestrator::SwdHandle>,
}

/// The HidHide masking the helper applied for the app, retained so teardown can
/// undo exactly what we changed: remove only the device ids WE added to the
/// blacklist (leaving any a user set via other tools) and, if our apply switched
/// HidHide on, switch it back off. Cleared on parent-death / shutdown so a closed
/// app never leaves a user's controllers hidden.
#[derive(Default, Clone)]
struct HidHideApplied {
    our_blacklist: Vec<String>,
    we_activated: bool,
}

/// Process-wide helper state shared between the client loop and the
/// parent-watch thread.
struct HelperState {
    devices: Mutex<HashMap<String, LiveDevice>>,
    /// HidHide masking applied on the app's behalf; `None` until first apply.
    hidhide_applied: Mutex<Option<HidHideApplied>>,
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
    /// The pid this helper currently serves. Updated by every `Hello` so a helper
    /// ADOPTED by a newer app (see `helper::ensure_connected`) follows that app
    /// instead of tearing down when the original parent dies. The watch thread
    /// reads this: when its watched handle signals, it only tears down if the pid
    /// that died is STILL the one we serve; otherwise it re-opens the watch for
    /// the new pid. `0` = no parent (watch idles). Guarantees ONE helper across a
    /// close→reopen / overlap instead of two fighting over device nodes.
    parent_pid: std::sync::atomic::AtomicU32,
}

impl HelperState {
    fn new() -> Self {
        HelperState {
            devices: Mutex::new(HashMap::new()),
            hidhide_applied: Mutex::new(None),
            persist: AtomicBool::new(false),
            parent_gone: AtomicBool::new(false),
            did_startup_cleanup: AtomicBool::new(false),
            parent_pid: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Tear down every device this helper created (used on shutdown when
    /// persistence is off). Best-effort.
    fn teardown_tracked(&self) {
        if let Ok(mut devs) = self.devices.lock() {
            // One shared ALLCLASSES info set for the whole teardown — removing
            // several tracked pads (each a HID node + maybe a companion, each
            // with a HID child) otherwise re-enumerated every device in the
            // system per node, which was the bulk of the exit hang.
            let batch = RemovalBatch::new();
            for (id, dev) in devs.drain() {
                // Drop the SWD companion's held handle FIRST — that's what removes
                // the companion node (default Handle lifetime). Do it explicitly so
                // the companion is gone before we remove the HID node.
                drop(dev._companion_handle);
                // Fallback for a companion with no live handle (legacy/reclaimed).
                if let Some(cid) = &dev.companion_instance_id {
                    let _ = batch.remove(cid);
                }
                let _ = batch.remove(&id);
            }
        }
    }

    /// Undo any HidHide masking we applied: drop the device ids WE added from the
    /// blacklist (keeping any a user set elsewhere) and, if our apply switched
    /// HidHide on, switch it back off. Best-effort; runs on parent-death and clean
    /// exit (always — masking physical pads must never outlive the app, regardless
    /// of the device-persist setting).
    fn clear_hidhide(&self) {
        let applied = self.hidhide_applied.lock().ok().and_then(|mut g| g.take());
        let Some(applied) = applied else { return };
        if applied.our_blacklist.is_empty() && !applied.we_activated {
            return;
        }
        let Some(hh) = crate::hidhide::HidHide::open() else { return };
        if !applied.our_blacklist.is_empty() {
            let ours: std::collections::HashSet<String> =
                applied.our_blacklist.iter().map(|s| s.to_uppercase()).collect();
            let kept: Vec<String> = hh
                .blacklist()
                .into_iter()
                .filter(|s| !ours.contains(&s.to_uppercase()))
                .collect();
            hh.set_blacklist(&kept);
        }
        if applied.we_activated {
            hh.set_active(false);
        }
        diag_log("[helper] cleared HidHide masking on teardown");
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

    // Cross-process single-helper lock. Block until any prior helper has fully
    // exited (or died) before we touch any device — so a rapid relaunch after a
    // crash/force-kill can't run two helpers concurrently and clobber each
    // other's nodes. Held for this whole function (released on return). 10s is
    // generous vs the worst-case prior teardown; on timeout we proceed (the
    // startup clear-and-wait + per-Create reclaim are backstops).
    let _singleton = HelperSingleton::acquire(10_000);

    let state = Arc::new(HelperState::new());
    state.persist.store(initial_persist, Ordering::SeqCst);
    if let Some(pid) = parent_pid {
        state.parent_pid.store(pid, Ordering::SeqCst);
    }

    // Watch the parent: if it exits and persistence is off, tear everything
    // down and flip `parent_gone` so the accept loop unblocks and exits. The
    // watch follows `state.parent_pid`, which a later `Hello` can re-target if a
    // newer app adopts this helper (keeps it a single shared helper).
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

    // Exit cleanup: if persistence is off, remove this helper's OWN tracked
    // devices. (Was a system-wide remove_all; scoped to tracked for the same
    // reason as parent-death teardown — a concurrently-spawned new helper's
    // fresh nodes must not be clobbered. Untracked orphans are reclaimed by the
    // next helper's startup `clear_hidmaestro_devices_and_wait`.)
    let persist_at_exit = state.persist.load(Ordering::SeqCst);
    diag_log(&format!("[helper] exit: persist={persist_at_exit}"));
    if !persist_at_exit {
        state.teardown_tracked();
        diag_log("[helper] exit cleanup (persist off): removed tracked device(s)");
    }
    // Always undo HidHide masking on exit (independent of device persistence).
    state.clear_hidhide();
    diag_log("[helper] stopped");
}

/// Watch the CURRENT parent process; when it dies, tear down (per persistence)
/// and stop the accept loop — UNLESS the helper was meanwhile adopted by a newer
/// app (a `Hello` updated `state.parent_pid`), in which case re-target to the new
/// pid instead of exiting. This is what guarantees a SINGLE helper across a
/// close→reopen / overlap: a newer app adopts the live helper and re-points it,
/// so the old parent's death never strands the new app's devices, and we never
/// spawn a second helper to fight the first.
fn watch_parent(initial_pid: u32, state: Arc<HelperState>) {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0;

    let mut cur_pid = initial_pid;
    let mut handle = unsafe { OpenProcess(SYNCHRONIZE, 0, cur_pid) };
    if handle.is_null() {
        eprintln!("[hidmaestro-helper] could not open parent pid {cur_pid} to watch");
        return;
    }
    loop {
        // Re-target if a newer app adopted us (Hello changed the served pid).
        let served = state.parent_pid.load(Ordering::SeqCst);
        if served != 0 && served != cur_pid {
            diag_log(&format!("[helper] re-targeting parent {cur_pid} -> {served} (adopted)"));
            unsafe { CloseHandle(handle) };
            cur_pid = served;
            handle = unsafe { OpenProcess(SYNCHRONIZE, 0, cur_pid) };
            if handle.is_null() {
                // New parent already gone — loop will treat it as dead below via
                // a null handle: re-check served pid; if still this, tear down.
                eprintln!("[hidmaestro-helper] could not open adopted parent {cur_pid}");
                // Fall through: if served is still cur_pid and unopenable, exit
                // like a dead parent. Re-open attempt next iteration covers a
                // transient race.
                std::thread::sleep(std::time::Duration::from_millis(200));
                if state.parent_pid.load(Ordering::SeqCst) == cur_pid {
                    handle_parent_death(cur_pid, &state);
                    return;
                }
                continue;
            }
        }

        let r = unsafe { WaitForSingleObject(handle, 1000) };
        if r != WAIT_OBJECT_0 {
            continue; // timeout: still alive, re-check re-target + wait again
        }
        // The watched handle signalled (cur_pid exited). But if we were adopted in
        // the meantime, the death we just saw is the OLD parent's — don't tear
        // down; loop re-targets to the new served pid.
        let served_now = state.parent_pid.load(Ordering::SeqCst);
        if served_now != 0 && served_now != cur_pid {
            diag_log(&format!("[helper] watched {cur_pid} died but adopted by {served_now}; not tearing down"));
            unsafe { CloseHandle(handle) };
            cur_pid = served_now;
            handle = unsafe { OpenProcess(SYNCHRONIZE, 0, cur_pid) };
            if handle.is_null() {
                eprintln!("[hidmaestro-helper] could not open adopted parent {cur_pid}");
                if state.parent_pid.load(Ordering::SeqCst) == cur_pid {
                    handle_parent_death(cur_pid, &state);
                    return;
                }
            }
            continue;
        }
        // The app we actually serve died → tear down and stop.
        handle_parent_death(cur_pid, &state);
        unsafe { CloseHandle(handle) };
        return;
    }
}

/// React to the served parent's death: mark `parent_gone` FIRST (so the accept
/// loop's race guard rejects new work immediately — a multi-second teardown must
/// not service a new app's Create on this dying helper), then tear down our own
/// tracked devices when persistence is off, and nudge the accept loop to exit.
fn handle_parent_death(parent_pid: u32, state: &Arc<HelperState>) {
    let persist_now = state.persist.load(Ordering::SeqCst);
    state.parent_gone.store(true, Ordering::SeqCst);
    diag_log(&format!("[helper] parent {parent_pid} exited; persist={persist_now}"));
    if !persist_now {
        // Remove ONLY this helper's own tracked devices — NOT a system-wide
        // remove_all. Orphans from an abrupt prior exit are reclaimed by the next
        // helper's startup `clear_hidmaestro_devices_and_wait`.
        state.teardown_tracked();
        diag_log("[helper] removed tracked device(s) after parent death");
    }
    // Always undo HidHide masking — physical pads must never stay hidden once the
    // app is gone, independent of the device-persist policy.
    state.clear_hidhide();
    wake_accept_loop();
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

        // RACE GUARD: the parent may have died while we were blocked in
        // read_line above. If this helper has entered its death sequence
        // (parent_gone set by watch_parent), it is about to exit and tear down —
        // it must NOT service new work, or it would accept a Create, build the
        // device, then immediately abandon it on exit. That happened on a rapid
        // relaunch: the NEW app connected to the OLD (dying) helper via the
        // fast-path Ping, its DS4 was created on the dying helper and then swept,
        // leaving only DualSense (created on the respawned helper). Reject the
        // request so the client's connection breaks and it respawns a clean
        // helper. (Re-checked here, not just at the loop top, because the wait is
        // inside read_line.)
        if state.parent_gone.load(Ordering::SeqCst) {
            let _ = write_response(
                &mut writer,
                &Response::err("helper shutting down; reconnect"),
            );
            diag_log("[helper] rejected request after parent death (race guard)");
            return true;
        }

        let (resp, shutdown) = handle_request(req, state);
        let _ = write_response(&mut writer, &resp);
        if shutdown {
            return true;
        }
    }
}

fn handle_request(req: Request, state: &Arc<HelperState>) -> (Response, bool) {
    match req {
        Request::Hello { parent_pid, persist } => {
            // Re-target the parent watch to whoever is greeting us now. On the
            // first Hello this just confirms the spawn-time pid; on a later Hello
            // from a NEWER app (adoption — see helper::ensure_connected) it
            // re-points the watch so this single helper follows the current app
            // and won't tear down when the original parent later dies. (The race
            // guard above means we only get here while still alive — parent_gone
            // unset — so adoption can't collide with our own teardown.)
            if parent_pid != 0 {
                let prev = state.parent_pid.swap(parent_pid, Ordering::SeqCst);
                if prev != parent_pid {
                    diag_log(&format!("[helper] hello: now serving parent {parent_pid} (was {prev})"));
                }
            }
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
            diag_log(&format!(
                "[helper] hello: persist={persist} (was={was}) first={first}"
            ));
            if first && !persist {
                // Sweep leftovers AND wait until the system is verified clear
                // before returning Ok — so the app's first Create can't race a
                // prior (force-killed/crashed) helper that's still mid-teardown
                // and orphan a half-removed node. Idempotent vs the old helper's
                // own sweep. Bounded so a stuck node can't hang startup; an
                // un-cleared node falls to the per-Create reclaim path.
                let clear = clear_hidmaestro_devices_and_wait(std::time::Duration::from_secs(5));
                diag_log(&format!(
                    "[helper] hello (startup): persist off; swept leftovers, clear={clear}"
                ));
            }
            // A live toggle changes only the EXIT policy (read at exit /
            // parent-death). It must NOT tear down a device the user is using
            // right now — toggling persist off mid-session keeps live devices and
            // simply removes them when the app closes; toggling on flips the exit
            // policy to "keep". The previous on->off teardown here yanked a live
            // controller out from under a running game, so it was removed.
            let _ = was;
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
        Request::ReinstallDriver => {
            // Live nodes can pin the driver and make /delete-driver fail, so tear
            // them all down first (the app re-creates them after this returns).
            state.teardown_tracked();
            let n = remove_all_hidmaestro_devices();
            diag_log(&format!("[helper] reinstall: removed {n} device(s) before driver swap"));
            match crate::deploy::reinstall_driver_force() {
                Ok(()) => (Response::Ok { detail: Some("driver reinstalled".into()) }, false),
                Err(e) => (Response::err(format!("driver reinstall failed: {e}")), false),
            }
        }
        Request::UninstallDriver => {
            // Live nodes pin the driver and make /delete-driver fail; tear them all
            // down first, then remove the package(s).
            state.teardown_tracked();
            let n = remove_all_hidmaestro_devices();
            diag_log(&format!("[helper] uninstall: removed {n} device(s) before driver removal"));
            match crate::deploy::uninstall_driver() {
                Ok(()) => (Response::Ok { detail: Some("driver uninstalled".into()) }, false),
                Err(e) => (Response::err(format!("driver uninstall failed: {e}")), false),
            }
        }
        Request::Create { device_id, profile_json, index_hint, poll_interval_ms } => {
            (handle_create(&device_id, &profile_json, index_hint, poll_interval_ms, state), false)
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
        Request::HidHideApply { blacklist, whitelist, active } => {
            (handle_hidhide_apply(blacklist, whitelist, active, state), false)
        }
        Request::Shutdown => (Response::ok(), true),
    }
}

/// Apply a HidHide masking config elevated, and record what we changed so
/// teardown can undo it. Returns the read-back state (or `present: false` when
/// the HidHide driver isn't installed).
fn handle_hidhide_apply(
    blacklist: Vec<String>,
    whitelist: Vec<String>,
    active: bool,
    state: &Arc<HelperState>,
) -> Response {
    let Some(hh) = crate::hidhide::HidHide::open() else {
        diag_log("[helper] hidhide apply: driver not present");
        return Response::HidHideState { present: false, active: false, hidden: vec![] };
    };
    let was_active = hh.is_active();
    // Keep FlexInput (+ the helper) able to see the pads it hides.
    if !whitelist.is_empty() {
        hh.ensure_whitelisted(&whitelist);
    }
    // MERGE rather than replace: a user may run HidHide with their own blacklist
    // entries from other tools. Drop only OUR previous entries, then add our new
    // ones — never clobber third-party entries. (`blacklist` is our desired set,
    // not the whole list.)
    let prev_ours: Vec<String> = state
        .hidhide_applied
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|a| a.our_blacklist.clone()))
        .unwrap_or_default();
    let prev_set: std::collections::HashSet<String> =
        prev_ours.iter().map(|s| s.to_uppercase()).collect();
    let new_set: std::collections::HashSet<String> =
        blacklist.iter().map(|s| s.to_uppercase()).collect();
    let mut merged: Vec<String> = hh
        .blacklist()
        .into_iter()
        // keep entries that are neither a stale one of ours nor a fresh one of ours
        // (fresh ones are re-added below to dedup)
        .filter(|s| {
            let up = s.to_uppercase();
            !prev_set.contains(&up) && !new_set.contains(&up)
        })
        .collect();
    merged.extend(blacklist.iter().cloned());
    hh.set_blacklist(&merged);
    hh.set_active(active);
    // Record what we applied (sticky `we_activated`: once we switched it on, we own
    // switching it off at teardown even if a later apply leaves it on).
    if let Ok(mut g) = state.hidhide_applied.lock() {
        let prev_activated = g.as_ref().map(|a| a.we_activated).unwrap_or(false);
        *g = Some(HidHideApplied {
            our_blacklist: blacklist.clone(),
            we_activated: prev_activated || (active && !was_active),
        });
    }
    let hidden = hh.blacklist();
    let active_now = hh.is_active();
    diag_log(&format!(
        "[helper] hidhide apply: active={active_now} hidden={} (was_active={was_active})",
        hidden.len()
    ));
    Response::HidHideState { present: true, active: active_now, hidden }
}

fn handle_create(
    device_id: &str,
    profile_json: &str,
    index_hint: u32,
    poll_interval_ms: u32,
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
    // Match the HID gamepad node (it drives the SHM section); the XUSB companion
    // shares the same device_id/index but is a separate System-class node.
    if let Some(found) = existing
        .iter()
        .find(|d| d.device_id == device_id && !device_id.is_empty() && !d.is_companion)
    {
        let mut input = match InputSection::create(found.index).or_else(|_| InputSection::open(found.index)) {
            Ok(s) => s,
            Err(e) => return Response::err(format!("reclaim input section idx={}: {e}", found.index)),
        };
        // Seed a neutral, full-battery frame (same rationale as the create path)
        // so a host reading right after reclaim doesn't catch a zeroed section.
        {
            let neutral = crate::encode::encode_report(&profile, &crate::encode::GamepadState::neutral());
            let gip = if profile.requires_xusb_companion {
                Some(crate::encode::gip_from_state(&crate::encode::GamepadState::neutral()))
            } else {
                None
            };
            input.write_frame(&neutral, gip.as_ref());
        }
        let output = OutputSection::create(found.index)
            .or_else(|_| OutputSection::open(found.index))
            .ok();
        // Re-discover any surviving XUSB companion at this index (best-effort).
        // NOTE: with the held-handle lifetime model, SWD companions are removed
        // when the creating helper exits, so a cross-session survivor is not
        // expected here; we can't re-acquire its owning handle either. Kept for
        // the in-session case + any legacy ROOT companion.
        let companion_instance_id = existing
            .iter()
            .find(|d| d.is_companion && d.index == found.index)
            .map(|d| d.instance_id.clone());
        if let Ok(mut devs) = state.devices.lock() {
            devs.insert(
                found.instance_id.clone(),
                LiveDevice {
                    _input: input,
                    _output: output,
                    index: found.index,
                    device_id: device_id.to_string(),
                    companion_instance_id,
                    _companion_handle: None,
                },
            );
        }
        // The surviving node's driver lost its section handle when the previous
        // helper exited; we just re-created the section above. Wait for the HID
        // child to be Started again before returning so the app's first writes
        // land on a listening driver — otherwise the reclaimed device can look
        // dead until another relaunch (the "fails to redeploy some" symptom).
        wait_for_hid_child_started(&found.instance_id, 5000);
        diag_log(&format!(
            "[helper] reclaim (cross-run) device_id={device_id} idx={} instance={}",
            found.index, found.instance_id
        ));
        return Response::Created { instance_id: found.instance_id.clone(), index: found.index };
    }

    // ALLOCATE a globally-unique index: lowest free, considering both nodes
    // present in the system and indices we already hold this session.
    let index = allocate_index(&existing, state, index_hint);

    // Stamp the XUSB companion's input-pump period (PollIntervalMs) into the
    // device's config key BEFORE the node is created, so the companion driver
    // reads it at CompanionDeviceAdd. Derived from the app's polling-rate
    // setting; clamped 1..8 ms (1000..125 Hz). 0 (older app / non-XInput) =>
    // leave unset so the driver keeps its 8ms (125Hz) default. Only meaningful
    // for XInput profiles, but harmless to write otherwise.
    if profile.requires_xusb_companion && poll_interval_ms > 0 {
        crate::orchestrator::write_poll_interval(index, poll_interval_ms);
    }

    let mut input = match InputSection::create(index) {
        Ok(s) => s,
        Err(e) => return Response::err(format!("create input section idx={index}: {e}")),
    };
    // Seed an initial NEUTRAL frame BEFORE the device node enumerates, so the very
    // first report a host can read already has centered sticks and a FULL battery.
    // The section is zeroed at create, and a zeroed Sony report decodes as battery
    // = 0 (critical) — if a host (Steam) read in the window between node-create and
    // FlexInput's first flush(), it flashed a spurious "low battery" warning. The
    // neutral encode runs `encode_extended`, which stamps battery to 100%.
    {
        let neutral = crate::encode::encode_report(&profile, &crate::encode::GamepadState::neutral());
        let gip = if profile.requires_xusb_companion {
            Some(crate::encode::gip_from_state(&crate::encode::GamepadState::neutral()))
        } else {
            None
        };
        input.write_frame(&neutral, gip.as_ref());
    }
    let output = OutputSection::create(index).ok();

    match create_device_node(&profile, &inf.display().to_string(), index, device_id) {
        Ok(dev) => {
            // For XInput/Xbox360 profiles, also create the XUSB companion node at
            // the SAME index. The companion is the ONE XInput identity; its sibling
            // HID node carries a non-Xbox HardwareID so it doesn't publish a second
            // XInput face. Non-fatal: if the companion fails the HID gamepad still
            // works (just not via XInput).
            let mut companion_instance_id = None;
            let mut companion_handle = None;
            if profile.requires_xusb_companion {
                match installed_xusb_inf_path() {
                    Some(xusb_inf) => {
                        match create_xusb_companion_node(&profile, &xusb_inf.display().to_string(), index) {
                            Ok((cid, handle)) => {
                                diag_log(&format!("[helper] created XUSB companion idx={index} instance={cid}"));
                                companion_instance_id = Some(cid);
                                companion_handle = Some(handle);
                            }
                            Err(e) => diag_log(&format!(
                                "[helper] XUSB companion create FAILED idx={index}: {e} (XInput off; HID pad still works)"
                            )),
                        }
                    }
                    None => diag_log("[helper] XUSB companion INF not found; XInput off"),
                }
            }
            if let Ok(mut devs) = state.devices.lock() {
                devs.insert(
                    dev.instance_id.clone(),
                    LiveDevice {
                        _input: input,
                        _output: output,
                        index,
                        device_id: device_id.to_string(),
                        companion_instance_id,
                        _companion_handle: companion_handle,
                    },
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
    // Pull the tracked device first so we can also remove its XUSB companion.
    let tracked = state.devices.lock().ok().and_then(|mut devs| devs.remove(instance_id));
    if let Some(mut dev) = tracked {
        // Drop the SWD companion's held handle — that removes the companion node
        // (default Handle lifetime). Fallback to an explicit remove for a
        // legacy/reclaimed companion with no live handle.
        dev._companion_handle.take();
        if let Some(cid) = &dev.companion_instance_id {
            let _ = remove_device_node(cid);
        }
    }
    let removed = match remove_device_node(instance_id) {
        Ok(g) => g,
        Err(e) => return Response::err(format!("remove device: {e}")),
    };
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

/// SECURITY_ATTRIBUTES layout (matches Win32; only the pointer matters here).
#[repr(C)]
struct SecurityAttributes {
    n_length: u32,
    lp_security_descriptor: *mut c_void,
    b_inherit_handle: i32,
}

const SDDL_REVISION_1: u32 = 1;

#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_sd: *const u16,
        revision: u32,
        sd: *mut *mut c_void,
        sd_size: *mut u32,
    ) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(h: *mut c_void) -> *mut c_void;
}

/// Self-freeing security descriptor + the `SECURITY_ATTRIBUTES` that points at
/// it. Built from an SDDL string. The helper runs elevated, so a pipe created
/// with the *default* DACL is only openable by other high-integrity / admin
/// processes — the unelevated app then fails `CreateFileW` with ERROR_ACCESS_
/// DENIED (os error 5), which is exactly the "dead devices on a normal launch"
/// bug. We instead grant the pipe to all authenticated users and label it Low
/// integrity so a medium-IL (normal) client can connect.
struct PipeSecurity {
    sd: *mut c_void,
    attrs: SecurityAttributes,
}

impl PipeSecurity {
    /// Build from SDDL. Returns `None` if the conversion fails (caller then
    /// falls back to a null/default descriptor — admin-launched still works).
    fn from_sddl(sddl: &str) -> Option<Self> {
        let w = wide(sddl);
        let mut sd: *mut c_void = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut(),
            )
        };
        if ok == 0 || sd.is_null() {
            return None;
        }
        let attrs = SecurityAttributes {
            n_length: std::mem::size_of::<SecurityAttributes>() as u32,
            lp_security_descriptor: sd,
            b_inherit_handle: 0,
        };
        Some(PipeSecurity { sd, attrs })
    }

    fn as_ptr(&mut self) -> *mut c_void {
        &mut self.attrs as *mut SecurityAttributes as *mut c_void
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        if !self.sd.is_null() {
            unsafe { LocalFree(self.sd) };
        }
    }
}

/// Pipe security: DACL grants GENERIC_ALL to Authenticated Users (AU) and the
/// local SYSTEM (SY); SACL sets a Low mandatory label (LW) so a medium-IL
/// client isn't blocked by the integrity check. This lets the normally-launched
/// (unelevated) app talk to the elevated helper.
const PIPE_SDDL: &str = "D:(A;;GA;;;AU)(A;;GA;;;SY)S:(ML;;NW;;;LW)";

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
    fn CreateMutexW(sec: *mut c_void, initial_owner: i32, name: *const u16) -> *mut c_void;
    fn ReleaseMutex(h: *mut c_void) -> i32;
}

/// Cross-process single-helper lock. Only one helper may perform device
/// operations at a time; a freshly-spawned helper BLOCKS here until any prior
/// helper has fully exited (releasing the mutex) or died (Windows marks the
/// mutex abandoned, which still grants ownership). This is what makes an abrupt
/// prior exit safe regardless of timing: without it, `PIPE_UNLIMITED_INSTANCES`
/// lets a dying old helper and a new one run concurrently, so the old helper's
/// late teardown (or a reused ROOT-node index) could clobber the new helper's
/// freshly-created devices. Acquired before any sweep/create; released on drop.
struct HelperSingleton {
    handle: *mut c_void,
}

impl HelperSingleton {
    /// Acquire the global helper lock, waiting up to `wait_ms` for a prior holder
    /// to release. Returns `None` only if the mutex can't be created at all
    /// (should never happen); a timeout still returns the guard (we proceed
    /// rather than strand the user — the startup `clear_and_wait` + per-Create
    /// reclaim remain as backstops). `Global\` so it spans sessions/elevation.
    fn acquire(wait_ms: u32) -> Option<Self> {
        const WAIT_OBJECT_0: u32 = 0;
        const WAIT_ABANDONED: u32 = 0x0000_0080;
        let name = wide(r"Global\FlexInputHIDMaestroHelperLock");
        let h = unsafe { CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr()) };
        if h.is_null() {
            eprintln!("[hidmaestro-helper] CreateMutexW failed: {}", unsafe { GetLastError() });
            return None;
        }
        let r = unsafe { WaitForSingleObject(h, wait_ms) };
        match r {
            WAIT_OBJECT_0 => diag_log("[helper] acquired singleton lock"),
            WAIT_ABANDONED => diag_log("[helper] acquired singleton lock (prior holder died)"),
            _ => diag_log(&format!(
                "[helper] singleton lock wait timed out (r={r:#x}); proceeding anyway"
            )),
        }
        Some(HelperSingleton { handle: h })
    }
}

impl Drop for HelperSingleton {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                ReleaseMutex(self.handle);
                CloseHandle(self.handle);
            }
        }
    }
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
        // Grant access to the unelevated app (see PIPE_SDDL). If building the SD
        // fails for any reason, fall back to the default (null) descriptor — an
        // admin-launched app still connects, matching the old behavior.
        let mut sec = PipeSecurity::from_sddl(PIPE_SDDL);
        let sec_ptr = sec.as_mut().map(|s| s.as_ptr()).unwrap_or(std::ptr::null_mut());
        let h = unsafe {
            CreateNamedPipeW(
                w.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                sec_ptr,
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
