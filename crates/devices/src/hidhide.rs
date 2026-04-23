// HidHide IOCTL client + Windows device instance path lookup.
// HidHideClient is defined on all platforms (ZST on non-Windows) so callers
// can hold Option<HidHideClient> without cfg noise at every use-site.

// ── IOCTL codes (CTL_CODE results for HidHide driver) ────────────────────────
// CTL_CODE(FILE_DEVICE_UNKNOWN=0x22, fn, METHOD_BUFFERED=0, access)
// = (0x22<<16)|(access<<14)|(fn<<2)|0
const IOCTL_GET_WHITELIST: u32 = 0x0022_6000; // access=FILE_READ_DATA(1),  fn=0x800
const IOCTL_SET_WHITELIST: u32 = 0x0022_6004; // fn=0x801
const IOCTL_GET_BLACKLIST: u32 = 0x0022_6008; // fn=0x802
const IOCTL_SET_BLACKLIST: u32 = 0x0022_600C; // fn=0x803
const IOCTL_GET_ACTIVE:    u32 = 0x0022_6010; // fn=0x804
const IOCTL_SET_ACTIVE:    u32 = 0x0022_A014; // access=FILE_WRITE_DATA(2), fn=0x805

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
    pub fn try_open() -> Option<Self> {
        #[cfg(windows)] {
            use win32::*;
            let path = to_wide(r"\\.\HidHide");
            // Try read+write first (needed for set_active/set_blacklist/set_whitelist).
            // Fall back to read-only so the eye icon and blacklist queries still work
            // when the process lacks write access (no admin, restricted group).
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
                    return Some(HidHideClient { handle });
                }
            }
            None
        }
        #[cfg(not(windows))] { None }
    }

    #[cfg(windows)]
    fn ioctl_get(&self, code: u32) -> Vec<u16> {
        let mut buf = vec![0u16; 32_768]; // 64 KB — plenty for any realistic list
        let mut returned = 0u32;
        let ok = unsafe {
            win32::DeviceIoControl(
                self.handle, code,
                std::ptr::null(), 0,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                (buf.len() * 2) as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok != 0 { buf.truncate(returned as usize / 2); } else { buf.clear(); }
        buf
    }

    #[cfg(windows)]
    fn ioctl_set(&self, code: u32, data: &[u16]) {
        let mut returned = 0u32;
        unsafe {
            win32::DeviceIoControl(
                self.handle, code,
                data.as_ptr() as *const std::ffi::c_void,
                (data.len() * 2) as u32,
                std::ptr::null_mut(), 0,
                &mut returned,
                std::ptr::null_mut(),
            );
        }
    }

    pub fn blacklist(&self) -> Vec<String> {
        #[cfg(windows)] { from_wide_multi(&self.ioctl_get(IOCTL_GET_BLACKLIST)) }
        #[cfg(not(windows))] { vec![] }
    }

    pub fn set_blacklist(&self, list: &[String]) {
        #[cfg(windows)] { self.ioctl_set(IOCTL_SET_BLACKLIST, &to_wide_multi(list)); }
        let _ = list;
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
            unsafe {
                win32::DeviceIoControl(
                    self.handle, IOCTL_SET_ACTIVE,
                    &val as *const u8 as *const std::ffi::c_void, 1,
                    std::ptr::null_mut(), 0,
                    &mut returned, std::ptr::null_mut(),
                );
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
    pub fn set_hidden(&self, instance_id: &str, hidden: bool) {
        let mut list = self.blacklist();
        let upper = instance_id.to_uppercase();
        let present = list.iter().any(|s| s.to_uppercase() == upper);
        if hidden && !present {
            list.push(instance_id.to_uppercase());
            self.set_blacklist(&list);
        } else if !hidden && present {
            list.retain(|s| s.to_uppercase() != upper);
            self.set_blacklist(&list);
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
                    win32::GetCurrentProcess(), 0,
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
/// for the first NON-ViGEmBus device matching the given VID and PID.
/// Returns `None` when only virtual (IG_) instances exist.
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
        let mut result = None;
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
                if !s.to_uppercase().contains("IG_") {
                    // Prefer the first physical (non-ViGEmBus) device.
                    result = Some(s);
                    break;
                }
                // IG_ device: skip (it's a ViGEmBus virtual controller).
            }
        }

        unsafe { SetupDiDestroyDeviceInfoList(hdevinfo); }
        result
    }
    #[cfg(not(windows))] { let _ = (vid, pid); None }
}
