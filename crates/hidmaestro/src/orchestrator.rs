//! Virtual device node creation + teardown (plain-HID path).
//!
//! Rust port of the plain-HID (non-XUSB, non-xinputhid) slice of HIDMaestro's
//! `DeviceNodeCreator.CreateDeviceNode` + `DeviceManager.RemoveDevice`
//! (v1.3.17). This is Phase 3a: it creates a root-enumerated HID-class device
//! for a profile whose VID != 0x045E and which declares no upper filter (e.g.
//! DualShock 4, DualSense, generic gamepads), writes its `ControllerIndex`,
//! binds the installed HIDMaestro driver, and (on teardown) removes the node.
//!
//! **Elevation:** `DIF_REGISTERDEVICE` and `UpdateDriverForPlugAndPlayDevicesW`
//! require admin (SeLoadDriverPrivilege). Per the Phase-3 decision the
//! production caller is a dedicated elevated helper process; these functions
//! contain the logic that helper runs.
//!
//! **Scope (3a):** only the plain-HID branch. The Xbox360 XUSB-companion path
//! (`VID == 0x045E`, `RequiresXusbCompanion`) and the xinputhid upper-filter
//! path are Phase 3b. `SetAllNamingProperties` (cosmetic friendly-name) is
//! reduced to the essential `ControllerIndex` write here; full naming is a
//! later refinement.

use std::ffi::c_void;

use crate::profile::Profile;

/// HID class GUID — every virtual controller is created in this class.
/// `{745a17a0-74d3-11d0-b6fe-00a0c90f57da}` (verbatim from DeviceNodeCreator).
const HID_CLASS_GUID: Guid = Guid {
    data1: 0x745a_17a0,
    data2: 0x74d3,
    data3: 0x11d0,
    data4: [0xb6, 0xfe, 0x00, 0xa0, 0xc9, 0x0f, 0x57, 0xda],
};

// SetupAPI / CfgMgr constants.
const DICD_GENERATE_ID: u32 = 0x0000_0001;
const SPDRP_HARDWAREID: u32 = 0x0000_0001;
const DIF_REGISTERDEVICE: u32 = 0x0000_0019;
const DIF_REMOVE: u32 = 0x0000_0005;
const CR_SUCCESS: u32 = 0x0000_0000;
const CM_LOCATE_DEVNODE_NORMAL: u32 = 0x0000_0000;
const CM_LOCATE_DEVNODE_PHANTOM: u32 = 0x0000_0001;
const INVALID_HANDLE_VALUE: isize = -1;

#[derive(Debug)]
pub enum OrchestratorError {
    /// A SetupAPI / CfgMgr32 call failed (step label + GetLastError).
    Win32(&'static str, u32),
    /// The newly-created node could not be located in the registry.
    NodeNotFound,
    /// 3a only supports the plain-HID path; this profile needs 3b.
    Unsupported(&'static str),
}

impl std::fmt::Display for OrchestratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrchestratorError::Win32(s, e) => write!(f, "{s} failed (err {e})"),
            OrchestratorError::NodeNotFound => write!(f, "created node not found in registry"),
            OrchestratorError::Unsupported(s) => write!(f, "unsupported profile path: {s}"),
        }
    }
}
impl std::error::Error for OrchestratorError {}

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// SP_DEVINFO_DATA — 32 bytes on x64. We treat it as an opaque pinned buffer.
#[repr(C)]
struct SpDevinfoData {
    cb_size: u32,
    class_guid: Guid,
    dev_inst: u32,
    reserved: usize,
}

#[link(name = "setupapi")]
extern "system" {
    fn SetupDiCreateDeviceInfoList(class_guid: *const Guid, hwnd_parent: *mut c_void) -> *mut c_void;
    fn SetupDiCreateDeviceInfoW(
        dev_info_set: *mut c_void,
        device_name: *const u16,
        class_guid: *const Guid,
        device_description: *const u16,
        hwnd_parent: *mut c_void,
        creation_flags: u32,
        device_info_data: *mut SpDevinfoData,
    ) -> i32;
    fn SetupDiSetDeviceRegistryPropertyW(
        dev_info_set: *mut c_void,
        device_info_data: *mut SpDevinfoData,
        property: u32,
        property_buffer: *const u8,
        property_buffer_size: u32,
    ) -> i32;
    fn SetupDiCallClassInstaller(
        install_function: u32,
        dev_info_set: *mut c_void,
        device_info_data: *mut SpDevinfoData,
    ) -> i32;
    fn SetupDiDestroyDeviceInfoList(dev_info_set: *mut c_void) -> i32;
    fn SetupDiGetClassDevsW(
        class_guid: *const Guid,
        enumerator: *const u16,
        hwnd_parent: *mut c_void,
        flags: u32,
    ) -> *mut c_void;
    fn SetupDiOpenDeviceInfoW(
        dev_info_set: *mut c_void,
        device_instance_id: *const u16,
        hwnd_parent: *mut c_void,
        open_flags: u32,
        device_info_data: *mut SpDevinfoData,
    ) -> i32;
}

#[link(name = "newdev")]
extern "system" {
    fn UpdateDriverForPlugAndPlayDevicesW(
        hwnd_parent: *mut c_void,
        hardware_id: *const u16,
        full_inf_path: *const u16,
        install_flags: u32,
        reboot_required: *mut i32,
    ) -> i32;
}

#[link(name = "cfgmgr32")]
extern "system" {
    fn CM_Locate_DevNodeW(dev_inst: *mut u32, device_id: *const u16, flags: u32) -> u32;
    fn CM_Get_Child(child: *mut u32, dev_inst: u32, flags: u32) -> u32;
    fn CM_Set_DevNode_PropertyW(
        dev_inst: u32,
        property_key: *const DevPropKey,
        property_type: u32,
        property_buffer: *const u8,
        property_buffer_size: u32,
        flags: u32,
    ) -> u32;
}

/// DEVPROPKEY — `{ fmtid: GUID, pid: u32 }`.
#[repr(C)]
struct DevPropKey {
    fmtid: Guid,
    pid: u32,
}

const DEVPROP_TYPE_GUID: u32 = 0x0000_000D;

/// `DEVPKEY_Device_BusTypeGuid` = `{a45c254e-df1c-4efd-8020-67d146a850e0}, 21`.
const DEVPKEY_DEVICE_BUS_TYPE_GUID: DevPropKey = DevPropKey {
    fmtid: Guid {
        data1: 0xa45c_254e,
        data2: 0xdf1c,
        data3: 0x4efd,
        data4: [0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0],
    },
    pid: 21,
};

/// `GUID_BUS_TYPE_USB` = `{9d7debbc-c85d-11d1-9eb4-006008c3a19a}` as raw bytes.
const GUID_BUS_TYPE_USB_BYTES: [u8; 16] = [
    0xbc, 0xeb, 0x7d, 0x9d, // data1 LE
    0x5d, 0xc8, // data2 LE
    0xd1, 0x11, // data3 LE
    0x9e, 0xb4, 0x00, 0x60, 0x08, 0xc3, 0xa1, 0x9a, // data4
];

/// Stamp `DEVPKEY_Device_BusTypeGuid = GUID_BUS_TYPE_USB` onto the devnode at
/// `instance_id` and its first child. Port of `SetBusTypeGuidUsb` (restricted to
/// our just-created node). **This is what makes the HID stack report the device
/// as a USB HID device with proper VID/PID** — without it the HID child
/// enumerates as a bare `HID\HIDCLASS` generic gamepad (no Sony identity), which
/// then gets XInput-translated by Steam/etc.
fn set_bus_type_usb(instance_id: &str) {
    unsafe {
        let w_id = to_wide(instance_id);
        let mut dev_inst: u32 = 0;
        if CM_Locate_DevNodeW(&mut dev_inst, w_id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) != CR_SUCCESS {
            return;
        }
        let stamp = |inst: u32| {
            CM_Set_DevNode_PropertyW(
                inst,
                &DEVPKEY_DEVICE_BUS_TYPE_GUID,
                DEVPROP_TYPE_GUID,
                GUID_BUS_TYPE_USB_BYTES.as_ptr(),
                GUID_BUS_TYPE_USB_BYTES.len() as u32,
                0,
            );
        };
        stamp(dev_inst);
        let mut child: u32 = 0;
        if CM_Get_Child(&mut child, dev_inst, 0) == CR_SUCCESS {
            stamp(child);
        }
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Build a REG_MULTI_SZ as UTF-16 from a list of strings (each NUL-terminated,
/// plus a trailing NUL). Returns the byte buffer SetupAPI expects.
fn multi_sz_bytes(items: &[&str]) -> Vec<u8> {
    let mut words: Vec<u16> = Vec::new();
    for it in items {
        words.extend(it.encode_utf16());
        words.push(0);
    }
    words.push(0); // terminating empty string
    let mut bytes = Vec::with_capacity(words.len() * 2);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn new_devinfo() -> SpDevinfoData {
    SpDevinfoData {
        cb_size: std::mem::size_of::<SpDevinfoData>() as u32,
        class_guid: HID_CLASS_GUID,
        dev_inst: 0,
        reserved: 0,
    }
}

/// True when the profile is on the plain-HID path this module handles.
fn is_plain_hid(profile: &Profile) -> bool {
    // Plain HID = not Microsoft (Xbox/XUSB) and (we don't model upper-filter
    // profiles yet, so anything non-0x045E here is treated as plain HID).
    profile.vid != 0x045E
}

/// Outcome of [`create_device_node`].
#[derive(Debug, Clone)]
pub struct CreatedDevice {
    /// Full PnP instance id, e.g. `ROOT\HIDClass\0001`.
    pub instance_id: String,
    pub controller_index: u32,
}

/// Create a plain-HID virtual device node for `profile` at `controller_index`,
/// binding the HIDMaestro driver at `inf_path`. Requires elevation.
///
/// Port of the plain-HID branch of `DeviceNodeCreator.CreateDeviceNode`:
/// SetupDi create-info-list → create-device (`HIDClass`, generate id) → set
/// HardwareID multi-sz (`root\VID_xxxx&PID_yyyy`, `root\HIDMaestro`) →
/// `DIF_REGISTERDEVICE` → locate our new node → write `ControllerIndex` →
/// `UpdateDriverForPlugAndPlayDevicesW`.
pub fn create_device_node(
    profile: &Profile,
    inf_path: &str,
    controller_index: u32,
) -> Result<CreatedDevice, OrchestratorError> {
    if !is_plain_hid(profile) {
        return Err(OrchestratorError::Unsupported(
            "VID 0x045E (Xbox/XUSB) needs the Phase 3b companion path",
        ));
    }

    let vid = format!("{:04X}", profile.vid);
    let pid = format!("{:04X}", profile.pid);
    let enumerator = "HIDClass";
    let hw_id = format!("root\\VID_{vid}&PID_{pid}");
    let desc = &profile.name;

    // Per-instance driver config (VID/PID/descriptor/etc.). The UMDF driver reads
    // this at startup to know what identity to report; WITHOUT it the driver
    // falls back to the Microsoft default VID_045E and the device presents as an
    // Xbox pad instead of the profile's real (Sony/etc.) identity.
    write_instance_config(profile, controller_index);

    unsafe {
        let class_guid = HID_CLASS_GUID;
        let dis = SetupDiCreateDeviceInfoList(&class_guid, std::ptr::null_mut());
        if dis as isize == INVALID_HANDLE_VALUE {
            return Err(OrchestratorError::Win32(
                "SetupDiCreateDeviceInfoList",
                GetLastError(),
            ));
        }
        // Ensure the info list is always destroyed.
        let _guard = DisGuard(dis);

        let mut devinfo = new_devinfo();
        let w_enum = to_wide(enumerator);
        let w_desc = to_wide(desc);
        if SetupDiCreateDeviceInfoW(
            dis,
            w_enum.as_ptr(),
            &class_guid,
            w_desc.as_ptr(),
            std::ptr::null_mut(),
            DICD_GENERATE_ID,
            &mut devinfo,
        ) == 0
        {
            return Err(OrchestratorError::Win32("SetupDiCreateDeviceInfoW", GetLastError()));
        }

        // HardwareID multi-sz: the device's id + the HIDMaestro ownership tag.
        let hw_bytes = multi_sz_bytes(&[&hw_id, "root\\HIDMaestro"]);
        if SetupDiSetDeviceRegistryPropertyW(
            dis,
            &mut devinfo,
            SPDRP_HARDWAREID,
            hw_bytes.as_ptr(),
            hw_bytes.len() as u32,
        ) == 0
        {
            return Err(OrchestratorError::Win32(
                "SetupDiSetDeviceRegistryPropertyW",
                GetLastError(),
            ));
        }

        // DIF_REGISTERDEVICE creates the PnP node (admin-only).
        if SetupDiCallClassInstaller(DIF_REGISTERDEVICE, dis, &mut devinfo) == 0 {
            return Err(OrchestratorError::Win32(
                "SetupDiCallClassInstaller(DIF_REGISTERDEVICE)",
                GetLastError(),
            ));
        }

        // Locate the node we just created (the first HIDMaestro-owned node
        // under ROOT\HIDClass without a ControllerIndex) and stamp it.
        let instance_id = claim_new_node(enumerator, controller_index)?;

        // Bind the HIDMaestro driver to the new hardware id.
        let w_hw = to_wide(&hw_id);
        let w_inf = to_wide(inf_path);
        let mut reboot = 0i32;
        // Non-fatal: the device exists even if the driver bind reports false;
        // the driver may already be associated by class. But surface failures.
        if UpdateDriverForPlugAndPlayDevicesW(
            std::ptr::null_mut(),
            w_hw.as_ptr(),
            w_inf.as_ptr(),
            0,
            &mut reboot,
        ) == 0
        {
            // ERROR_NO_SUCH_DEVINST / ERROR_NO_MORE_ITEMS can occur if the
            // class already bound the driver; log via the error path only when
            // the device is also missing.
            let err = GetLastError();
            if CM_Locate_DevNodeW(
                &mut 0u32,
                to_wide(&instance_id).as_ptr(),
                CM_LOCATE_DEVNODE_NORMAL,
            ) != CR_SUCCESS
            {
                return Err(OrchestratorError::Win32(
                    "UpdateDriverForPlugAndPlayDevicesW",
                    err,
                ));
            }
        }

        // Wait for the HID child PDO to arrive (async PnP install), then stamp
        // the USB bus-type GUID on the node + child. Without this the HID child
        // enumerates as a bare generic `HID\HIDCLASS` gamepad with no VID/PID, so
        // apps don't recognize the Sony identity and XInput-translation layers
        // wrap it as a virtual XInput pad. Poll up to ~3s.
        wait_for_hid_child(&instance_id, 3000);
        set_bus_type_usb(&instance_id);

        Ok(CreatedDevice { instance_id, controller_index })
    }
}

/// Poll until the devnode at `instance_id` has a child (its HID PDO), or
/// `timeout_ms` elapses. Returns true if a child appeared.
fn wait_for_hid_child(instance_id: &str, timeout_ms: u64) -> bool {
    let w_id = to_wide(instance_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        unsafe {
            let mut dev_inst: u32 = 0;
            if CM_Locate_DevNodeW(&mut dev_inst, w_id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL)
                == CR_SUCCESS
            {
                let mut child: u32 = 0;
                if CM_Get_Child(&mut child, dev_inst, 0) == CR_SUCCESS {
                    return true;
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Find the freshly-created HIDMaestro node under `ROOT\<enumerator>` and write
/// its `ControllerIndex`. Mirrors the post-DIF_REGISTERDEVICE registry walk in
/// `CreateDeviceNode`: the first present, HIDMaestro-owned node lacking a
/// `ControllerIndex` is the one we just made.
fn claim_new_node(enumerator: &str, controller_index: u32) -> Result<String, OrchestratorError> {
    use registry::*;
    let base = format!(r"SYSTEM\CurrentControlSet\Enum\ROOT\{enumerator}");
    let subkeys = enum_subkeys(HKLM, &base).unwrap_or_default();
    for inst in subkeys {
        let instance_id = format!(r"ROOT\{enumerator}\{inst}");
        // Must be present.
        if unsafe {
            CM_Locate_DevNodeW(
                &mut 0u32,
                to_wide(&instance_id).as_ptr(),
                CM_LOCATE_DEVNODE_NORMAL,
            )
        } != CR_SUCCESS
        {
            continue;
        }
        // Must be HIDMaestro-owned (HardwareID multi-sz contains "HIDMaestro").
        if !node_is_hidmaestro_owned(&instance_id) {
            continue;
        }
        let dp = format!(r"{base}\{inst}\Device Parameters");
        if read_dword(HKLM, &dp, "ControllerIndex").is_some() {
            continue; // already claimed
        }
        write_dword(HKLM, &dp, "ControllerIndex", controller_index)
            .map_err(|e| OrchestratorError::Win32("write ControllerIndex", e))?;
        return Ok(instance_id);
    }
    Err(OrchestratorError::NodeNotFound)
}

/// True if the node's HardwareID multi-sz contains the "HIDMaestro" ownership
/// tag. Port of the conservative `DeviceManager.IsHidMaestroOwned` check.
fn node_is_hidmaestro_owned(instance_id: &str) -> bool {
    use registry::*;
    let path = format!(r"SYSTEM\CurrentControlSet\Enum\{instance_id}");
    match read_multi_sz(HKLM, &path, "HardwareID") {
        Some(ids) => ids.iter().any(|s| s.contains("HIDMaestro")),
        None => false,
    }
}

/// Write the per-instance driver config under
/// `HKLM\SOFTWARE\HIDMaestro\Controller{index}`. The UMDF driver reads VID/PID/
/// descriptor/etc. from here at startup to report the device's identity. Port of
/// the plain-HID-relevant subset of `DeviceOrchestrator.WriteInstanceConfig`
/// (omits the FunctionMode/XUSB bits, which are Xbox-only).
fn write_instance_config(profile: &Profile, controller_index: u32) {
    use registry::*;
    let path = format!(r"SOFTWARE\HIDMaestro\Controller{controller_index}");
    let instance_suffix = format!("\\{controller_index:04}");
    let device_instance_id = format!(
        "ROOT\\VID_{:04X}&PID_{:04X}&IG_00{instance_suffix}",
        profile.vid, profile.pid
    );
    let display_name = profile
        .device_description
        .as_deref()
        .or(profile.product_string.as_deref())
        .unwrap_or("HIDMaestro Controller");

    // Best-effort: each write is independent; a failure on one shouldn't abort
    // creation (the driver tolerates some missing optional values).
    let _ = write_string(HKLM, &path, "DeviceInstanceId", &device_instance_id);
    let _ = write_dword(HKLM, &path, "FunctionMode", 0); // plain HID, no XUSB
    let _ = write_binary(HKLM, &path, "ReportDescriptor", &profile.descriptor);
    let _ = write_dword(HKLM, &path, "VendorId", profile.vid as u32);
    let _ = write_dword(HKLM, &path, "ProductId", profile.pid as u32);
    let _ = write_dword(HKLM, &path, "VersionNumber", profile.version_number);
    if let Some(ps) = profile.product_string.as_deref() {
        let _ = write_string(HKLM, &path, "ProductString", ps);
    }
    if profile.input_report_size > 0 {
        let _ = write_dword(HKLM, &path, "InputReportByteLength", profile.input_report_size as u32);
    }
    let _ = write_string(HKLM, &path, "DeviceDescription", display_name);

    // Joystick OEM display name (HKLM; joy.cpl reads it). Non-destructive — HKCU
    // wins for joy.cpl, so we only touch HKLM (the C# routes HKCU through a
    // capture/restore mechanism we don't replicate yet).
    let oem_path = format!(
        r"SYSTEM\CurrentControlSet\Control\MediaProperties\PrivateProperties\Joystick\OEM\VID_{:04X}&PID_{:04X}",
        profile.vid, profile.pid
    );
    let _ = write_string(HKLM, &oem_path, "OEMName", display_name);
}

/// One HIDMaestro-owned device discovered in the registry.
#[derive(Debug, Clone)]
pub struct ExistingDevice {
    pub instance_id: String,
    pub index: u32,
    pub vid: u16,
    pub pid: u16,
}

/// Enumerate HIDMaestro-owned device nodes currently present under
/// `ROOT\HIDClass`. Used for reclaim-on-startup (persistence on) and for
/// orphan cleanup (persistence off). A node counts if it's present *and*
/// HIDMaestro-owned (its HardwareID multi-sz carries the ownership tag).
///
/// The `index`/`vid`/`pid` are read back from the node's `ControllerIndex` and
/// its per-instance `HKLM\SOFTWARE\HIDMaestro\Controller{index}` config so the
/// app can re-attach the right profile to the right section.
pub fn list_hidmaestro_devices() -> Vec<ExistingDevice> {
    use registry::*;
    let enumerator = "HIDClass";
    let base = format!(r"SYSTEM\CurrentControlSet\Enum\ROOT\{enumerator}");
    let mut out = Vec::new();
    for inst in enum_subkeys(HKLM, &base).unwrap_or_default() {
        let instance_id = format!(r"ROOT\{enumerator}\{inst}");
        // Present?
        if unsafe {
            CM_Locate_DevNodeW(&mut 0u32, to_wide(&instance_id).as_ptr(), CM_LOCATE_DEVNODE_NORMAL)
        } != CR_SUCCESS
        {
            continue;
        }
        if !node_is_hidmaestro_owned(&instance_id) {
            continue;
        }
        let dp = format!(r"{base}\{inst}\Device Parameters");
        let index = read_dword(HKLM, &dp, "ControllerIndex").unwrap_or(u32::MAX);
        // VID/PID from the per-instance config (best-effort).
        let cfg = format!(r"SOFTWARE\HIDMaestro\Controller{index}");
        let vid = read_dword(HKLM, &cfg, "VendorId").unwrap_or(0) as u16;
        let pid = read_dword(HKLM, &cfg, "ProductId").unwrap_or(0) as u16;
        out.push(ExistingDevice { instance_id, index, vid, pid });
    }
    out
}

/// Remove every HIDMaestro-owned device node currently present. Returns the
/// number of nodes removed. Used for orphan cleanup when persistence is off
/// (on startup, and on parent-death teardown). Best-effort: a failure on one
/// node doesn't stop the others.
pub fn remove_all_hidmaestro_devices() -> usize {
    let mut removed = 0;
    for dev in list_hidmaestro_devices() {
        if matches!(remove_device_node(&dev.instance_id), Ok(true)) {
            removed += 1;
        }
    }
    removed
}

/// Remove a plain-HID device node previously created here. Port of the non-SWD
/// branch of `DeviceManager.RemoveDevice`: `DIF_REMOVE` the parent (root-
/// enumerated HID devices cascade their single HID child). Returns true if the
/// node is gone (or was never present). Requires elevation.
pub fn remove_device_node(instance_id: &str) -> Result<bool, OrchestratorError> {
    unsafe {
        let w_id = to_wide(instance_id);
        // Already gone?
        if CM_Locate_DevNodeW(&mut 0u32, w_id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) != CR_SUCCESS
            && CM_Locate_DevNodeW(&mut 0u32, w_id.as_ptr(), CM_LOCATE_DEVNODE_PHANTOM) != CR_SUCCESS
        {
            return Ok(true);
        }
        dif_remove(instance_id)
    }
}

/// `DIF_REMOVE` a single device by instance id via a fresh ALLCLASSES info set.
/// Port of `DeviceManager.DifRemoveDevice`.
unsafe fn dif_remove(instance_id: &str) -> Result<bool, OrchestratorError> {
    const DIGCF_ALLCLASSES: u32 = 0x0000_0004;
    let dis = SetupDiGetClassDevsW(
        std::ptr::null(),
        std::ptr::null(),
        std::ptr::null_mut(),
        DIGCF_ALLCLASSES,
    );
    if dis as isize == INVALID_HANDLE_VALUE {
        return Err(OrchestratorError::Win32("SetupDiGetClassDevs", GetLastError()));
    }
    let _guard = DisGuard(dis);

    let mut devinfo = new_devinfo();
    let w_id = to_wide(instance_id);
    if SetupDiOpenDeviceInfoW(dis, w_id.as_ptr(), std::ptr::null_mut(), 0, &mut devinfo) == 0 {
        // Node not in the set — treat as already removed.
        return Ok(true);
    }
    if SetupDiCallClassInstaller(DIF_REMOVE, dis, &mut devinfo) == 0 {
        return Err(OrchestratorError::Win32(
            "SetupDiCallClassInstaller(DIF_REMOVE)",
            GetLastError(),
        ));
    }
    // Confirm it's gone (NORMAL locate fails).
    let gone = CM_Locate_DevNodeW(&mut 0u32, w_id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) != CR_SUCCESS;
    Ok(gone)
}

/// RAII guard that destroys a SetupDi device-info-list handle.
struct DisGuard(*mut c_void);
impl Drop for DisGuard {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

/// Minimal registry helpers (advapi32) — only what the orchestrator needs:
/// enumerate subkeys, read/write a DWORD, read a REG_MULTI_SZ.
mod registry {
    use std::ffi::c_void;

    pub const HKLM: *mut c_void = 0x8000_0002u32 as usize as *mut c_void;

    const KEY_READ: u32 = 0x2_0019;
    const KEY_SET_VALUE: u32 = 0x0002;
    const KEY_CREATE_SUB_KEY: u32 = 0x0004;
    const REG_SZ: u32 = 1;
    const REG_BINARY: u32 = 3;
    const REG_DWORD: u32 = 4;
    const REG_MULTI_SZ: u32 = 7;
    const ERROR_SUCCESS: i32 = 0;
    const ERROR_MORE_DATA: i32 = 234;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(key: *mut c_void, sub: *const u16, opts: u32, access: u32, out: *mut *mut c_void) -> i32;
        fn RegCreateKeyExW(
            key: *mut c_void, sub: *const u16, reserved: u32, class: *const u16, options: u32,
            access: u32, sec: *mut c_void, out: *mut *mut c_void, disp: *mut u32,
        ) -> i32;
        fn RegCloseKey(key: *mut c_void) -> i32;
        fn RegEnumKeyExW(
            key: *mut c_void, index: u32, name: *mut u16, name_len: *mut u32, reserved: *mut u32,
            class: *mut u16, class_len: *mut u32, last_write: *mut c_void,
        ) -> i32;
        fn RegQueryValueExW(
            key: *mut c_void, name: *const u16, reserved: *mut u32, ty: *mut u32,
            data: *mut u8, data_len: *mut u32,
        ) -> i32;
        fn RegSetValueExW(
            key: *mut c_void, name: *const u16, reserved: u32, ty: u32, data: *const u8, data_len: u32,
        ) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn open(root: *mut c_void, path: &str, access: u32) -> Option<*mut c_void> {
        let mut h: *mut c_void = std::ptr::null_mut();
        let w = wide(path);
        let rc = unsafe { RegOpenKeyExW(root, w.as_ptr(), 0, access, &mut h) };
        if rc == ERROR_SUCCESS {
            Some(h)
        } else {
            None
        }
    }

    pub fn enum_subkeys(root: *mut c_void, path: &str) -> Option<Vec<String>> {
        let h = open(root, path, KEY_READ)?;
        let mut out = Vec::new();
        let mut idx = 0u32;
        loop {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            let rc = unsafe {
                RegEnumKeyExW(
                    h, idx, name.as_mut_ptr(), &mut len, std::ptr::null_mut(),
                    std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(),
                )
            };
            if rc != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
            idx += 1;
        }
        unsafe { RegCloseKey(h) };
        Some(out)
    }

    pub fn read_dword(root: *mut c_void, path: &str, name: &str) -> Option<u32> {
        let h = open(root, path, KEY_READ)?;
        let wn = wide(name);
        let mut ty = 0u32;
        let mut buf = [0u8; 4];
        let mut len = 4u32;
        let rc = unsafe {
            RegQueryValueExW(h, wn.as_ptr(), std::ptr::null_mut(), &mut ty, buf.as_mut_ptr(), &mut len)
        };
        unsafe { RegCloseKey(h) };
        if rc == ERROR_SUCCESS && ty == REG_DWORD {
            Some(u32::from_le_bytes(buf))
        } else {
            None
        }
    }

    pub fn write_string(root: *mut c_void, path: &str, name: &str, value: &str) -> Result<(), u32> {
        let h = create(root, path)?;
        let wn = wide(name);
        let wv = wide(value); // includes terminating NUL
        let bytes: Vec<u8> = wv.iter().flat_map(|w| w.to_le_bytes()).collect();
        let rc = unsafe {
            RegSetValueExW(h, wn.as_ptr(), 0, REG_SZ, bytes.as_ptr(), bytes.len() as u32)
        };
        unsafe { RegCloseKey(h) };
        if rc == ERROR_SUCCESS { Ok(()) } else { Err(rc as u32) }
    }

    pub fn write_binary(root: *mut c_void, path: &str, name: &str, value: &[u8]) -> Result<(), u32> {
        let h = create(root, path)?;
        let wn = wide(name);
        let rc = unsafe {
            RegSetValueExW(h, wn.as_ptr(), 0, REG_BINARY, value.as_ptr(), value.len() as u32)
        };
        unsafe { RegCloseKey(h) };
        if rc == ERROR_SUCCESS { Ok(()) } else { Err(rc as u32) }
    }

    /// Create-or-open a key for writing.
    fn create(root: *mut c_void, path: &str) -> Result<*mut c_void, u32> {
        let mut h: *mut c_void = std::ptr::null_mut();
        let w = wide(path);
        let rc = unsafe {
            RegCreateKeyExW(
                root, w.as_ptr(), 0, std::ptr::null(), 0,
                KEY_SET_VALUE | KEY_CREATE_SUB_KEY, std::ptr::null_mut(), &mut h, std::ptr::null_mut(),
            )
        };
        if rc != ERROR_SUCCESS { Err(rc as u32) } else { Ok(h) }
    }

    pub fn write_dword(root: *mut c_void, path: &str, name: &str, value: u32) -> Result<(), u32> {
        // Create-or-open the key (Device Parameters may not exist yet).
        let mut h: *mut c_void = std::ptr::null_mut();
        let w = wide(path);
        let rc = unsafe {
            RegCreateKeyExW(
                root, w.as_ptr(), 0, std::ptr::null(), 0,
                KEY_SET_VALUE | KEY_CREATE_SUB_KEY, std::ptr::null_mut(), &mut h, std::ptr::null_mut(),
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(rc as u32);
        }
        let wn = wide(name);
        let bytes = value.to_le_bytes();
        let rc = unsafe {
            RegSetValueExW(h, wn.as_ptr(), 0, REG_DWORD, bytes.as_ptr(), bytes.len() as u32)
        };
        unsafe { RegCloseKey(h) };
        if rc == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(rc as u32)
        }
    }

    pub fn read_multi_sz(root: *mut c_void, path: &str, name: &str) -> Option<Vec<String>> {
        let h = open(root, path, KEY_READ)?;
        let wn = wide(name);
        let mut ty = 0u32;
        let mut len = 0u32;
        // First call: size query.
        let rc = unsafe {
            RegQueryValueExW(h, wn.as_ptr(), std::ptr::null_mut(), &mut ty, std::ptr::null_mut(), &mut len)
        };
        if (rc != ERROR_SUCCESS && rc != ERROR_MORE_DATA) || ty != REG_MULTI_SZ || len == 0 {
            unsafe { RegCloseKey(h) };
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let rc = unsafe {
            RegQueryValueExW(h, wn.as_ptr(), std::ptr::null_mut(), &mut ty, buf.as_mut_ptr(), &mut len)
        };
        unsafe { RegCloseKey(h) };
        if rc != ERROR_SUCCESS {
            return None;
        }
        // Decode UTF-16 multi-sz.
        let u16s: Vec<u16> = buf[..len as usize]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let mut out = Vec::new();
        for part in u16s.split(|&w| w == 0) {
            if part.is_empty() {
                continue;
            }
            out.push(String::from_utf16_lossy(part));
        }
        Some(out)
    }
}
