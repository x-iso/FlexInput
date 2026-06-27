//! Minimal **elevated** HidHide control-device client for the helper.
//!
//! HidHide (nefarius/HidHide) is a HID-class filter driver that hides physical
//! HID controllers from non-whitelisted processes system-wide. Its config IOCTLs
//! write driver state that lives under HKLM, so **modifying** the blacklist /
//! whitelist / active flag requires admin — which the unelevated app can't do.
//! That is exactly why the in-app `flexinput_devices::HidHideClient` could only
//! read; the elevated helper owns the writes.
//!
//! This module is intentionally a tiny, self-contained copy of just the IOCTL
//! surface the helper needs (open + get/set blacklist/whitelist/active) so the
//! lean helper crate doesn't pull in the heavy device-input dependencies. The
//! IOCTL codes are externally defined by the HidHide driver and never change.
//!
//! HidHide hides HID-class devices only — it cannot hide the XInput/XUSB face of
//! Xbox controllers (those don't traverse the HID filter). So this is the masking
//! path for DS4 / DualSense / Switch-style pads, not for physical Xbox pads.

#![cfg(windows)]

use std::ffi::c_void;

// ── IOCTL codes (must match flexinput_devices::hidhide) ──────────────────────
// CTL_CODE(0x8001, fn, METHOD_BUFFERED=0, FILE_READ_DATA=1)
const IOCTL_GET_WHITELIST: u32 = 0x8001_6000; // fn=0x800
const IOCTL_SET_WHITELIST: u32 = 0x8001_6004; // fn=0x801
const IOCTL_GET_BLACKLIST: u32 = 0x8001_6008; // fn=0x802
const IOCTL_SET_BLACKLIST: u32 = 0x8001_600C; // fn=0x803
const IOCTL_GET_ACTIVE:    u32 = 0x8001_6010; // fn=0x804
const IOCTL_SET_ACTIVE:    u32 = 0x8001_6014; // fn=0x805

const GENERIC_READ:  u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ:  u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        name: *const u16, access: u32, share: u32, sec: *mut c_void,
        disposition: u32, flags: u32, template: *mut c_void,
    ) -> *mut c_void;
    fn CloseHandle(h: *mut c_void) -> i32;
    fn DeviceIoControl(
        h: *mut c_void, code: u32, in_buf: *const c_void, in_len: u32,
        out_buf: *mut c_void, out_len: u32, returned: *mut u32, ovl: *mut c_void,
    ) -> i32;
    fn GetLastError() -> u32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Pack a list of strings into a double-null-terminated UTF-16 multi-string,
/// the form HidHide's SET IOCTLs expect.
fn to_wide_multi(items: &[String]) -> Vec<u16> {
    let mut buf: Vec<u16> = items
        .iter()
        .flat_map(|s| s.encode_utf16().chain(std::iter::once(0u16)))
        .collect();
    buf.push(0); // double-null terminator
    buf
}

/// Unpack a double-null-terminated UTF-16 multi-string into individual entries.
fn from_wide_multi(buf: &[u16]) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    for (i, &c) in buf.iter().enumerate() {
        if c == 0 {
            if start == i {
                break;
            }
            result.push(String::from_utf16_lossy(&buf[start..i]));
            start = i + 1;
        }
    }
    result
}

/// An open handle to the HidHide control device (`\\.\HidHide`). Opened with
/// read+write so the SET IOCTLs pass the driver's internal access check (we run
/// elevated, so this succeeds when HidHide is installed).
pub struct HidHide {
    handle: *mut c_void,
}

impl Drop for HidHide {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

impl HidHide {
    /// Open the control device. `None` when HidHide isn't installed (or the
    /// device can't be opened even elevated). Tries read-write first, then falls
    /// back to read-only: HidHide's IOCTLs declare only FILE_READ_DATA and the
    /// driver does its own privilege check, so SET works on a read-only handle when
    /// we're elevated — and some installs won't grant RW. (Mirrors the app-side
    /// `HidHideClient::try_open`.)
    pub fn open() -> Option<Self> {
        let path = wide(r"\\.\HidHide");
        let mut last_err = 0u32;
        for access in [GENERIC_READ | GENERIC_WRITE, GENERIC_READ] {
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    access,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE && !handle.is_null() {
                return Some(HidHide { handle });
            }
            last_err = unsafe { GetLastError() };
        }
        // err 32 = sharing violation (another process holds the device), 5 = access
        // denied, 2 = not installed.
        eprintln!("[helper:hidhide] open \\\\.\\HidHide failed: GetLastError={last_err}");
        None
    }

    fn ioctl_get(&self, code: u32) -> Vec<u16> {
        // Phase 1: zero out-buffer => size-query (bytes needed via `returned`).
        let mut needed = 0u32;
        let ok = unsafe {
            DeviceIoControl(self.handle, code, std::ptr::null(), 0,
                std::ptr::null_mut(), 0, &mut needed, std::ptr::null_mut())
        };
        if ok == 0 || needed == 0 {
            return vec![];
        }
        // Phase 2: fetch exactly that many bytes.
        let n_u16 = ((needed as usize) + 1) / 2;
        let mut buf = vec![0u16; n_u16];
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(self.handle, code, std::ptr::null(), 0,
                buf.as_mut_ptr() as *mut c_void, needed, &mut returned, std::ptr::null_mut())
        };
        if ok == 0 {
            return vec![];
        }
        buf.truncate(returned as usize / 2);
        buf
    }

    fn ioctl_set_multi(&self, code: u32, list: &[String]) -> bool {
        let data = to_wide_multi(list);
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(self.handle, code, data.as_ptr() as *const c_void,
                (data.len() * 2) as u32, std::ptr::null_mut(), 0, &mut returned, std::ptr::null_mut())
        };
        if ok == 0 {
            eprintln!("[helper:hidhide] ioctl_set(code={code:#X}) failed: GetLastError={}",
                unsafe { GetLastError() });
        }
        ok != 0
    }

    pub fn blacklist(&self) -> Vec<String> {
        from_wide_multi(&self.ioctl_get(IOCTL_GET_BLACKLIST))
    }

    pub fn set_blacklist(&self, list: &[String]) -> bool {
        self.ioctl_set_multi(IOCTL_SET_BLACKLIST, list)
    }

    pub fn whitelist(&self) -> Vec<String> {
        from_wide_multi(&self.ioctl_get(IOCTL_GET_WHITELIST))
    }

    pub fn set_whitelist(&self, list: &[String]) -> bool {
        self.ioctl_set_multi(IOCTL_SET_WHITELIST, list)
    }

    pub fn is_active(&self) -> bool {
        let mut val = 0u8;
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(self.handle, IOCTL_GET_ACTIVE, std::ptr::null(), 0,
                &mut val as *mut u8 as *mut c_void, 1, &mut returned, std::ptr::null_mut())
        };
        ok != 0 && val != 0
    }

    pub fn set_active(&self, active: bool) -> bool {
        let val = active as u8;
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(self.handle, IOCTL_SET_ACTIVE, &val as *const u8 as *const c_void, 1,
                std::ptr::null_mut(), 0, &mut returned, std::ptr::null_mut())
        };
        if ok == 0 {
            eprintln!("[helper:hidhide] set_active({active}) failed: GetLastError={}",
                unsafe { GetLastError() });
        }
        ok != 0
    }

    /// Ensure every path in `paths` is present in the whitelist (case-insensitive),
    /// preserving any existing entries. Returns the resulting whitelist.
    pub fn ensure_whitelisted(&self, paths: &[String]) -> Vec<String> {
        let mut list = self.whitelist();
        for p in paths {
            let up = p.to_uppercase();
            if !list.iter().any(|s| s.to_uppercase() == up) {
                list.push(p.clone());
            }
        }
        self.set_whitelist(&list);
        list
    }
}

// Raw handle owned exclusively by this struct; safe to move across threads.
unsafe impl Send for HidHide {}
