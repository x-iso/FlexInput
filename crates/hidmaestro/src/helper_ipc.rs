//! IPC protocol + client for the elevated HIDMaestro helper.
//!
//! Device creation, section creation, and driver install all require elevation
//! (`SeLoadDriverPrivilege` / `SeCreateGlobalPrivilege`). FlexInput's main
//! process runs unelevated; it spawns a small `hidmaestro-helper.exe` elevated
//! once (single UAC prompt) and talks to it over a **named pipe** using
//! newline-delimited JSON.
//!
//! This module defines the wire types and a blocking [`HelperClient`]. The
//! server side lives in the helper binary (`src/bin/hidmaestro_helper.rs`).

use serde::{Deserialize, Serialize};

/// Named-pipe path the helper listens on. `\\.\pipe\...` — local only.
pub const PIPE_NAME: &str = r"\\.\pipe\flexinput-hidmaestro-helper";

/// A request from the main app to the elevated helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// First message after connect: tells the helper which process to watch
    /// (`parent_pid`) so it can self-destruct if the app dies, and whether
    /// virtual devices should `persist` beyond the app's lifetime. When
    /// `persist` is false the helper removes any leftover HIDMaestro devices it
    /// finds at this point (orphans from a previous crash) and arms parent-death
    /// teardown. Idempotent; the app sends it once per connection.
    Hello { parent_pid: u32, persist: bool },
    /// Liveness/handshake; helper replies `Status`.
    Ping,
    /// Ensure the HIDMaestro driver is installed (idempotent).
    EnsureDriver,
    /// Force a clean driver reinstall: remove every installed HIDMaestro package
    /// then install fresh. Tears down all live HIDMaestro device nodes first (a
    /// bound driver can refuse removal). Unlike `EnsureDriver` this runs even
    /// when the driver is already present — the "Reinstall drivers" recovery
    /// path. The app re-creates its devices afterward.
    ReinstallDriver,
    /// Create (or reclaim) a virtual device for `device_id` from `profile_json`.
    /// The helper allocates a **globally-unique** controller index — the app no
    /// longer guesses one (different device kinds previously both passed index 0
    /// and collided on `Global\HIDMaestroInput0`). If a device for `device_id`
    /// already exists (live this session, or persisted from a prior run), the
    /// helper returns that existing index instead of creating a duplicate
    /// (reclaim). The chosen index comes back in `Created.index`; the app opens
    /// its sections there. `index_hint` is the app's legacy per-kind instance
    /// number, used only as a tiebreaker / for migration.
    Create { device_id: String, profile_json: String, index_hint: u32 },
    /// Tear down the device at `instance_id` and release its sections.
    Destroy { instance_id: String },
    /// Enumerate HIDMaestro devices currently present in the system (for
    /// reclaim-on-startup when persistence is on). Includes devices created by a
    /// previous app run that this helper instance isn't tracking yet.
    ListDevices,
    /// Ask the helper to exit (releases everything).
    Shutdown,
}

/// A reply from the helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    /// Generic ok with optional human-readable detail.
    Ok { detail: Option<String> },
    /// Helper status: whether the driver is currently installed.
    Status { driver_installed: bool, version: String },
    /// A device was created.
    Created { instance_id: String, index: u32 },
    /// Enumerated HIDMaestro devices currently present (reply to `ListDevices`).
    Devices { devices: Vec<DeviceInfo> },
    /// An error occurred handling the request.
    Error { message: String },
}

/// One HIDMaestro device the helper knows about (present in the system).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Full PnP instance id, e.g. `ROOT\HIDClass\0001`.
    pub instance_id: String,
    /// `ControllerIndex` stamped on the node's Device Parameters.
    pub index: u32,
    /// USB vendor id from the node's per-instance config (0 if unknown).
    pub vid: u16,
    /// USB product id from the node's per-instance config (0 if unknown).
    pub pid: u16,
    /// FlexInput device id that owns this controller (e.g. `virtual.hm.ds4`),
    /// recorded in the per-instance config so the app can reclaim the right
    /// device across runs. Empty if unknown (created by an older build).
    #[serde(default)]
    pub device_id: String,
}

impl Response {
    pub fn ok() -> Self {
        Response::Ok { detail: None }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Response::Error { message: msg.into() }
    }
}

/// Serialize a request as a single newline-terminated JSON line.
pub fn encode_line<T: Serialize>(value: &T) -> String {
    let mut s = serde_json::to_string(value).unwrap_or_else(|e| {
        format!(r#"{{"result":"error","message":"encode failed: {e}"}}"#)
    });
    s.push('\n');
    s
}

// ── Blocking named-pipe client (Windows) ────────────────────────────────────
#[cfg(windows)]
mod client {
    use super::*;
    use std::ffi::c_void;
    use std::io::{BufRead, BufReader, Write};

    /// Blocking client over the helper's named pipe. Connect after the helper
    /// has been spawned elevated (see [`spawn_elevated_helper`]).
    pub struct HelperClient {
        pipe: PipeStream,
    }

    impl HelperClient {
        /// Connect to a helper already listening on [`PIPE_NAME`], retrying for
        /// up to `timeout_ms` (the helper may still be starting / accepting UAC).
        pub fn connect(timeout_ms: u64) -> std::io::Result<Self> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
            loop {
                match PipeStream::connect(PIPE_NAME) {
                    Ok(pipe) => return Ok(HelperClient { pipe }),
                    Err(e) => {
                        if std::time::Instant::now() >= deadline {
                            return Err(e);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }
        }

        /// Send a request and read one response line.
        pub fn call(&mut self, req: &Request) -> std::io::Result<Response> {
            let line = encode_line(req);
            self.pipe.write_all(line.as_bytes())?;
            self.pipe.flush()?;
            let mut reader = BufReader::new(self.pipe.try_clone()?);
            let mut resp = String::new();
            reader.read_line(&mut resp)?;
            serde_json::from_str(resp.trim()).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad response: {e}"))
            })
        }
    }

    /// Minimal blocking named-pipe stream (client side) over CreateFileW.
    pub struct PipeStream {
        handle: *mut c_void,
    }
    unsafe impl Send for PipeStream {}

    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const OPEN_EXISTING: u32 = 3;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            sec: *mut c_void,
            disposition: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn ReadFile(h: *mut c_void, buf: *mut u8, len: u32, read: *mut u32, ovl: *mut c_void) -> i32;
        fn WriteFile(h: *mut c_void, buf: *const u8, len: u32, written: *mut u32, ovl: *mut c_void) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
        fn DuplicateHandle(
            src_proc: *mut c_void, src: *mut c_void, tgt_proc: *mut c_void,
            tgt: *mut *mut c_void, access: u32, inherit: i32, options: u32,
        ) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn GetLastError() -> u32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    impl PipeStream {
        pub fn connect(name: &str) -> std::io::Result<Self> {
            let w = wide(name);
            let h = unsafe {
                CreateFileW(
                    w.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if h as isize == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32));
            }
            Ok(PipeStream { handle: h })
        }

        pub fn try_clone(&self) -> std::io::Result<PipeStream> {
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
            Ok(PipeStream { handle: dup })
        }
    }

    impl std::io::Read for PipeStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut n = 0u32;
            let ok = unsafe { ReadFile(self.handle, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut()) };
            if ok == 0 {
                let e = unsafe { GetLastError() };
                // ERROR_BROKEN_PIPE (109) → clean EOF.
                if e == 109 {
                    return Ok(0);
                }
                return Err(std::io::Error::from_raw_os_error(e as i32));
            }
            Ok(n as usize)
        }
    }

    impl std::io::Write for PipeStream {
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

    impl Drop for PipeStream {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    /// Spawn `helper_exe` elevated via ShellExecuteW("runas") with no args.
    /// Triggers one UAC prompt.
    pub fn spawn_elevated_helper(helper_exe: &std::path::Path) -> std::io::Result<()> {
        spawn_elevated_helper_with_args(helper_exe, "")
    }

    /// Spawn `helper_exe` elevated via ShellExecuteW("runas") with `args` (a
    /// command-line string). Triggers one UAC prompt. Used to re-exec the app
    /// itself as the helper (`--hidmaestro-helper --parent-pid N`). The helper
    /// then listens on [`PIPE_NAME`]; connect with [`HelperClient::connect`].
    pub fn spawn_elevated_helper_with_args(
        helper_exe: &std::path::Path,
        args: &str,
    ) -> std::io::Result<()> {
        use std::os::windows::ffi::OsStrExt;
        let verb = wide("runas");
        let file: Vec<u16> = helper_exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let params = wide(args);
        // SW_HIDE (0): the helper is a console-less worker; don't flash a window.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                if args.is_empty() { std::ptr::null() } else { params.as_ptr() },
                std::ptr::null(),
                0, // SW_HIDE
            )
        };
        // ShellExecuteW returns >32 on success.
        if (result as isize) <= 32 {
            return Err(std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32));
        }
        Ok(())
    }

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void, verb: *const u16, file: *const u16,
            params: *const u16, dir: *const u16, show: i32,
        ) -> *mut c_void;
    }
}

#[cfg(windows)]
pub use client::{
    spawn_elevated_helper, spawn_elevated_helper_with_args, HelperClient, PipeStream,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips_json() {
        let req = Request::Create {
            device_id: "virtual.hm.ds4".into(),
            profile_json: "{}".into(),
            index_hint: 2,
        };
        let line = encode_line(&req);
        assert!(line.ends_with('\n'));
        let back: Request = serde_json::from_str(line.trim()).unwrap();
        match back {
            Request::Create { index_hint, device_id, .. } => {
                assert_eq!(index_hint, 2);
                assert_eq!(device_id, "virtual.hm.ds4");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_roundtrips_json() {
        let resp = Response::Created { instance_id: r"ROOT\HIDClass\0000".into(), index: 0 };
        let line = encode_line(&resp);
        let back: Response = serde_json::from_str(line.trim()).unwrap();
        match back {
            Response::Created { instance_id, .. } => assert_eq!(instance_id, r"ROOT\HIDClass\0000"),
            _ => panic!("wrong variant"),
        }
    }
}
