//! Elevated HIDMaestro helper — named-pipe server.
//!
//! Runs elevated (spawned by the main app via `spawn_elevated_helper`). Owns the
//! privileged operations the unelevated app can't do: driver install, `Global\`
//! section creation, and device-node create/teardown. Speaks the
//! `helper_ipc` newline-JSON protocol over a named pipe.
//!
//! Lifetime: it keeps each created device's `InputSection`/`OutputSection`
//! handles (and the devnode) alive until a matching `Destroy` (or process exit),
//! because closing the sections would unmap the shared memory the driver reads.
//!
//! Build: `cargo build -p flexinput-hidmaestro --features helper-bin --bin hidmaestro_helper`

#![cfg(windows)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::io::{BufRead, BufReader, Write};

use flexinput_hidmaestro::deploy::ensure_driver_installed;
use flexinput_hidmaestro::helper_ipc::{Request, Response, PIPE_NAME};
use flexinput_hidmaestro::install::{hidmaestro_available, installed_inf_path};
use flexinput_hidmaestro::orchestrator::{create_device_node, remove_device_node};
use flexinput_hidmaestro::shm::{InputSection, OutputSection};
use flexinput_hidmaestro::Profile;

/// A live device the helper is keeping alive: its sections (mapped) + index.
struct LiveDevice {
    _input: InputSection,
    _output: Option<OutputSection>,
    index: u32,
}

fn main() {
    eprintln!("[hidmaestro-helper] starting; listening on {PIPE_NAME}");
    let mut devices: HashMap<String, LiveDevice> = HashMap::new();

    loop {
        let pipe = match NamedPipeServer::create_and_wait(PIPE_NAME) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[hidmaestro-helper] pipe error: {e}; exiting");
                return;
            }
        };
        // One client connection → handle requests until it disconnects or asks
        // us to shut down.
        if handle_client(pipe, &mut devices) {
            eprintln!("[hidmaestro-helper] shutdown requested");
            return;
        }
    }
}

/// Returns true if the client requested shutdown.
fn handle_client(pipe: NamedPipeServer, devices: &mut HashMap<String, LiveDevice>) -> bool {
    let reader_pipe = match pipe.try_clone() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let mut writer = pipe;
    let mut reader = BufReader::new(reader_pipe);

    loop {
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

        let (resp, shutdown) = handle_request(req, devices);
        let _ = write_response(&mut writer, &resp);
        if shutdown {
            return true;
        }
    }
}

fn handle_request(
    req: Request,
    devices: &mut HashMap<String, LiveDevice>,
) -> (Response, bool) {
    match req {
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
        Request::Create { profile_json, index } => {
            (handle_create(&profile_json, index, devices), false)
        }
        Request::Destroy { instance_id } => (handle_destroy(&instance_id, devices), false),
        Request::Shutdown => (Response::ok(), true),
    }
}

fn handle_create(
    profile_json: &str,
    index: u32,
    devices: &mut HashMap<String, LiveDevice>,
) -> Response {
    let profile = match Profile::from_json(profile_json) {
        Ok(p) => p,
        Err(e) => return Response::err(format!("bad profile: {e}")),
    };
    // Ensure the driver is present (idempotent).
    if let Err(e) = ensure_driver_installed() {
        return Response::err(format!("driver not available: {e}"));
    }
    let inf = match installed_inf_path() {
        Some(p) => p,
        None => return Response::err("HIDMaestro INF not found after install"),
    };

    // Pre-create the Global\ sections so the driver can open them on bind.
    let input = match InputSection::create(index) {
        Ok(s) => s,
        Err(e) => return Response::err(format!("create input section: {e}")),
    };
    let output = OutputSection::create(index).ok();

    match create_device_node(&profile, &inf.display().to_string(), index) {
        Ok(dev) => {
            devices.insert(
                dev.instance_id.clone(),
                LiveDevice { _input: input, _output: output, index },
            );
            Response::Created { instance_id: dev.instance_id, index }
        }
        Err(e) => Response::err(format!("create device node: {e}")),
    }
}

fn handle_destroy(instance_id: &str, devices: &mut HashMap<String, LiveDevice>) -> Response {
    let removed = match remove_device_node(instance_id) {
        Ok(g) => g,
        Err(e) => return Response::err(format!("remove device: {e}")),
    };
    // Drop the held sections (unmaps the shared memory) after the devnode is gone.
    if let Some(dev) = devices.remove(instance_id) {
        let _ = dev.index;
    }
    Response::Ok {
        detail: Some(format!("removed={removed}")),
    }
}

fn write_response(pipe: &mut NamedPipeServer, resp: &Response) -> std::io::Result<()> {
    let line = flexinput_hidmaestro::helper_ipc::encode_line(resp);
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
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl NamedPipeServer {
    /// Create a pipe instance and block until a client connects.
    fn create_and_wait(name: &str) -> std::io::Result<Self> {
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
        // ERROR_PIPE_CONNECTED (535) means a client connected between create
        // and ConnectNamedPipe — also success.
        if ok == 0 {
            let e = unsafe { GetLastError() };
            if e != 535 {
                return Err(std::io::Error::from_raw_os_error(e as i32));
            }
        }
        Ok(server)
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
