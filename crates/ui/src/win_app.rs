//! Windows-only application-lifetime helpers used from the app binary's
//! `main`: single-instance enforcement and an off-screen guard for restoring a
//! persisted window position. Kept in the UI crate because that's where the
//! `windows-sys` dependency (and its feature set) already lives; `main.rs`
//! delegates here like it does for the relaunch/crash-log helpers.

/// Ensure only one interactive FlexInput instance runs per user session.
///
/// Returns `true` if this process should proceed to create its window, `false`
/// if another instance is already running (in which case that instance's window
/// is brought to the foreground and this process should exit immediately).
///
/// Uses a session-local named mutex whose mere existence signals "an instance is
/// up". The handle is intentionally leaked for the process lifetime (the OS
/// releases the name when the last holder exits).
///
/// **GPU-recovery interaction:** a GPU-loss / monitor-loss / renderer-cascade
/// relaunch spawns a fresh process *while the dying parent is still briefly
/// alive* (the parent `process::exit`s right after spawning). All those
/// relaunches set [`crate::GPU_RECOVERY_ENV`]. Such a child must NOT mistake its
/// own dying parent for "another instance", so when that env var is set we skip
/// the "already exists → bail" branch and just take (co-)ownership of the name.
/// The helper subprocess never reaches here — `main` short-circuits it first.
#[cfg(windows)]
pub fn try_become_primary_instance() -> bool {
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let is_recovery = std::env::var(crate::GPU_RECOVERY_ENV).is_ok();

    // Session-local (no `Global\`) — FlexInput's virtual devices + helper are
    // per-session, so single-instance is per-session too.
    let name: Vec<u16> = "FlexInput_SingleInstance_Mutex\0".encode_utf16().collect();
    let handle: HANDLE = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // Read the error IMMEDIATELY after CreateMutexW, before any other Win32 call.
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

    if handle.is_null() {
        // Couldn't create the mutex at all — fail OPEN (allow launch) rather
        // than lock the user out over a transient failure.
        return true;
    }

    // Keep the handle open for the whole process lifetime (never closed): while
    // any handle to the name is open, the name exists and other launches see it.
    static MUTEX_HANDLE: OnceLock<usize> = OnceLock::new();
    let _ = MUTEX_HANDLE.set(handle as usize);

    if already_exists && !is_recovery {
        focus_existing_window();
        return false;
    }
    true
}

/// Bring an already-running FlexInput window to the foreground (best-effort).
#[cfg(windows)]
fn focus_existing_window() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };
    // The main window's title is "FlexInput" (set in main's ViewportBuilder).
    let title: Vec<u16> = "FlexInput\0".encode_utf16().collect();
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if hwnd.is_null() {
        return;
    }
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
}

#[cfg(not(windows))]
pub fn try_become_primary_instance() -> bool {
    true
}

/// Coarse off-screen guard for a restored window position (logical points, as
/// egui/eframe report and consume them). Returns `Some(pos)` when the window's
/// top-left would land on (or near) the visible virtual desktop, or `None` when
/// the saved monitor is gone and the position should be dropped (let the OS
/// place the window) while keeping the saved size.
///
/// The virtual-screen metrics are physical pixels; we convert them to logical
/// points with the system DPI. This is exact on a single-DPI setup and a good
/// approximation across mixed-DPI monitors — enough to keep a window from
/// restoring completely off-screen after a monitor is unplugged.
#[cfg(windows)]
pub fn onscreen_position(pos: [f32; 2]) -> Option<[f32; 2]> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    // GetDpiForSystem lives under the same feature; fall back to 96 if it or the
    // metrics look bogus.
    let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() } as f32;
    let scale = if dpi >= 48.0 { dpi / 96.0 } else { 1.0 };
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN) as f32,
            GetSystemMetrics(SM_YVIRTUALSCREEN) as f32,
            GetSystemMetrics(SM_CXVIRTUALSCREEN) as f32,
            GetSystemMetrics(SM_CYVIRTUALSCREEN) as f32,
        )
    };
    if vw <= 0.0 || vh <= 0.0 {
        return Some(pos); // metrics unavailable — don't second-guess.
    }
    // Virtual desktop in logical points.
    let (lx, ly, lw, lh) = (vx / scale, vy / scale, vw / scale, vh / scale);
    // Require the window's top-left to sit within the desktop, allowing a small
    // margin (a title bar can hang slightly off the top/left and still be usable
    // — but a fully off-monitor position is rejected).
    const M: f32 = 100.0;
    let ok = pos[0] >= lx - M && pos[0] <= lx + lw - M && pos[1] >= ly - M && pos[1] <= ly + lh - M;
    if ok {
        Some(pos)
    } else {
        None
    }
}

#[cfg(not(windows))]
pub fn onscreen_position(pos: [f32; 2]) -> Option<[f32; 2]> {
    Some(pos)
}
