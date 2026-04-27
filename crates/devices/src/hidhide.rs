// HidHide IOCTL client + Windows device instance path lookup.
// HidHideClient is defined on all platforms (ZST on non-Windows) so callers
// can hold Option<HidHideClient> without cfg noise at every use-site.

// ── IOCTL codes (CTL_CODE results for HidHide driver) ────────────────────────
// HidHide uses a custom device type (0x8001), NOT FILE_DEVICE_UNKNOWN (0x22).
// All IOCTLs (both GET and SET) declare FILE_READ_DATA access — the driver
// performs its own privilege checks internally rather than relying on the
// I/O manager's handle-access validation.
// CTL_CODE(0x8001, fn, METHOD_BUFFERED=0, FILE_READ_DATA=1)
// = (0x8001<<16) | (1<<14) | (fn<<2) | 0
const IOCTL_GET_WHITELIST: u32 = 0x8001_6000; // fn=0x800
const IOCTL_SET_WHITELIST: u32 = 0x8001_6004; // fn=0x801
const IOCTL_GET_BLACKLIST: u32 = 0x8001_6008; // fn=0x802
const IOCTL_SET_BLACKLIST: u32 = 0x8001_600C; // fn=0x803
const IOCTL_GET_ACTIVE:    u32 = 0x8001_6010; // fn=0x804
const IOCTL_SET_ACTIVE:    u32 = 0x8001_6014; // fn=0x805

// ── Windows-only Win32 declarations ──────────────────────────────────────────
#[cfg(windows)]
mod win32 {
    use std::ffi::c_void;

    pub const GENERIC_READ:          u32 = 0x8000_0000;
    pub const GENERIC_WRITE:         u32 = 0x4000_0000;
    pub const FILE_SHARE_READ:       u32 = 0x0000_0001;
    pub const FILE_SHARE_WRITE:      u32 = 0x0000_0002;
    pub const OPEN_EXISTING:          u32 = 3;
    pub const FILE_ATTRIBUTE_NORMAL:  u32 = 0x80;
    pub const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;
    pub const DIGCF_PRESENT:         u32 = 0x0000_0002;
    pub const DIGCF_ALLCLASSES:      u32 = 0x0000_0004;
    pub const SPDRP_HARDWAREID:      u32 = 1;

    #[repr(C)]
    pub struct GUID { pub data1: u32, pub data2: u16, pub data3: u16, pub data4: [u8; 8] }

    #[repr(C)]
    pub struct SP_DEVINFO_DATA {
        pub cb_size:    u32,
        pub class_guid: GUID,
        pub dev_inst:   u32,
        pub reserved:   usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateFileW(
            lp_file_name:             *const u16,
            dw_desired_access:        u32,
            dw_share_mode:            u32,
            lp_security_attributes:   *mut c_void,
            dw_creation_disposition:  u32,
            dw_flags_and_attributes:  u32,
            h_template_file:          *mut c_void,
        ) -> *mut c_void;

        pub fn CloseHandle(h_object: *mut c_void) -> i32;

        pub fn DeviceIoControl(
            h_device:            *mut c_void,
            dw_io_control_code:  u32,
            lp_in_buffer:        *const c_void,
            n_in_buffer_size:    u32,
            lp_out_buffer:       *mut c_void,
            n_out_buffer_size:   u32,
            lp_bytes_returned:   *mut u32,
            lp_overlapped:       *mut c_void,
        ) -> i32;

        pub fn GetCurrentProcess() -> *mut c_void;

        pub fn GetLastError() -> u32;

        pub fn QueryFullProcessImageNameW(
            h_process:  *mut c_void,
            dw_flags:   u32,
            lp_exe_name: *mut u16,
            lp_size:    *mut u32,
        ) -> i32;
    }

    #[link(name = "setupapi")]
    extern "system" {
        pub fn SetupDiGetClassDevsW(
            class_guid:   *const GUID,
            enumerator:   *const u16,
            hwnd_parent:  *mut c_void,
            flags:        u32,
        ) -> *mut c_void;

        pub fn SetupDiEnumDeviceInfo(
            device_info_set:  *mut c_void,
            member_index:     u32,
            device_info_data: *mut SP_DEVINFO_DATA,
        ) -> i32;

        pub fn SetupDiGetDeviceInstanceIdW(
            device_info_set:       *mut c_void,
            device_info_data:      *mut SP_DEVINFO_DATA,
            device_instance_id:    *mut u16,
            device_instance_id_size: u32,
            required_size:         *mut u32,
        ) -> i32;

        pub fn SetupDiGetDeviceRegistryPropertyW(
            device_info_set:     *mut c_void,
            device_info_data:    *mut SP_DEVINFO_DATA,
            property:            u32,
            property_reg_data_type: *mut u32,
            property_buffer:     *mut u8,
            property_buffer_size: u32,
            required_size:       *mut u32,
        ) -> i32;

        pub fn SetupDiDestroyDeviceInfoList(device_info_set: *mut c_void) -> i32;
    }
}

// ── String helpers (Windows-only) ────────────────────────────────────────────
#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn from_wide_multi(buf: &[u16]) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    for (i, &c) in buf.iter().enumerate() {
        if c == 0 {
            if start == i { break; }
            result.push(String::from_utf16_lossy(&buf[start..i]));
            start = i + 1;
        }
    }
    result
}

#[cfg(windows)]
fn to_wide_multi(items: &[String]) -> Vec<u16> {
    let mut buf: Vec<u16> = items.iter()
        .flat_map(|s| s.encode_utf16().chain(std::iter::once(0u16)))
        .collect();
    buf.push(0); // double-null terminator
    buf
}

// ── HidHideClient ─────────────────────────────────────────────────────────────

pub struct HidHideClient {
    #[cfg(windows)]
    handle: *mut std::ffi::c_void,
}

// Raw pointer field is not automatically Send/Sync; declare it safe because
// the handle is owned exclusively by this struct.
unsafe impl Send for HidHideClient {}
unsafe impl Sync for HidHideClient {}

#[cfg(windows)]
impl Drop for HidHideClient {
    fn drop(&mut self) {
        unsafe { win32::CloseHandle(self.handle); }
    }
}

impl HidHideClient {
    /// Returns `None` if HidHide is not installed or the control device cannot be opened.
    /// Tries to open with read+write access first (needed by the driver's
    /// internal access check on SET operations); falls back to read-only.
    pub fn try_open() -> Option<Self> {
        #[cfg(windows)] {
            use win32::*;
            let path = to_wide(r"\\.\HidHide");
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
                    let access_str = if access == (GENERIC_READ | GENERIC_WRITE) {
                        "GENERIC_READ | GENERIC_WRITE"
                    } else {
                        "GENERIC_READ only"
                    };
                    eprintln!("[hidhide] try_open: succeeded with {}", access_str);
                    return Some(HidHideClient { handle });
                }
                let err = unsafe { GetLastError() };
                eprintln!("[hidhide] try_open: access={:#010X} failed GetLastError={}", access, err);
            }
            None
        }
        #[cfg(not(windows))] { None }
    }

    #[cfg(windows)]
    fn ioctl_get(&self, code: u32) -> Vec<u16> {
        // Phase 1: outputBufferLength=0 triggers the driver's size-query path,
        // returning the number of bytes needed via lpBytesReturned.
        let mut needed = 0u32;
        let ok = unsafe {
            win32::DeviceIoControl(
                self.handle, code,
                std::ptr::null(), 0,
                std::ptr::null_mut(), 0,
                &mut needed,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = unsafe { win32::GetLastError() };
            eprintln!("[hidhide] ioctl_get size-query(code={:#010X}) FAILED: GetLastError={}", code, err);
            return vec![];
        }
        if needed == 0 {
            return vec![]; // empty list
        }
        // Phase 2: fetch data using the exact byte count the driver reported.
        let n_u16 = ((needed as usize) + 1) / 2; // bytes → u16, round up
        let mut buf = vec![0u16; n_u16];
        let mut returned = 0u32;
        let ok = unsafe {
            win32::DeviceIoControl(
                self.handle, code,
                std::ptr::null(), 0,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                needed,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok != 0 {
            buf.truncate(returned as usize / 2);
        } else {
            let err = unsafe { win32::GetLastError() };
            eprintln!("[hidhide] ioctl_get fetch(code={:#010X}) FAILED: GetLastError={}", code, err);
            buf.clear();
        }
        buf
    }

    #[cfg(windows)]
    fn ioctl_set(&self, code: u32, data: &[u16]) -> bool {
        let mut returned = 0u32;
        let ok = unsafe {
            win32::DeviceIoControl(
                self.handle, code,
                data.as_ptr() as *const std::ffi::c_void,
                (data.len() * 2) as u32,
                std::ptr::null_mut(), 0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = unsafe { win32::GetLastError() };
            eprintln!("[hidhide] ioctl_set(code={:#X}, bytes={}) failed: GetLastError={}",
                code, data.len() * 2, err);
        }
        ok != 0
    }

    pub fn blacklist(&self) -> Vec<String> {
        #[cfg(windows)] { from_wide_multi(&self.ioctl_get(IOCTL_GET_BLACKLIST)) }
        #[cfg(not(windows))] { vec![] }
    }

    pub fn set_blacklist(&self, list: &[String]) -> bool {
        #[cfg(windows)] { return self.ioctl_set(IOCTL_SET_BLACKLIST, &to_wide_multi(list)); }
        #[cfg(not(windows))] { let _ = list; true }
    }

    pub fn whitelist(&self) -> Vec<String> {
        #[cfg(windows)] { from_wide_multi(&self.ioctl_get(IOCTL_GET_WHITELIST)) }
        #[cfg(not(windows))] { vec![] }
    }

    pub fn set_whitelist(&self, list: &[String]) {
        #[cfg(windows)] { self.ioctl_set(IOCTL_SET_WHITELIST, &to_wide_multi(list)); }
        let _ = list;
    }

    pub fn is_active(&self) -> bool {
        #[cfg(windows)] {
            let mut val = 0u8;
            let mut returned = 0u32;
            let ok = unsafe {
                win32::DeviceIoControl(
                    self.handle, IOCTL_GET_ACTIVE,
                    std::ptr::null(), 0,
                    &mut val as *mut u8 as *mut std::ffi::c_void, 1,
                    &mut returned, std::ptr::null_mut(),
                )
            };
            ok != 0 && val != 0
        }
        #[cfg(not(windows))] { false }
    }

    pub fn set_active(&self, active: bool) {
        #[cfg(windows)] {
            let val = active as u8;
            let mut returned = 0u32;
            let ok = unsafe {
                win32::DeviceIoControl(
                    self.handle, IOCTL_SET_ACTIVE,
                    &val as *const u8 as *const std::ffi::c_void, 1,
                    std::ptr::null_mut(), 0,
                    &mut returned, std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let err = unsafe { win32::GetLastError() };
                eprintln!("[hidhide] set_active({}) failed: GetLastError={}", active, err);
            }
        }
        let _ = active;
    }

    /// Returns true if `instance_id` is currently in the HidHide blacklist.
    pub fn is_hidden(&self, instance_id: &str) -> bool {
        let upper = instance_id.to_uppercase();
        self.blacklist().iter().any(|s| s.to_uppercase() == upper)
    }

    /// Adds or removes `instance_id` from the blacklist.
    /// Returns a human-readable status string describing the outcome.
    pub fn set_hidden(&self, instance_id: &str, hidden: bool) -> String {
        let before = self.blacklist();
        let mut list = before.clone();
        let upper = instance_id.to_uppercase();
        let present = list.iter().any(|s| s.to_uppercase() == upper);
        let did_change = if hidden && !present {
            list.push(instance_id.to_string());
            true
        } else if !hidden && present {
            list.retain(|s| s.to_uppercase() != upper);
            true
        } else {
            return format!("no change (already {})", if hidden { "hidden" } else { "visible" });
        };
        let _ = did_change;
        eprintln!("[hidhide] set_hidden: writing id={:?} hidden={}", instance_id, hidden);
        let ok = self.set_blacklist(&list);
        let after = self.blacklist();
        eprintln!(
            "[hidhide] set_hidden — before={} wrote={} after={} ioctl_ok={}",
            before.len(), list.len(), after.len(), ok
        );
        eprintln!("[hidhide] after contents: {:?}", after);
        let now_present = after.iter().any(|s| s.to_uppercase() == upper);
        if !ok {
            return "IOCTL failed (see stderr)".to_string();
        }
        // Always include before/after counts and any obviously-different stored
        // path so we can see what the driver actually persisted.
        let before_n = before.len();
        let after_n = after.len();
        let summary = format!("before={before_n} wrote={} after={after_n}", list.len());
        if hidden && !now_present {
            // Did the driver perhaps store a normalized variant?
            let near = after.iter().find(|s| {
                let u = s.to_uppercase();
                u.contains(&upper[..upper.len().min(20)]) || upper.contains(&u[..u.len().min(20)])
            });
            match near {
                Some(p) => format!("DRIVER NORMALIZED PATH ({summary}) — stored as: {p}"),
                None => format!("WRITE DROPPED ({summary}) — registry write was silently rejected by driver"),
            }
        } else if !hidden && now_present {
            format!("REMOVE DROPPED ({summary}) — path still present after write")
        } else {
            format!("OK ({summary})")
        }
    }

    /// Adds `exe_path` to the whitelist if not already present.
    pub fn ensure_whitelisted(&self, exe_path: &str) {
        let mut list = self.whitelist();
        let upper = exe_path.to_uppercase();
        if !list.iter().any(|s| s.to_uppercase() == upper) {
            list.push(exe_path.to_string());
            self.set_whitelist(&list);
        }
    }

    /// Returns the full path of the current executable.
    pub fn current_exe_path() -> Option<String> {
        #[cfg(windows)] {
            let mut buf = vec![0u16; 4096];
            let mut size = buf.len() as u32;
            let ok = unsafe {
                win32::QueryFullProcessImageNameW(
                    win32::GetCurrentProcess(), 1,
                    buf.as_mut_ptr(), &mut size,
                )
            };
            if ok != 0 { Some(String::from_utf16_lossy(&buf[..size as usize])) } else { None }
        }
        #[cfg(not(windows))] {
            std::env::current_exe().ok().and_then(|p| p.to_str().map(str::to_owned))
        }
    }
}

// ── Device instance path lookup via SetupAPI ─────────────────────────────────

/// Returns true if any Windows device with this VID/PID is a ViGEmBus
/// virtual controller (instance ID contains "IG_").  Used to filter virtual
/// gamepads out of the physical-device panel.
pub fn has_vigem_for_vid_pid(vid: u16, pid: u16) -> bool {
    #[cfg(windows)] {
        use win32::*;
        use std::mem;
        let hdevinfo = unsafe {
            SetupDiGetClassDevsW(std::ptr::null(), std::ptr::null(), std::ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_ALLCLASSES)
        };
        if hdevinfo == INVALID_HANDLE_VALUE || hdevinfo.is_null() { return false; }
        let needle = format!("VID_{:04X}&PID_{:04X}", vid, pid);
        let ig_marker = "IG_";
        let mut found = false;
        let mut idx = 0u32;
        loop {
            let mut info = SP_DEVINFO_DATA {
                cb_size: mem::size_of::<SP_DEVINFO_DATA>() as u32,
                class_guid: GUID { data1: 0, data2: 0, data3: 0, data4: [0; 8] },
                dev_inst: 0, reserved: 0,
            };
            if unsafe { SetupDiEnumDeviceInfo(hdevinfo, idx, &mut info) } == 0 { break; }
            idx += 1;
            let mut hw_buf  = vec![0u8; 1024];
            let mut hw_type = 0u32;
            let mut hw_size = 0u32;
            let ok = unsafe {
                SetupDiGetDeviceRegistryPropertyW(
                    hdevinfo, &mut info, SPDRP_HARDWAREID, &mut hw_type,
                    hw_buf.as_mut_ptr(), hw_buf.len() as u32, &mut hw_size)
            };
            if ok == 0 { continue; }
            let hw_words: Vec<u16> = hw_buf[..hw_size as usize]
                .chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
            let hw_ids = from_wide_multi(&hw_words);
            if !hw_ids.iter().any(|id| id.to_uppercase().contains(&needle)) { continue; }
            let mut id_buf  = vec![0u16; 512];
            let mut id_size = 0u32;
            if unsafe { SetupDiGetDeviceInstanceIdW(
                hdevinfo, &mut info, id_buf.as_mut_ptr(), id_buf.len() as u32, &mut id_size) } != 0
            {
                let len = (id_size as usize).saturating_sub(1);
                let s = String::from_utf16_lossy(&id_buf[..len]);
                if s.to_uppercase().contains(ig_marker) { found = true; break; }
            }
        }
        unsafe { SetupDiDestroyDeviceInfoList(hdevinfo); }
        found
    }
    #[cfg(not(windows))] { let _ = (vid, pid); false }
}

/// Counts how many physical (non-ViGEmBus) devices with this VID/PID exist.
/// Used to filter virtual controller duplicates out of the physical-device list
/// even when a real device with the same VID/PID is also connected.
pub fn physical_count_for_vid_pid(vid: u16, pid: u16) -> usize {
    #[cfg(windows)] {
        use win32::*;
        use std::mem;
        let hdevinfo = unsafe {
            SetupDiGetClassDevsW(std::ptr::null(), std::ptr::null(), std::ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_ALLCLASSES)
        };
        if hdevinfo == INVALID_HANDLE_VALUE || hdevinfo.is_null() { return 0; }
        // USB HID:       HID\VID_057E&PID_2009\...
        // Bluetooth HID: BTHENUM\{GUID}_VID&02057E_PID&2009_REV&...
        let needle_usb = format!("VID_{:04X}&PID_{:04X}", vid, pid);
        let needle_bt  = format!("VID&02{:04X}_PID&{:04X}", vid, pid);
        let mut count = 0usize;
        let mut idx = 0u32;
        loop {
            let mut info = SP_DEVINFO_DATA {
                cb_size: mem::size_of::<SP_DEVINFO_DATA>() as u32,
                class_guid: GUID { data1: 0, data2: 0, data3: 0, data4: [0; 8] },
                dev_inst: 0, reserved: 0,
            };
            if unsafe { SetupDiEnumDeviceInfo(hdevinfo, idx, &mut info) } == 0 { break; }
            idx += 1;
            let mut hw_buf = vec![0u8; 1024];
            let mut hw_type = 0u32;
            let mut hw_size = 0u32;
            let ok = unsafe {
                SetupDiGetDeviceRegistryPropertyW(
                    hdevinfo, &mut info, SPDRP_HARDWAREID, &mut hw_type,
                    hw_buf.as_mut_ptr(), hw_buf.len() as u32, &mut hw_size)
            };
            if ok == 0 { continue; }
            let hw_words: Vec<u16> = hw_buf[..hw_size as usize]
                .chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
            if !from_wide_multi(&hw_words).iter().any(|id| {
                let up = id.to_uppercase();
                up.contains(&needle_usb) || up.contains(&needle_bt)
            }) { continue; }
            let mut id_buf = vec![0u16; 512];
            let mut id_size = 0u32;
            if unsafe { SetupDiGetDeviceInstanceIdW(
                hdevinfo, &mut info, id_buf.as_mut_ptr(), id_buf.len() as u32, &mut id_size) } != 0
            {
                let len = (id_size as usize).saturating_sub(1);
                if !String::from_utf16_lossy(&id_buf[..len]).to_uppercase().contains("IG_") {
                    count += 1;
                }
            }
        }
        unsafe { SetupDiDestroyDeviceInfoList(hdevinfo); }
        count
    }
    #[cfg(not(windows))] { let _ = (vid, pid); 0 }
}

/// Finds the Windows device instance ID (e.g. `HID\VID_054C&PID_09CC\5&...`)
/// for the first NON-ViGEmBus HID device matching the given VID and PID.
/// HidHide operates on HID-class devices, so paths starting with `HID\` are
/// preferred over USB composite parents (`USB\…`) which HidHide cannot hide.
/// Falls back to a non-HID match only when no HID instance exists.
pub fn instance_id_for_vid_pid(vid: u16, pid: u16) -> Option<String> {
    #[cfg(windows)] {
        use win32::*;
        use std::mem;

        let hdevinfo = unsafe {
            SetupDiGetClassDevsW(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_ALLCLASSES,
            )
        };
        if hdevinfo == INVALID_HANDLE_VALUE || hdevinfo.is_null() {
            return None;
        }

        // USB HID:       HID\VID_057E&PID_2009\...
        // Bluetooth HID: BTHENUM\{GUID}_VID&02057E_PID&2009_REV&...
        let needle_usb = format!("VID_{:04X}&PID_{:04X}", vid, pid);
        let needle_bt  = format!("VID&02{:04X}_PID&{:04X}", vid, pid);
        let mut hid_result: Option<String> = None;
        let mut other_result: Option<String> = None;
        let mut idx = 0u32;

        loop {
            let mut info = SP_DEVINFO_DATA {
                cb_size:    mem::size_of::<SP_DEVINFO_DATA>() as u32,
                class_guid: GUID { data1: 0, data2: 0, data3: 0, data4: [0; 8] },
                dev_inst:   0,
                reserved:   0,
            };
            if unsafe { SetupDiEnumDeviceInfo(hdevinfo, idx, &mut info) } == 0 { break; }
            idx += 1;

            // Fetch hardware ID multi-string (UTF-16 bytes)
            let mut hw_buf  = vec![0u8; 1024];
            let mut hw_type = 0u32;
            let mut hw_size = 0u32;
            let ok = unsafe {
                SetupDiGetDeviceRegistryPropertyW(
                    hdevinfo, &mut info, SPDRP_HARDWAREID, &mut hw_type,
                    hw_buf.as_mut_ptr(), hw_buf.len() as u32, &mut hw_size,
                )
            };
            if ok == 0 { continue; }

            let hw_words: Vec<u16> = hw_buf[..hw_size as usize]
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            let hw_ids = from_wide_multi(&hw_words);

            if !hw_ids.iter().any(|id| {
                let up = id.to_uppercase();
                up.contains(&needle_usb) || up.contains(&needle_bt)
            }) { continue; }

            // Fetch device instance ID
            let mut id_buf  = vec![0u16; 512];
            let mut id_size = 0u32;
            if unsafe {
                SetupDiGetDeviceInstanceIdW(
                    hdevinfo, &mut info,
                    id_buf.as_mut_ptr(), id_buf.len() as u32, &mut id_size,
                )
            } != 0 {
                let len = (id_size as usize).saturating_sub(1);
                let s = String::from_utf16_lossy(&id_buf[..len]);
                let upper = s.to_uppercase();
                if upper.contains("IG_") { continue; } // ViGEmBus virtual

                // Prefer HID-class devices since HidHide only hides those.
                // BTHHID covers Bluetooth HID; some Windows builds expose the
                // HID device directly under HID\{class-guid}_… as well.
                let is_hid = upper.starts_with("HID\\")
                    || upper.starts_with("BTHHID\\");
                if is_hid {
                    if hid_result.is_none() { hid_result = Some(s); }
                } else if other_result.is_none() {
                    other_result = Some(s);
                }
            }
        }

        unsafe { SetupDiDestroyDeviceInfoList(hdevinfo); }
        let chosen = hid_result.or(other_result);
        #[cfg(debug_assertions)]
        eprintln!("[hidhide] instance_id_for_vid_pid({:04X},{:04X}) = {:?}",
            vid, pid, chosen);
        chosen
    }
    #[cfg(not(windows))] { let _ = (vid, pid); None }
}
