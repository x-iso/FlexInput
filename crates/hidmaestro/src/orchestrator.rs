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

/// `GUID_DEVINTERFACE_XUSB` = `{EC87F1E3-C13B-4100-B5F7-8B84D54260CB}` — the device
/// interface `xinput1_x.dll` enumerates to discover XInput controllers.
const GUID_DEVINTERFACE_XUSB: Guid = Guid {
    data1: 0xec87_f1e3,
    data2: 0xc13b,
    data3: 0x4100,
    data4: [0xb5, 0xf7, 0x8b, 0x84, 0xd5, 0x42, 0x60, 0xcb],
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
    /// A container-GUID string couldn't be parsed for `SwDeviceCreate`.
    SwdBadGuid,
    /// `SwDeviceCreate` (or its async callback) returned a failure HRESULT.
    SwdCreate(u32),
    /// `SwDeviceCreate` neither fired its callback nor reached DN_STARTED in time.
    SwdTimeout,
    /// An XInput slot reorder could not be carried out (free-form detail).
    Reorder(String),
}

impl std::fmt::Display for OrchestratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrchestratorError::Win32(s, e) => write!(f, "{s} failed (err {e})"),
            OrchestratorError::NodeNotFound => write!(f, "created node not found in registry"),
            OrchestratorError::Unsupported(s) => write!(f, "unsupported profile path: {s}"),
            OrchestratorError::SwdBadGuid => write!(f, "invalid container GUID for SwDeviceCreate"),
            OrchestratorError::SwdCreate(hr) => write!(f, "SwDeviceCreate failed (hr 0x{hr:08X})"),
            OrchestratorError::SwdTimeout => write!(f, "SwDeviceCreate timed out"),
            OrchestratorError::Reorder(s) => write!(f, "xinput reorder: {s}"),
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
    fn CM_Get_Parent(parent: *mut u32, dev_inst: u32, flags: u32) -> u32;
    fn CM_Get_Sibling(sibling: *mut u32, dev_inst: u32, flags: u32) -> u32;
    fn CM_Get_DevNode_Status(status: *mut u32, problem: *mut u32, dev_inst: u32, flags: u32) -> u32;
    fn CM_Get_Device_ID_Size(len: *mut u32, dev_inst: u32, flags: u32) -> u32;
    fn CM_Get_Device_IDW(dev_inst: u32, buffer: *mut u16, buffer_len: u32, flags: u32) -> u32;
    fn CM_Set_DevNode_PropertyW(
        dev_inst: u32,
        property_key: *const DevPropKey,
        property_type: u32,
        property_buffer: *const u8,
        property_buffer_size: u32,
        flags: u32,
    ) -> u32;
    fn CM_Get_DevNode_PropertyW(
        dev_inst: u32,
        property_key: *const DevPropKey,
        property_type: *mut u32,
        property_buffer: *mut u8,
        property_buffer_size: *mut u32,
        flags: u32,
    ) -> u32;
    fn CM_Get_Device_Interface_List_SizeW(
        len: *mut u32,
        interface_class: *const Guid,
        device_id: *const u16,
        flags: u32,
    ) -> u32;
    fn CM_Get_Device_Interface_ListW(
        interface_class: *const Guid,
        device_id: *const u16,
        buffer: *mut u16,
        buffer_len: u32,
        flags: u32,
    ) -> u32;
    /// Disable a devnode (transient — flags 0 does NOT persist across reboot, so a
    /// reboot re-enables it as a safety net on top of our own watchdog).
    fn CM_Disable_DevNode(dev_inst: u32, flags: u32) -> u32;
    /// Re-enable a previously-disabled devnode.
    fn CM_Enable_DevNode(dev_inst: u32, flags: u32) -> u32;
}

/// Set a devnode (by PnP instance id) enabled or disabled. This is the
/// programmatic "reconnect" primitive behind XInput slot reordering: disabling a
/// devnode drops the device (freeing its XInput slot); enabling it makes it
/// re-arrive and claim the lowest free slot. **Transient** (flags 0): the disable
/// does not persist across a reboot, so a reboot is an automatic last-resort
/// recovery if our watchdog ever fails to re-enable.
pub fn set_devnode_enabled(instance_id: &str, enabled: bool) -> Result<(), OrchestratorError> {
    unsafe {
        let mut dev_inst: u32 = 0;
        let wide: Vec<u16> = instance_id.encode_utf16().chain(std::iter::once(0)).collect();
        let r = CM_Locate_DevNodeW(&mut dev_inst, wide.as_ptr(), CM_LOCATE_DEVNODE_NORMAL);
        if r != CR_SUCCESS {
            return Err(OrchestratorError::Win32("CM_Locate_DevNode (set_enabled)", r));
        }
        let r = if enabled {
            CM_Enable_DevNode(dev_inst, 0)
        } else {
            CM_Disable_DevNode(dev_inst, 0)
        };
        if r != CR_SUCCESS {
            return Err(OrchestratorError::Win32(
                if enabled { "CM_Enable_DevNode" } else { "CM_Disable_DevNode" },
                r,
            ));
        }
    }
    Ok(())
}

/// Re-enable a devnode, retrying a few times. Used by the reorder engine and its
/// crash-recovery path: leaving a controller (ours or the user's) disabled is the
/// one outcome we must never allow, so the re-enable is always insistent.
fn ensure_devnode_enabled(instance_id: &str) -> bool {
    for attempt in 0..6 {
        if set_devnode_enabled(instance_id, true).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(150 + attempt * 100));
    }
    false
}

/// Path of the reorder watchdog file. Holds one devnode instance id per line — the
/// set of XInput devnodes a reorder disabled. Written BEFORE the disable sequence
/// and deleted only once every node is confirmed re-enabled. On helper startup
/// [`recover_xinput_reorder`] re-enables anything still listed (a crash mid-reorder
/// would otherwise leave a controller disabled until the next reboot).
fn reorder_watchdog_path() -> std::path::PathBuf {
    std::env::temp_dir().join("flexinput_xinput_reorder.watchdog")
}

fn write_reorder_watchdog(nodes: &[String]) {
    let _ = std::fs::write(reorder_watchdog_path(), nodes.join("\n"));
}

fn clear_reorder_watchdog() {
    let _ = std::fs::remove_file(reorder_watchdog_path());
}

/// On helper startup, re-enable any devnodes a previous (crashed) reorder left
/// disabled, then clear the watchdog. Safe to call unconditionally — a missing or
/// empty file is a no-op.
pub fn recover_xinput_reorder() {
    let path = reorder_watchdog_path();
    let Ok(contents) = std::fs::read_to_string(&path) else { return };
    let mut recovered = 0u32;
    for line in contents.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if ensure_devnode_enabled(line) {
            recovered += 1;
        }
    }
    if recovered > 0 {
        eprintln!("[reorder] startup watchdog re-enabled {recovered} devnode(s) from a prior interrupted reorder");
    }
    let _ = std::fs::remove_file(&path);
}

/// Instance ids of every present devnode whose PnP id contains `marker` (matched
/// case-insensitively). Walks the live device tree from the root via CfgMgr32
/// child/sibling links — no SetupAPI enumeration set required. Used to find all
/// present XInput devnodes (their ids carry the `&IG_` interface tag).
fn present_devnodes_matching(marker: &str) -> Vec<String> {
    let mut out = Vec::new();
    let marker_up = marker.to_ascii_uppercase();
    unsafe {
        let mut root: u32 = 0;
        if CM_Locate_DevNodeW(&mut root, std::ptr::null(), CM_LOCATE_DEVNODE_NORMAL) != CR_SUCCESS {
            return out;
        }
        // Iterative DFS. For each node we enumerate its full child list (first
        // child + that child's sibling chain) and push them; popped nodes then
        // contribute their own children. The root id itself never matches.
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let mut child: u32 = 0;
            if CM_Get_Child(&mut child, node, 0) == CR_SUCCESS {
                stack.push(child);
                let mut cur = child;
                loop {
                    let mut sib: u32 = 0;
                    if CM_Get_Sibling(&mut sib, cur, 0) != CR_SUCCESS {
                        break;
                    }
                    stack.push(sib);
                    cur = sib;
                }
            }
            if let Some(id) = devnode_instance_id(node) {
                if id.to_ascii_uppercase().contains(&marker_up) {
                    out.push(id);
                }
            }
        }
    }
    out
}

/// All present XInput devnode instance ids (ids carrying the `&IG_` XInput
/// interface tag — this covers both physical Xbox XUSB nodes and our own virtual
/// companions, which enumerate under `ROOT\VID_..&PID_..&IG_00`).
pub fn present_xinput_devnodes() -> Vec<String> {
    present_devnodes_matching("&IG_")
}

/// Resolve a physical Xbox XInput devnode by USB `vid`/`pid` — the present `&IG_`
/// node whose id carries `VID_xxxx&PID_yyyy` and is NOT a `ROOT\` node (those are
/// our virtual companions). Returns the first match.
pub fn physical_xinput_devnode_for_vid_pid(vid: u16, pid: u16) -> Option<String> {
    let needle = format!("VID_{vid:04X}&PID_{pid:04X}");
    present_xinput_devnodes().into_iter().find(|id| {
        let up = id.to_ascii_uppercase();
        up.contains(&needle) && !up.starts_with("ROOT\\")
    })
}

/// Force `target_instance` onto ordinal `slot` (0-based) by an ordered re-arrival
/// of the given XInput `participants`. Disables all of them, then re-enables them
/// one at a time — the target at index `slot`, the others filling the remaining
/// positions in their existing order — with a settle delay so each claims the next
/// XInput user index in turn.
///
/// `participants` is supplied by the caller because the slot-holding devnode is
/// NOT uniformly discoverable: a physical Xbox's holder carries the `&IG_` tag
/// (see [`present_xinput_devnodes`]), but our own virtual's holder is its
/// `SWD\HIDMAESTRO` XUSB companion, which has no such tag and is known only to the
/// helper. The caller unions both and passes the target alongside.
///
/// Safety: the full disabled set is persisted to the watchdog file before the
/// disable sequence and cleared only after every node is confirmed re-enabled; a
/// crash mid-sequence is recovered by [`recover_xinput_reorder`] on the next helper
/// start, and the transient `CM_Disable` flag means a reboot also re-enables
/// everything. Every node is re-enabled on completion AND on any error path.
pub fn reorder_xinput_slots(
    participants: &[String],
    target_instance: &str,
    slot: usize,
) -> Result<(), OrchestratorError> {
    // De-dup participants case-insensitively, preserving order, and guarantee the
    // target is included even if the caller forgot it.
    let mut nodes: Vec<String> = Vec::new();
    for p in participants.iter().chain(std::iter::once(&target_instance.to_string())) {
        if !nodes.iter().any(|n: &String| n.eq_ignore_ascii_case(p)) {
            nodes.push(p.clone());
        }
    }
    if nodes.is_empty() {
        return Err(OrchestratorError::Reorder("no XInput participants".into()));
    }
    // Build the desired re-enable order: others before `slot`, target, others after.
    let others: Vec<String> = nodes
        .iter()
        .filter(|n| !n.eq_ignore_ascii_case(target_instance))
        .cloned()
        .collect();
    let slot = slot.min(others.len());
    let mut order: Vec<String> = Vec::with_capacity(nodes.len());
    order.extend_from_slice(&others[..slot]);
    order.push(target_instance.to_string());
    order.extend_from_slice(&others[slot..]);

    // Single device that's already our target → identical to a plain re-arrive; the
    // ordered path still works, but skip the watchdog churn.
    eprintln!(
        "[reorder] placing {target_instance} at slot {slot} among {} XInput devnode(s)",
        nodes.len()
    );

    // Persist the watchdog FIRST so a crash between here and the re-enable loop is
    // recoverable.
    write_reorder_watchdog(&nodes);

    // Phase 1: disable every XInput devnode. Tolerate individual failures (a
    // policy-locked controller may refuse) — those simply keep their slot.
    for n in &nodes {
        if let Err(e) = set_devnode_enabled(n, false) {
            eprintln!("[reorder] disable {n} failed: {e} (leaving it in place)");
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(250));

    // Phase 2: re-enable in the desired order, settling between each so the XInput
    // stack assigns user indices in arrival order.
    let mut failed: Vec<String> = Vec::new();
    for n in &order {
        if !ensure_devnode_enabled(n) {
            eprintln!("[reorder] re-enable {n} FAILED");
            failed.push(n.clone());
        }
        std::thread::sleep(std::time::Duration::from_millis(600));
    }

    // Phase 3 (belt-and-suspenders): make sure EVERY node we touched is enabled,
    // even any not in `order`, before clearing the watchdog.
    for n in &nodes {
        if !order.iter().any(|o| o.eq_ignore_ascii_case(n)) {
            let _ = ensure_devnode_enabled(n);
        }
    }

    if failed.is_empty() {
        clear_reorder_watchdog();
        eprintln!("[reorder] done; {target_instance} should now hold slot {slot}");
        Ok(())
    } else {
        // Leave the watchdog in place so startup recovery retries the stragglers.
        Err(OrchestratorError::Reorder(format!(
            "re-enable failed for {} devnode(s): {}",
            failed.len(),
            failed.join(", ")
        )))
    }
}

/// The instance id string of a devnode (`CM_Get_Device_IDW`).
fn devnode_instance_id(dev_inst: u32) -> Option<String> {
    unsafe {
        let mut len: u32 = 0;
        if CM_Get_Device_ID_Size(&mut len, dev_inst, 0) != CR_SUCCESS {
            return None;
        }
        // len excludes the NUL; allocate +1.
        let mut buf = vec![0u16; len as usize + 1];
        if CM_Get_Device_IDW(dev_inst, buf.as_mut_ptr(), buf.len() as u32, 0) != CR_SUCCESS {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

/// All immediate child device-instance ids of `parent_inst`
/// (`CM_Get_Child` + `CM_Get_Sibling` loop). Port of
/// `DeviceManager.GetAllChildDeviceIds`. These are the HID PDOs that must be
/// removed explicitly — `DIF_REMOVE` on the root parent does NOT cascade them,
/// so they survive as orphaned `HID\HIDCLASS\...` "game controller" nodes bound
/// to the generic input.inf driver (the device-leak bug).
fn child_device_ids(parent_inst: u32) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let mut child: u32 = 0;
        if CM_Get_Child(&mut child, parent_inst, 0) != CR_SUCCESS {
            return out;
        }
        loop {
            if let Some(id) = devnode_instance_id(child) {
                out.push(id);
            }
            let mut sib: u32 = 0;
            if CM_Get_Sibling(&mut sib, child, 0) != CR_SUCCESS {
                break;
            }
            child = sib;
        }
    }
    out
}

/// DEVPROPKEY — `{ fmtid: GUID, pid: u32 }`.
#[repr(C)]
struct DevPropKey {
    fmtid: Guid,
    pid: u32,
}

const DEVPROP_TYPE_GUID: u32 = 0x0000_000D;
const DEVPROP_TYPE_STRING: u32 = 0x0000_0012;

/// `DEVPKEY_Device_DeviceDesc` = `{a45c254e-df1c-4efd-8020-67d146a850e0}, 2`.
const DEVPKEY_DEVICE_DESC: DevPropKey = DevPropKey {
    fmtid: Guid {
        data1: 0xa45c_254e,
        data2: 0xdf1c,
        data3: 0x4efd,
        data4: [0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0],
    },
    pid: 2,
};

/// `DEVPKEY_Device_FriendlyName` = same fmtid, pid 14.
const DEVPKEY_DEVICE_FRIENDLY_NAME: DevPropKey = DevPropKey {
    fmtid: Guid {
        data1: 0xa45c_254e,
        data2: 0xdf1c,
        data3: 0x4efd,
        data4: [0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0],
    },
    pid: 14,
};

/// `DEVPKEY_Device_BusReportedDeviceDesc` =
/// `{540b947e-8b40-45bc-a8a2-6a0b894cbda2}, 4`.
const DEVPKEY_BUS_REPORTED_DEVICE_DESC: DevPropKey = DevPropKey {
    fmtid: Guid {
        data1: 0x540b_947e,
        data2: 0x8b40,
        data3: 0x45bc,
        data4: [0xa8, 0xa2, 0x6a, 0x0b, 0x89, 0x4c, 0xbd, 0xa2],
    },
    pid: 4,
};

/// Set FriendlyName + DeviceDesc + BusReportedDeviceDesc to `name` on the node
/// at `instance_id` and its first HID child. Port of
/// `DeviceProperties.SetAllNamingProperties` — this is what makes the device
/// show as e.g. "Wireless Controller" instead of the generic "HID-compliant
/// game controller" in Device Manager / joy.cpl. Best-effort.
fn set_all_naming_properties(instance_id: &str, name: &str) {
    // DEVPROP string value = UTF-16 + terminating NUL, as bytes.
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes: Vec<u8> = wide.iter().flat_map(|w| w.to_le_bytes()).collect();
    unsafe {
        let w_id = to_wide(instance_id);
        let mut dev_inst: u32 = 0;
        if CM_Locate_DevNodeW(&mut dev_inst, w_id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) != CR_SUCCESS {
            return;
        }
        let stamp = |inst: u32| {
            for key in [
                &DEVPKEY_DEVICE_FRIENDLY_NAME,
                &DEVPKEY_DEVICE_DESC,
                &DEVPKEY_BUS_REPORTED_DEVICE_DESC,
            ] {
                CM_Set_DevNode_PropertyW(
                    inst,
                    key,
                    DEVPROP_TYPE_STRING,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    0,
                );
            }
        };
        stamp(dev_inst);
        let mut child: u32 = 0;
        if CM_Get_Child(&mut child, dev_inst, 0) == CR_SUCCESS {
            stamp(child);
        }
    }
}

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

fn new_devinfo_for(class_guid: Guid) -> SpDevinfoData {
    SpDevinfoData {
        cb_size: std::mem::size_of::<SpDevinfoData>() as u32,
        class_guid,
        dev_inst: 0,
        reserved: 0,
    }
}

fn new_devinfo() -> SpDevinfoData {
    new_devinfo_for(HID_CLASS_GUID)
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
    device_id: &str,
) -> Result<CreatedDevice, OrchestratorError> {
    let desc = &profile.name;
    // The main HID node is the gamepad's HID identity. For an Xbox360-family
    // profile (XUSB companion) it follows the shipping HIDMaestro layout
    // (DeviceNodeCreator, Xbox-legacy path): the HardwareID carries the **`&IG_00`**
    // suffix — which makes HIDAPI/SDL3 skip it (gamecontroller blocklist) so they
    // fall back to XInput — and the enumerator matches that form. The XInput
    // identity itself comes from the separate XUSB companion (create_xusb_companion_
    // node); this node just supplies the HID/WGI gamepad face with FunctionMode=1.
    // Plain-HID profiles (DS4/DualSense) keep the standard HIDClass enumerator.
    let (enumerator, hw_id) = if profile.requires_xusb_companion {
        let e = format!("VID_{:04X}&PID_{:04X}&IG_00", profile.vid, profile.pid);
        let h = format!("root\\{e}");
        (e, h)
    } else {
        (
            "HIDClass".to_string(),
            format!("root\\VID_{:04X}&PID_{:04X}", profile.vid, profile.pid),
        )
    };
    let enumerator = enumerator.as_str();

    // Per-instance driver config (VID/PID/descriptor/etc.). The UMDF driver reads
    // this at startup to know what identity to report; WITHOUT it the driver
    // falls back to the Microsoft default VID_045E and the device presents as an
    // Xbox pad instead of the profile's real (Sony/etc.) identity.
    write_instance_config(profile, controller_index, device_id, profile.function_mode);

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

        // NOTE: we deliberately do NOT set the `USB\MS_COMP_XUSB10` compatible IDs
        // on the main HID node, even though upstream's Xbox-legacy path does. Those
        // compat IDs cause Windows to attach the inbox `xinputhid` upper filter to
        // the HID node, which makes the HID node publish its OWN XInput face — a
        // SECOND xinput1_4 slot, serving the EMPTY HID Data[] that XInput profiles
        // write (the stuck/garbage virtual pad that tangled with a real one). The
        // SWD companion is the SOLE XInput identity (it publishes {EC87F1E3}); the
        // main node is only the HID/WGI gamepad face. Upstream tolerates the dual
        // face because their consumer (WGI) dedups two faces sharing the HIDMAESTRO
        // name — but xinput1_4 (what FlexInput's gilrs reads) does NOT dedup, so it
        // would see two slots. Keeping just the &IG_00 hardware id still makes
        // SDL/HIDAPI defer the HID face to XInput.

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

        // Friendly name on root + HID child (e.g. "Wireless Controller") so the
        // device doesn't show as a generic "HID-compliant game controller".
        // This is the Windows DeviceDesc/FriendlyName and is kept CLEAN — gilrs's
        // WGI backend does NOT read it (it reads the USB product string via
        // RawGameController.DisplayName), so the own-virtual marker lives on the
        // product string instead (see `write_instance_config`), not here.
        let display_name = profile
            .device_description
            .as_deref()
            .or(profile.product_string.as_deref())
            .unwrap_or(&profile.name);
        set_all_naming_properties(&instance_id, display_name);

        // Block until the driver has actually started on the HID child, so the
        // app's first writes land on a listening driver (else the device looks
        // dead until relaunch). Best-effort with a generous budget.
        wait_for_hid_child_started(&instance_id, 5000);

        Ok(CreatedDevice { instance_id, controller_index })
    }
}

/// Per-controller deterministic ContainerID = `{48494430-4D41-4553-5452-4F000000XXXX}`
/// (ASCII "HIDMAESTRO" + 16-bit controller index). Verbatim from upstream
/// `SwdDeviceFactory.ContainerIdFor`. A **non-sentinel** ContainerID on the XUSB
/// companion is the actual fix for xinput1_4's slot-0-skip (a null-sentinel
/// container made the allocator treat the pad as embedded/primary and skip slot 0).
/// Formatted as a registry GUID string `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`.
fn container_id_for(controller_index: u32) -> String {
    let idx = controller_index as u16;
    // Guid {0x48494430, 0x4D41, 0x4553, [0x54,0x52,0x4F,0x00,0x00,0x00, hi, lo]}.
    // Registry string form splits data4 as [0..2]="5452" then [2..8]=
    // "4F 00 00 00 hi lo" → the final 12-hex segment is 4F000000<hi><lo>.
    format!(
        "{{48494430-4D41-4553-5452-4F000000{:02X}{:02X}}}",
        (idx >> 8) & 0xFF,
        idx & 0xFF
    )
}

/// Monotonic per-process sequence so every `SwDeviceCreate` gets a UNIQUE
/// instance-id suffix. Required: Windows keeps a sticky per-(container+suffix)
/// record after teardown; a subsequent create with an identical tuple takes a
/// fast "re-enumerate" path that leaves the devnode an EMPTY SHELL (no driver
/// bound, no XUSB interface). A fresh suffix every create dodges it.
static SWD_CREATE_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Build the unique SWD instance-id suffix: `<pidhex><seqhex>_<ctrlidx:04>`
/// (mirrors upstream `DeviceOrchestrator.NextSwdSuffix` intent — a session-unique
/// prefix + per-create counter + the controller index for human-readable teardown
/// matching). The companion is found by `ControllerIndex` in Device Parameters, not
/// by suffix, so varying the suffix per call is transparent to teardown.
fn next_swd_suffix(controller_index: u32) -> String {
    let pid = std::process::id();
    let seq = SWD_CREATE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{pid:08X}{seq:04X}_{controller_index:04}")
}

/// Create the XUSB/XInput **companion** devnode for an Xbox360-family profile via
/// in-process [`swd_create`] (`SwDeviceCreate`), matching the shipping HIDMaestro
/// design (`DeviceOrchestrator.CreateXusbCompanion`). Requires elevation.
///
/// This is the node that publishes `GUID_DEVINTERFACE_XUSB` (via `HMXInput.dll`/
/// UMDF) so `xinput1_4.dll` discovers an XInput controller. It is created under the
/// **SWD** enumerator `HIDMAESTRO` with an explicit **non-sentinel ContainerID**
/// ([`container_id_for`]) — the actual slot-0-skip fix. `hidmaestro_xusb.inf`
/// (installed by `deploy.rs` into the DriverStore) binds via `DriverRequired`,
/// applying its `.NT.Wdf` UMDF binding, `XusbMode=1`, `UpperFilters=xinputhid`, and
/// the `{EC87F1E3}` AddInterface.
///
/// **Lifetime: the returned [`SwdHandle`] OWNS the node.** `SwDeviceCreate` is called
/// with the DEFAULT (`Handle`) lifetime, so the node lives exactly as long as the
/// handle is held — dropping it removes the node (synchronous, reliable). This is the
/// ONLY teardown that actually works on Win10 19045: the `ParentPresent` +
/// reconnect-and-downgrade path is a cosmetic no-op there (every reconnect/
/// SetLifetime/Close returns hr=0 yet the node survives). Holding the handle in the
/// long-lived elevated helper also makes a helper crash auto-remove the nodes (no
/// zombies).
///
/// The `_xusb_inf_path` is unused (the INF binds from the DriverStore); kept for
/// signature stability. Returns `(instance_id, owning handle)`.
pub fn create_xusb_companion_node(
    profile: &Profile,
    _xusb_inf_path: &str,
    controller_index: u32,
) -> Result<(String, SwdHandle), OrchestratorError> {
    let vid = format!("{:04X}", profile.vid);
    let pid = format!("{:04X}", profile.pid);
    let desc = format!("{} (XInput)", profile.name);

    // Exact hardware/compat IDs from upstream CreateXusbCompanion. The XI alias +
    // generic `root\HIDMaestroXUSB` are the INF [Models] match keys; the XUSB
    // compat IDs are what WGI/GameInputSvc recognize as an Xbox gamepad.
    let xi_alias = format!("root\\VID_{vid}&PID_{pid}&XI_00");
    let hw_ids: [&str; 2] = [&xi_alias, "root\\HIDMaestroXUSB"];
    let compat_ids: [&str; 4] = [
        "USB\\MS_COMP_XUSB10",
        "USB\\Class_FF&SubClass_5D&Prot_01",
        "USB\\Class_FF&SubClass_5D",
        "USB\\Class_FF",
    ];
    let container = container_id_for(controller_index);
    let suffix = next_swd_suffix(controller_index);

    let (instance_id, handle) =
        swd_create("HIDMAESTRO", &suffix, &container, &hw_ids, &compat_ids, &desc)?;

    // The driver reads ControllerIndex from Device Parameters at startup to attach
    // to the right shared section (Global\HIDMaestroInput<N>).
    {
        use registry::*;
        let dp = format!(r"SYSTEM\CurrentControlSet\Enum\{instance_id}\Device Parameters");
        let _ = write_dword(HKLM, &dp, "ControllerIndex", controller_index);
    }

    // SwDeviceCreate(DriverRequired) binds the INF synchronously, but the XUSB
    // interface coinstaller can lag a few seconds after the callback. Wait for the
    // {EC87F1E3} interface to actually publish on this node before returning.
    for _ in 0..30 {
        if count_xusb_interfaces(Some(&instance_id)) >= 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    Ok((instance_id, handle))
}

// ── In-process SwDeviceCreate (cfgmgr32!SwDevice*, link SwDevice.lib) ─────────
//
// `SwDeviceCreate` is the only user-mode API that can set a real ContainerID. We
// call it IN-PROCESS (not via a child exe) specifically so the elevated helper can
// HOLD the resulting HSWDEVICE for the node's lifetime — the only reliable teardown
// on Win10 19045. (.NET's loader bug that forced upstream's native helper does not
// apply to Rust — no managed loader in the call path.)

/// `SW_DEVICE_CREATE_INFO` — EXACT layout from swdevicedef.h (9 fields, ends at
/// pSecurityDescriptor — there are NO trailing property fields; getting cbSize
/// wrong makes SwDeviceCreate return E_INVALIDARG 0x80070057).
#[repr(C)]
struct SwDeviceCreateInfo {
    cb_size: u32,
    pszz_instance_id: *const u16,
    pszz_hardware_ids: *const u16,
    pszz_compatible_ids: *const u16,
    p_container_id: *const Guid,
    capability_flags: u32,
    psz_device_description: *const u16,
    psz_device_location: *const u16,
    p_security_descriptor: *const c_void,
}

const SW_DEVICE_CAPABILITIES_SILENT_INSTALL: u32 = 0x0000_0002;
const SW_DEVICE_CAPABILITIES_DRIVER_REQUIRED: u32 = 0x0000_0008;
const SW_DEVICE_CAPABILITIES_NO_DISPLAY_IN_UI: u32 = 0x0000_0004;

#[link(name = "SwDevice")]
extern "system" {
    fn SwDeviceCreate(
        psz_enumerator_name: *const u16,
        psz_parent_device_instance: *const u16,
        p_create_info: *const SwDeviceCreateInfo,
        c_property_count: u32,
        p_properties: *const c_void,
        pf_callback: extern "system" fn(*mut c_void, i32, *mut c_void, *const u16),
        p_context: *mut c_void,
        ph_sw_device: *mut *mut c_void,
    ) -> i32;
    fn SwDeviceClose(h_sw_device: *mut c_void);
}

/// Owning handle to an SWD-created devnode. Default (`Handle`) lifetime means the
/// node exists exactly while this is alive; `Drop` closes it → the node is removed.
pub struct SwdHandle(*mut c_void);
// The HSWDEVICE is just an opaque kernel handle; moving it across threads is safe
// (the helper creates on one thread, may drop on another during teardown).
unsafe impl Send for SwdHandle {}
unsafe impl Sync for SwdHandle {}
impl Drop for SwdHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { SwDeviceClose(self.0) };
            self.0 = std::ptr::null_mut();
        }
    }
}

/// Shared state for the create callback (the callback runs on a PnP worker thread).
struct SwdCbState {
    done: std::sync::Mutex<Option<(i32, String)>>,
    cv: std::sync::Condvar,
}

extern "system" fn swd_create_callback(
    _h: *mut c_void,
    create_result: i32,
    context: *mut c_void,
    instance_id: *const u16,
) {
    if context.is_null() {
        return;
    }
    let state = unsafe { &*(context as *const SwdCbState) };
    let id = if instance_id.is_null() {
        String::new()
    } else {
        let mut len = 0usize;
        unsafe {
            while *instance_id.add(len) != 0 {
                len += 1;
            }
        }
        let slice = unsafe { std::slice::from_raw_parts(instance_id, len) };
        String::from_utf16_lossy(slice)
    };
    *state.done.lock().unwrap() = Some((create_result, id));
    state.cv.notify_all();
}

/// In-process `SwDeviceCreate` with DEFAULT (`Handle`) lifetime. Returns the
/// instance id + the owning handle. Waits for the create callback (or the
/// DN_STARTED fast-path) up to ~30s. The node persists only while the returned
/// handle is held.
fn swd_create(
    enumerator: &str,
    suffix: &str,
    container_guid: &str,
    hw_ids: &[&str],
    compat_ids: &[&str],
    description: &str,
) -> Result<(String, SwdHandle), OrchestratorError> {
    let w_enum = to_wide(enumerator);
    let w_parent = to_wide("HTREE\\ROOT\\0");
    let w_suffix = to_wide(suffix);
    let w_hw = multi_sz_wide(hw_ids);
    let w_compat = multi_sz_wide(compat_ids);
    let w_desc = to_wide(description);
    let container = parse_guid_str(container_guid).ok_or(OrchestratorError::SwdBadGuid)?;

    let info = SwDeviceCreateInfo {
        cb_size: std::mem::size_of::<SwDeviceCreateInfo>() as u32,
        pszz_instance_id: w_suffix.as_ptr(),
        pszz_hardware_ids: w_hw.as_ptr(),
        pszz_compatible_ids: w_compat.as_ptr(),
        p_container_id: &container,
        capability_flags: SW_DEVICE_CAPABILITIES_SILENT_INSTALL
            | SW_DEVICE_CAPABILITIES_NO_DISPLAY_IN_UI
            | SW_DEVICE_CAPABILITIES_DRIVER_REQUIRED,
        psz_device_description: w_desc.as_ptr(),
        psz_device_location: std::ptr::null(),
        p_security_descriptor: std::ptr::null(),
    };

    let state = std::sync::Arc::new(SwdCbState {
        done: std::sync::Mutex::new(None),
        cv: std::sync::Condvar::new(),
    });
    let ctx = std::sync::Arc::as_ptr(&state) as *mut c_void;

    let mut h: *mut c_void = std::ptr::null_mut();
    let hr = unsafe {
        SwDeviceCreate(
            w_enum.as_ptr(),
            w_parent.as_ptr(),
            &info,
            0,
            std::ptr::null(),
            swd_create_callback,
            ctx,
            &mut h,
        )
    };
    if hr < 0 {
        return Err(OrchestratorError::SwdCreate(hr as u32));
    }
    // Own the handle immediately so any early return still closes it.
    let handle = SwdHandle(h);

    // Wait for the callback OR the DN_STARTED fast path (the callback sometimes
    // never fires on the reuse path even though the node is live).
    let expected_id = format!(r"SWD\{enumerator}\{suffix}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        {
            let guard = state.done.lock().unwrap();
            if let Some((cb_hr, id)) = guard.as_ref() {
                if *cb_hr < 0 {
                    return Err(OrchestratorError::SwdCreate(*cb_hr as u32));
                }
                let id = if id.is_empty() { expected_id.clone() } else { id.clone() };
                return Ok((id, handle));
            }
        }
        // DN_STARTED fast-path probe.
        let mut dev_inst = 0u32;
        if unsafe {
            CM_Locate_DevNodeW(
                &mut dev_inst,
                to_wide(&expected_id).as_ptr(),
                CM_LOCATE_DEVNODE_NORMAL,
            )
        } == CR_SUCCESS
        {
            let mut status = 0u32;
            let mut problem = 0u32;
            if unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, dev_inst, 0) }
                == CR_SUCCESS
                && status & DN_STARTED != 0
            {
                return Ok((expected_id, handle));
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(OrchestratorError::SwdTimeout);
        }
        // Wait a slice for the callback; loop re-checks DN_STARTED.
        let guard = state.done.lock().unwrap();
        let _ = state
            .cv
            .wait_timeout(guard, std::time::Duration::from_millis(100));
    }
}

/// Build a UTF-16 double-NUL-terminated multi-sz (`PCZZWSTR`) from a string list.
fn multi_sz_wide(items: &[&str]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    for it in items {
        out.extend(it.encode_utf16());
        out.push(0);
    }
    out.push(0); // final terminator
    out
}

/// Parse `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` (braces optional) into a `Guid`.
fn parse_guid_str(s: &str) -> Option<Guid> {
    let s = s.trim().trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return None;
    }
    let data1 = u32::from_str_radix(parts[0], 16).ok()?;
    let data2 = u16::from_str_radix(parts[1], 16).ok()?;
    let data3 = u16::from_str_radix(parts[2], 16).ok()?;
    let d4hi = u16::from_str_radix(parts[3], 16).ok()?;
    let d4lo = u64::from_str_radix(parts[4], 16).ok()?;
    let mut data4 = [0u8; 8];
    data4[0] = (d4hi >> 8) as u8;
    data4[1] = (d4hi & 0xFF) as u8;
    let lo_bytes = d4lo.to_be_bytes(); // 8 bytes, low 6 are the node
    data4[2..8].copy_from_slice(&lo_bytes[2..8]);
    Some(Guid { data1, data2, data3, data4 })
}

/// `DEVPKEY_Device_DriverInfPath` = `{a8b865dd-2e3d-4094-ad97-e593a70c75d6}, 5`.
const DEVPKEY_DEVICE_DRIVER_INF_PATH: DevPropKey = DevPropKey {
    fmtid: Guid { data1: 0xa8b8_65dd, data2: 0x2e3d, data3: 0x4094, data4: [0xad, 0x97, 0xe5, 0x93, 0xa7, 0x0c, 0x75, 0xd6] },
    pid: 5,
};

/// Read a string DEVPKEY off a devnode (for `node_diag`). `None` if absent.
fn read_devnode_string_prop(dev_inst: u32, key: &DevPropKey) -> Option<String> {
    unsafe {
        let mut ty = 0u32;
        let mut len = 0u32;
        let _ = CM_Get_DevNode_PropertyW(dev_inst, key, &mut ty, std::ptr::null_mut(), &mut len, 0);
        if len == 0 { return None; }
        let mut buf = vec![0u8; len as usize];
        if CM_Get_DevNode_PropertyW(dev_inst, key, &mut ty, buf.as_mut_ptr(), &mut len, 0) != CR_SUCCESS { return None; }
        let u16s: Vec<u16> = buf[..len as usize].chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&w| w != 0).collect();
        Some(String::from_utf16_lossy(&u16s))
    }
}

/// Count present `GUID_DEVINTERFACE_XUSB` interfaces, optionally filtered to a
/// single device instance id (`None` = system-wide). For the validation probe.
fn count_xusb_interfaces(device_id: Option<&str>) -> usize {
    // CfgMgr32 flag (NOT the SetupDi DIGCF_PRESENT 0x100 — that's a different API
    // and an invalid value here, which made this always return 0 even when the
    // interface existed). CM_GET_DEVICE_INTERFACE_LIST_PRESENT = 0 (present-only
    // is the default).
    const CM_GET_DEVICE_INTERFACE_LIST_PRESENT: u32 = 0x0000_0000;
    let w_id = device_id.map(to_wide);
    let id_ptr = w_id.as_ref().map(|v| v.as_ptr()).unwrap_or(std::ptr::null());
    unsafe {
        let mut len = 0u32;
        if CM_Get_Device_Interface_List_SizeW(
            &mut len,
            &GUID_DEVINTERFACE_XUSB,
            id_ptr,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        ) == CR_SUCCESS
            && len > 1
        {
            let mut buf = vec![0u16; len as usize];
            if CM_Get_Device_Interface_ListW(
                &GUID_DEVINTERFACE_XUSB,
                id_ptr,
                buf.as_mut_ptr(),
                len,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            ) == CR_SUCCESS
            {
                return buf.split(|&c| c == 0).filter(|s| !s.is_empty()).count();
            }
        }
    }
    0
}

/// Diagnostic snapshot of a node for the validation probe.
#[derive(Debug, Clone)]
pub struct NodeDiag {
    pub status: Option<u32>,
    pub problem: u32,
    pub started: bool,
    /// XUSB interfaces on this node, its child PDO, and system-wide.
    pub xusb_interfaces: usize,
    pub xusb_interfaces_child: usize,
    pub xusb_interfaces_global: usize,
    pub driver_inf: Option<String>,
}

/// Read status/problem/started + XUSB interface counts + bound INF for a node.
pub fn node_diag(instance_id: &str) -> NodeDiag {
    let w_id = to_wide(instance_id);
    let mut status = None;
    let mut problem = 0u32;
    let mut started = false;
    let mut driver_inf = None;
    unsafe {
        let mut dev_inst: u32 = 0;
        if CM_Locate_DevNodeW(&mut dev_inst, w_id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) == CR_SUCCESS {
            let mut st = 0u32;
            let mut pr = 0u32;
            if CM_Get_DevNode_Status(&mut st, &mut pr, dev_inst, 0) == CR_SUCCESS {
                status = Some(st);
                problem = pr;
                started = (st & DN_STARTED) != 0;
            }
            driver_inf = read_devnode_string_prop(dev_inst, &DEVPKEY_DEVICE_DRIVER_INF_PATH);
            let mut child: u32 = 0;
            if CM_Get_Child(&mut child, dev_inst, 0) == CR_SUCCESS {
                let mut cst = 0u32;
                let mut cpr = 0u32;
                if CM_Get_DevNode_Status(&mut cst, &mut cpr, child, 0) == CR_SUCCESS && (cst & DN_STARTED) != 0 {
                    started = true;
                }
            }
        }
    }
    let xusb_interfaces = count_xusb_interfaces(Some(instance_id));
    let xusb_interfaces_global = count_xusb_interfaces(None);
    let xusb_interfaces_child = unsafe {
        let mut di: u32 = 0;
        let mut child: u32 = 0;
        if CM_Locate_DevNodeW(&mut di, w_id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) == CR_SUCCESS
            && CM_Get_Child(&mut child, di, 0) == CR_SUCCESS
        {
            devnode_instance_id(child).map(|c| count_xusb_interfaces(Some(&c))).unwrap_or(0)
        } else { 0 }
    };
    NodeDiag { status, problem, started, xusb_interfaces, xusb_interfaces_child, xusb_interfaces_global, driver_inf }
}

/// `DN_STARTED` bit in the devnode status word (the device is started and its
/// driver is running). When set with problem code 0 the UMDF driver has bound
/// and opened its side of the shared section.
const DN_STARTED: u32 = 0x0000_0008;

/// Poll until the HID child of `instance_id` reaches the **started** state (its
/// driver is running and reading the section), or `timeout_ms` elapses. Returns
/// true if it started. This is stronger than [`wait_for_hid_child`], which only
/// waits for the child PDO to exist — the section isn't actually being read
/// until the driver is *started*, and writing before then is what made a
/// freshly-created device look dead until the app was relaunched.
///
/// Public so the cross-run reclaim path (server.rs) can wait for a *surviving*
/// node's driver to re-bind to the freshly re-created section before returning —
/// otherwise the app's first writes race the driver and the reclaimed device
/// looks dead until yet another relaunch.
pub fn wait_for_hid_child_started(instance_id: &str, timeout_ms: u64) -> bool {
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
                    let mut status: u32 = 0;
                    let mut problem: u32 = 0;
                    if CM_Get_DevNode_Status(&mut status, &mut problem, child, 0) == CR_SUCCESS
                        && (status & DN_STARTED) != 0
                        && problem == 0
                    {
                        return true;
                    }
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
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

/// Find the freshly-created XUSB companion node under `ROOT\System` and stamp its
/// `ControllerIndex`. Like [`claim_new_node`] but scoped to System + nodes whose
/// HardwareID carries the XUSB tag, so it never collides with the HIDClass claim.
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
/// descriptor/`FunctionMode`/etc. from here at startup to report the device's
/// identity AND whether to behave as a plain-HID device (`function_mode = 0`) or
/// an XUSB/XInput device (`function_mode = 1`). Port of the relevant subset of
/// `DeviceOrchestrator.WriteInstanceConfig`.
///
/// Public so the Xbox360/XUSB path (and the validation probe) can stamp the config
/// the companion driver reads — without it, a stale config left at the same index
/// makes the companion think it's a plain-HID pad and it never brings up XUSB.
pub fn write_instance_config(
    profile: &Profile,
    controller_index: u32,
    device_id: &str,
    function_mode: u32,
) {
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
    // FlexInput's own ownership tag: which app-side device id owns this index, so
    // we can reclaim the right device across runs (and which index is free).
    let _ = write_string(HKLM, &path, "FlexInputDeviceId", device_id);
    let _ = write_dword(HKLM, &path, "FunctionMode", function_mode);
    let _ = write_binary(HKLM, &path, "ReportDescriptor", &profile.descriptor);
    let _ = write_dword(HKLM, &path, "VendorId", profile.vid as u32);
    let _ = write_dword(HKLM, &path, "ProductId", profile.pid as u32);
    let _ = write_dword(HKLM, &path, "VersionNumber", profile.version_number);
    if let Some(ps) = profile.product_string.as_deref() {
        // Clean product string ("Wireless Controller") so games see a faithful
        // DualSense/DS4. We do NOT mark it: the input enumerator distinguishes our
        // own emulated pad from a real same-VID/PID one by HID *instance path*
        // (root-enumerated `HID\HIDCLASS\..` vs `HID\VID_..`), not by any string —
        // gilrs's WGI backend reports a generic name and nil uuid for both, and
        // even the product string comes back as the generic class name, so no
        // string marker reaches it. See `gyro::is_own_virtual_instance`.
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

/// Write the XUSB companion's input-pump period (ms) to
/// `HKLM\SOFTWARE\HIDMaestro\Controller{index}` `PollIntervalMs`. The companion
/// driver reads this at `CompanionDeviceAdd` and re-reads it periodically, so it
/// pumps XInput at the app's configured polling rate. `interval_ms` is clamped to
/// 1..8 (1000..125 Hz); values outside make no sense (the WDF timer is whole-ms
/// and >125Hz..1000Hz is the supported band). Requires elevation (called from the
/// helper). Best-effort: a failed write just leaves the driver on its default.
pub fn write_poll_interval(index: u32, interval_ms: u32) {
    use registry::*;
    let ms = interval_ms.clamp(1, 8);
    let path = format!(r"SOFTWARE\HIDMaestro\Controller{index}");
    let _ = write_dword(HKLM, &path, "PollIntervalMs", ms);
}

/// One HIDMaestro-owned device discovered in the registry.
#[derive(Debug, Clone)]
pub struct ExistingDevice {
    pub instance_id: String,
    pub index: u32,
    pub vid: u16,
    pub pid: u16,
    /// FlexInput device id that owns this controller (empty if unknown).
    pub device_id: String,
    /// True if this is the XUSB/XInput companion node (System class, under
    /// `ROOT\System`) rather than the HID gamepad node (`ROOT\HIDClass`). A single
    /// Xbox360 device yields TWO entries with the same `index`: the HID node
    /// (`false`) and the companion (`true`). The SHM/reclaim path uses the HID
    /// node; teardown removes both.
    pub is_companion: bool,
}

/// Enumerate HIDMaestro-owned device nodes currently present, across BOTH the HID
/// gamepad enumerator (`ROOT\HIDClass`) and the XUSB companion enumerator
/// (`ROOT\System`). Used for reclaim-on-startup (persistence on) and orphan
/// cleanup (persistence off). A node counts if present *and* HIDMaestro-owned
/// (HardwareID multi-sz carries the tag — `root\HIDMaestroXUSB` satisfies the
/// companion too).
pub fn list_hidmaestro_devices() -> Vec<ExistingDevice> {
    // Scan EVERY ROOT\* enumerator subkey, not just HIDClass/System. Our plain-HID
    // gamepad node actually enumerates under `ROOT\VID_<vid>&PID_<pid>&IG_00`
    // (e.g. `ROOT\VID_045E&PID_02FF&IG_00`), NOT `ROOT\HIDClass` — so the old
    // two-enumerator scan never found an ORPHANED HID node left by a force-killed
    // helper / crash, and startup cleanup leaked it. The `node_is_hidmaestro_owned`
    // gate (HardwareID must carry "HIDMaestro") makes scanning all ROOT subkeys
    // safe: it can never match a physical device. (Normal teardown removes the HID
    // node by its tracked instance_id, so this path is purely orphan recovery.)
    let mut out = scan_all_root_enumerators();
    // The XUSB companions live under SWD\HIDMAESTRO (created via SwDeviceCreate),
    // NOT ROOT\System. Scanning this subtree is what lets cleanup find + remove
    // orphaned companion shells from prior runs (live ones are torn down by dropping
    // their owning SwdHandle; orphans with no live handle are removed via the
    // pnputil path that remove_device_node routes SWD ids to).
    out.extend(scan_swd_companions());
    out
}

/// Scan every `ROOT\*` enumerator subkey for present-or-phantom, HIDMaestro-owned
/// nodes. Generalizes the old hardcoded HIDClass/System scan so an orphaned HID
/// gamepad node under `ROOT\VID_..&PID_..&IG_00` is found and cleaned up.
fn scan_all_root_enumerators() -> Vec<ExistingDevice> {
    use registry::*;
    let mut out = Vec::new();
    for enumerator in enum_subkeys(HKLM, r"SYSTEM\CurrentControlSet\Enum\ROOT").unwrap_or_default() {
        out.extend(scan_enumerator(&enumerator));
    }
    out
}

/// Scan `SWD\HIDMAESTRO` for present, HIDMaestro-owned companion nodes (the XUSB
/// companions). Mirrors [`scan_enumerator`] but for the SWD enumerator path.
fn scan_swd_companions() -> Vec<ExistingDevice> {
    use registry::*;
    let base = r"SYSTEM\CurrentControlSet\Enum\SWD\HIDMAESTRO";
    let mut out = Vec::new();
    for inst in enum_subkeys(HKLM, base).unwrap_or_default() {
        let instance_id = format!(r"SWD\HIDMAESTRO\{inst}");
        // Include phantom/failed nodes too (CM_LOCATE either normal or phantom):
        // orphaned FAILEDINSTALL shells may not locate "normal" but still need
        // tearing down via the pnputil remove path.
        let present = unsafe {
            CM_Locate_DevNodeW(&mut 0u32, to_wide(&instance_id).as_ptr(), CM_LOCATE_DEVNODE_NORMAL)
                == CR_SUCCESS
                || CM_Locate_DevNodeW(
                    &mut 0u32,
                    to_wide(&instance_id).as_ptr(),
                    CM_LOCATE_DEVNODE_PHANTOM,
                ) == CR_SUCCESS
        };
        if !present {
            continue;
        }
        if !node_is_hidmaestro_owned(&instance_id) {
            continue;
        }
        let dp = format!(r"{base}\{inst}\Device Parameters");
        let index = read_dword(HKLM, &dp, "ControllerIndex").unwrap_or(u32::MAX);
        let cfg = format!(r"SOFTWARE\HIDMaestro\Controller{index}");
        let vid = read_dword(HKLM, &cfg, "VendorId").unwrap_or(0) as u16;
        let pid = read_dword(HKLM, &cfg, "ProductId").unwrap_or(0) as u16;
        let device_id = read_string(HKLM, &cfg, "FlexInputDeviceId").unwrap_or_default();
        out.push(ExistingDevice {
            instance_id,
            index,
            vid,
            pid,
            device_id,
            is_companion: true,
        });
    }
    out
}

/// Scan one `ROOT\{enumerator}` subtree for present, HIDMaestro-owned nodes.
fn scan_enumerator(enumerator: &str) -> Vec<ExistingDevice> {
    use registry::*;
    let is_companion = enumerator == "System";
    let base = format!(r"SYSTEM\CurrentControlSet\Enum\ROOT\{enumerator}");
    let mut out = Vec::new();
    for inst in enum_subkeys(HKLM, &base).unwrap_or_default() {
        let instance_id = format!(r"ROOT\{enumerator}\{inst}");
        // Present (normal) OR phantom — an orphaned node whose creating helper
        // died may be phantom, and we still want to tear it down.
        let located = unsafe {
            CM_Locate_DevNodeW(&mut 0u32, to_wide(&instance_id).as_ptr(), CM_LOCATE_DEVNODE_NORMAL)
                == CR_SUCCESS
                || CM_Locate_DevNodeW(
                    &mut 0u32,
                    to_wide(&instance_id).as_ptr(),
                    CM_LOCATE_DEVNODE_PHANTOM,
                ) == CR_SUCCESS
        };
        if !located {
            continue;
        }
        if !node_is_hidmaestro_owned(&instance_id) {
            continue;
        }
        let dp = format!(r"{base}\{inst}\Device Parameters");
        let index = read_dword(HKLM, &dp, "ControllerIndex").unwrap_or(u32::MAX);
        let cfg = format!(r"SOFTWARE\HIDMaestro\Controller{index}");
        let vid = read_dword(HKLM, &cfg, "VendorId").unwrap_or(0) as u16;
        let pid = read_dword(HKLM, &cfg, "ProductId").unwrap_or(0) as u16;
        let device_id = read_string(HKLM, &cfg, "FlexInputDeviceId").unwrap_or_default();
        out.push(ExistingDevice { instance_id, index, vid, pid, device_id, is_companion });
    }
    out
}

/// Remove every HIDMaestro-owned device node currently present. Returns the
/// number of nodes removed. Used for orphan cleanup when persistence is off
/// (on startup, and on parent-death teardown). Best-effort: a failure on one
/// node doesn't stop the others.
pub fn remove_all_hidmaestro_devices() -> usize {
    let mut removed = 0;
    let devices = list_hidmaestro_devices();

    // SWD companion orphans are removed by an out-of-process `pnputil
    // /remove-device` each — a ~100-300ms subprocess. Kick them ALL off
    // concurrently up front (independent processes; no shared state) so their
    // latency overlaps the in-proc ROOT work below instead of serializing.
    let swd_children: Vec<_> = devices
        .iter()
        .filter(|d| d.instance_id.to_ascii_uppercase().starts_with("SWD\\"))
        .filter_map(|d| spawn_swd_removal(&d.instance_id).map(|c| (d.instance_id.clone(), c)))
        .collect();

    // Build the ALLCLASSES info set ONCE for the whole pass. Every ROOT-node and
    // orphan-child removal reuses it via `*_in`, so a teardown of N nodes (each
    // with a HID child) plus orphan sweeps performs ONE system-wide device
    // enumeration instead of one per removal — the bulk of the old exit hang.
    let set = unsafe { open_allclasses_set() };
    for dev in devices.iter().filter(|d| !d.instance_id.to_ascii_uppercase().starts_with("SWD\\")) {
        let r = match &set {
            Ok(g) => remove_device_node_in(g.0, &dev.instance_id),
            Err(_) => remove_device_node(&dev.instance_id),
        };
        if matches!(r, Ok(true)) {
            removed += 1;
        }
    }
    // Sweep up any HID children whose parent is already gone (orphans from a
    // prior build that didn't remove children, or a force-kill). Counts toward
    // the total so callers see real cleanup progress. Reuses the same set.
    removed += match &set {
        Ok(g) => remove_orphan_hid_children_in(g.0),
        Err(_) => remove_orphan_hid_children(),
    };

    // Now join the pnputil children we launched earlier — by now most/all have
    // finished while we did the ROOT work. Count one removed per node that's
    // actually gone afterwards.
    for (instance_id, mut child) in swd_children {
        let _ = child.wait();
        let gone = unsafe {
            CM_Locate_DevNodeW(&mut 0u32, to_wide(&instance_id).as_ptr(), CM_LOCATE_DEVNODE_NORMAL)
                != CR_SUCCESS
        };
        if gone {
            removed += 1;
        }
    }
    removed
}

/// Sweep leftover HIDMaestro nodes, then BLOCK until none remain (or `timeout`
/// elapses), retrying the sweep each round. Returns `true` if the system is
/// verified clear, `false` if nodes persisted past the timeout.
///
/// This is the startup gate that makes an abrupt prior exit (force-kill, crash,
/// GPU-loss relaunch) safe: a too-soon relaunch can spawn THIS helper while the
/// previous helper is still mid-teardown (its `remove_all_hidmaestro_devices`
/// runs for several seconds after parent death). If the app's first `Create`
/// raced that, it built on a half-removed node and orphaned it (the "virtual
/// failed to redeploy" / stuck-representation symptom). By sweeping-and-waiting
/// here before we report startup-ready, the app only ever deploys onto a
/// confirmed-clean system. Removal is idempotent, so racing the old helper's
/// own sweep is harmless — whichever removes a node first, both then observe it
/// gone. A node we don't manage to clear within the timeout is left for the
/// per-`Create` reclaim path rather than blocking startup forever.
pub fn clear_hidmaestro_devices_and_wait(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let n = remove_all_hidmaestro_devices();
        // Re-enumerate AFTER removing: removal/CM teardown can lag, and the old
        // helper may still be releasing handles, so a node can linger one round.
        let remaining = list_hidmaestro_devices().len();
        if remaining == 0 {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        // Short backoff; CM device removal settles on the order of ~100s of ms.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let _ = n;
    }
}

/// Scan `HKLM\...\Enum\HID` for orphaned HIDMaestro HID children — those whose
/// `HardwareID` carries the "HIDMaestro" ownership tag but whose parent devnode
/// no longer exists — and `DIF_REMOVE` each. Returns the count removed.
///
/// Port of `DeviceManager.RemoveOrphanHidChildren`. This is what reclaims the
/// `HID\HIDCLASS\...` "game controller" ghosts that accumulated one-per-run
/// before [`remove_device_node`] learned to remove children. The "HIDMaestro"
/// tag in the **child's own** HardwareID is the ownership proof — it survives
/// even after the parent is gone — so this never touches a real generic pad.
pub fn remove_orphan_hid_children() -> usize {
    match unsafe { open_allclasses_set() } {
        Ok(g) => remove_orphan_hid_children_in(g.0),
        // Set failed to open — fall back to per-removal one-off sets inside the
        // sweep (dif_remove builds its own).
        Err(_) => remove_orphan_hid_children_with(None),
    }
}

/// As [`remove_orphan_hid_children`], reusing a caller-provided ALLCLASSES set.
fn remove_orphan_hid_children_in(dis: *mut c_void) -> usize {
    remove_orphan_hid_children_with(Some(dis))
}

/// Shared body: when `dis` is `Some`, each orphan removal reuses that set via
/// `dif_remove_in`; when `None`, falls back to a one-off set per removal.
fn remove_orphan_hid_children_with(dis: Option<*mut c_void>) -> usize {
    use registry::*;
    let mut removed = 0;
    let base = r"SYSTEM\CurrentControlSet\Enum\HID";
    let Some(devices) = enum_subkeys(HKLM, base) else {
        return 0;
    };
    for device_name in devices {
        let dev_path = format!(r"{base}\{device_name}");
        for instance_name in enum_subkeys(HKLM, &dev_path).unwrap_or_default() {
            let inst_path = format!(r"{dev_path}\{instance_name}");
            // Ownership: HardwareID multi-sz contains "HIDMaestro".
            let owned = match read_multi_sz(HKLM, &inst_path, "HardwareID") {
                Some(ids) => ids.iter().any(|s| s.contains("HIDMaestro")),
                None => false,
            };
            if !owned {
                continue;
            }
            let hid_instance_id = format!(r"HID\{device_name}\{instance_name}");
            // Is the parent gone? Locate the child devnode, get its parent, and
            // check the parent isn't locatable (normal or phantom).
            let orphaned = unsafe {
                let w = to_wide(&hid_instance_id);
                let mut child_inst: u32 = 0;
                let located = CM_Locate_DevNodeW(&mut child_inst, w.as_ptr(), CM_LOCATE_DEVNODE_NORMAL)
                    == CR_SUCCESS
                    || CM_Locate_DevNodeW(&mut child_inst, w.as_ptr(), CM_LOCATE_DEVNODE_PHANTOM)
                        == CR_SUCCESS;
                if !located {
                    // Child devnode itself is gone (registry residue only) —
                    // treat as orphan to clear the stale key.
                    true
                } else {
                    let mut parent_inst: u32 = 0;
                    if CM_Get_Parent(&mut parent_inst, child_inst, 0) != CR_SUCCESS {
                        true // no parent at all → orphan
                    } else {
                        // Parent exists in the tree; only orphaned if it can't
                        // be located by id. Get its id, then locate.
                        match devnode_instance_id(parent_inst) {
                            Some(pid) => {
                                let wp = to_wide(&pid);
                                CM_Locate_DevNodeW(&mut 0u32, wp.as_ptr(), CM_LOCATE_DEVNODE_NORMAL)
                                    != CR_SUCCESS
                                    && CM_Locate_DevNodeW(
                                        &mut 0u32,
                                        wp.as_ptr(),
                                        CM_LOCATE_DEVNODE_PHANTOM,
                                    ) != CR_SUCCESS
                            }
                            None => true,
                        }
                    }
                }
            };
            if orphaned {
                let r = match dis {
                    Some(d) => unsafe { dif_remove_in(d, &hid_instance_id) },
                    None => unsafe { dif_remove(&hid_instance_id) },
                };
                if let Ok(true) = r {
                    removed += 1;
                }
            }
        }
    }
    removed
}

/// Remove a plain-HID device node previously created here. Port of the non-SWD
/// branch of `DeviceManager.RemoveDevice`.
///
/// **Children first, then parent.** `DIF_REMOVE` on the root parent does NOT
/// cascade-remove its HID child PDO — the child survives as an orphaned
/// `HID\HIDCLASS\...` "HID-compliant game controller" bound to the generic
/// `input.inf`, with no VID/PID and no HIDMaestro ownership tag, so it can't be
/// found or cleaned up later and accumulates one-per-run (the device-leak bug).
/// We enumerate the children (`CM_Get_Child`/`CM_Get_Sibling`) and `DIF_REMOVE`
/// each before removing the parent — exactly as HIDMaestro's C# `RemoveDevice`
/// does ("prevents ghost HID children from surviving"). Returns true if the
/// parent node is gone (or was never present). Requires elevation.
pub fn remove_device_node(instance_id: &str) -> Result<bool, OrchestratorError> {
    // SWD companions go through pnputil (no info set needed); ROOT nodes build a
    // one-off set. Bulk paths use `remove_device_node_in` to share one set.
    if instance_id.to_ascii_uppercase().starts_with("SWD\\") {
        return remove_swd_node(instance_id);
    }
    let guard = unsafe { open_allclasses_set()? };
    unsafe { remove_root_node_in(guard.0, instance_id) }
}

/// Remove a HIDMaestro node, reusing a caller-provided ALLCLASSES info set for
/// the ROOT (plain-HID) path so a bulk teardown pays the system enumeration once.
/// SWD companions ignore `dis` (they use the pnputil path).
fn remove_device_node_in(dis: *mut c_void, instance_id: &str) -> Result<bool, OrchestratorError> {
    if instance_id.to_ascii_uppercase().starts_with("SWD\\") {
        return remove_swd_node(instance_id);
    }
    unsafe { remove_root_node_in(dis, instance_id) }
}

/// SWD\ companions WE create are owned by a held HSWDEVICE handle (default
/// `Handle` lifetime) — the helper removes them by DROPPING that handle, not
/// through this function. This path only runs for ORPHAN SWD nodes with no live
/// handle (leftover shells from earlier code revs). On Win10 19045 the
/// SwDeviceCreate-reconnect teardown is a cosmetic no-op (proven), so the best
/// available cleanup for an orphan is pnputil /remove-device /force + /subtree.
/// Best-effort: if it doesn't take (ParentPresent shells can survive until
/// reboot) we still report progress so callers move on.
fn remove_swd_node(instance_id: &str) -> Result<bool, OrchestratorError> {
    // Single-node path: spawn + wait. Bulk teardown uses `spawn_swd_removal`
    // directly to run all companions' pnputil concurrently.
    if let Some(mut child) = spawn_swd_removal(instance_id) {
        let _ = child.wait();
    }
    // Report removed only if it's actually gone now.
    let gone = unsafe {
        CM_Locate_DevNodeW(
            &mut 0u32,
            to_wide(instance_id).as_ptr(),
            CM_LOCATE_DEVNODE_NORMAL,
        ) != CR_SUCCESS
    };
    Ok(gone)
}

/// Launch `pnputil /remove-device <id> /subtree /force` WITHOUT waiting, so a
/// bulk teardown can run every companion's removal concurrently and join later.
/// Returns the spawned child (None if pnputil couldn't be launched at all).
fn spawn_swd_removal(instance_id: &str) -> Option<std::process::Child> {
    let pnputil = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("pnputil.exe");
    std::process::Command::new(&pnputil)
        .arg("/remove-device")
        .arg(instance_id)
        .arg("/subtree")
        .arg("/force")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
}

/// Children-first, then-parent `DIF_REMOVE` for a ROOT (plain-HID) node, all
/// against the shared `dis` info set. See [`remove_device_node`] for why the HID
/// child PDOs must be removed explicitly (they don't cascade).
unsafe fn remove_root_node_in(dis: *mut c_void, instance_id: &str) -> Result<bool, OrchestratorError> {
    let w_id = to_wide(instance_id);
    // Locate the parent (normal or phantom). Already gone → nothing to do,
    // but we still don't know its children, so just return.
    let mut parent_inst: u32 = 0;
    let present = CM_Locate_DevNodeW(&mut parent_inst, w_id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL)
        == CR_SUCCESS
        || CM_Locate_DevNodeW(&mut parent_inst, w_id.as_ptr(), CM_LOCATE_DEVNODE_PHANTOM)
            == CR_SUCCESS;
    if !present {
        return Ok(true);
    }

    // Step 1: remove every HID child PDO first (these don't cascade).
    for child_id in child_device_ids(parent_inst) {
        let _ = dif_remove_in(dis, &child_id);
    }

    // Step 2: remove the parent.
    dif_remove_in(dis, instance_id)
}

/// Build an empty `DIGCF_ALLCLASSES` device-info set once. This enumerates every
/// device in the system, which is the single most expensive step in teardown
/// (hundreds of ms on a populated machine) — so a bulk removal pass builds ONE
/// set and reuses it across all `dif_remove_in` calls instead of paying this per
/// node + per child. Returns a guard that destroys the set on drop.
unsafe fn open_allclasses_set() -> Result<DisGuard, OrchestratorError> {
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
    Ok(DisGuard(dis))
}

/// `DIF_REMOVE` a single device by instance id using a CALLER-PROVIDED info set.
/// `SetupDiOpenDeviceInfoW` adds the node to the given set by id (the set need
/// not already contain it), so one shared ALLCLASSES set serves an entire
/// teardown pass — avoiding a fresh system-wide enumeration per removal.
unsafe fn dif_remove_in(dis: *mut c_void, instance_id: &str) -> Result<bool, OrchestratorError> {
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

/// `DIF_REMOVE` a single device by instance id via a fresh ALLCLASSES info set.
/// One-off wrapper around [`dif_remove_in`] for standalone callers; bulk paths
/// build one set and call `dif_remove_in` directly. Port of
/// `DeviceManager.DifRemoveDevice`.
unsafe fn dif_remove(instance_id: &str) -> Result<bool, OrchestratorError> {
    let guard = open_allclasses_set()?;
    dif_remove_in(guard.0, instance_id)
}

/// A teardown session that holds ONE `DIGCF_ALLCLASSES` info set for its lifetime
/// so every [`RemovalBatch::remove`] reuses it instead of re-enumerating every
/// device in the system per call. Use this when removing several nodes (e.g. the
/// helper's exit teardown of its tracked devices) — it turns N system-wide
/// enumerations into one, which is the bulk of the old exit hang.
///
/// If the set can't be opened, `remove` transparently falls back to a one-off set
/// per call (i.e. behaves like [`remove_device_node`]), so callers never need to
/// special-case the failure.
pub struct RemovalBatch {
    set: Option<DisGuard>,
}

impl RemovalBatch {
    /// Open the shared info set for a teardown session. Never fails to construct;
    /// a set-open failure just degrades to per-call sets inside `remove`.
    pub fn new() -> Self {
        RemovalBatch {
            set: unsafe { open_allclasses_set().ok() },
        }
    }

    /// Remove one node (SWD companion → pnputil; ROOT plain-HID → shared set,
    /// children-first). Same semantics/return as [`remove_device_node`].
    pub fn remove(&self, instance_id: &str) -> Result<bool, OrchestratorError> {
        match &self.set {
            Some(g) => remove_device_node_in(g.0, instance_id),
            None => remove_device_node(instance_id),
        }
    }
}

impl Default for RemovalBatch {
    fn default() -> Self {
        Self::new()
    }
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

    pub fn read_string(root: *mut c_void, path: &str, name: &str) -> Option<String> {
        let h = open(root, path, KEY_READ)?;
        let wn = wide(name);
        let mut ty = 0u32;
        let mut len = 0u32;
        // Size query.
        let rc = unsafe {
            RegQueryValueExW(h, wn.as_ptr(), std::ptr::null_mut(), &mut ty, std::ptr::null_mut(), &mut len)
        };
        if (rc != ERROR_SUCCESS && rc != ERROR_MORE_DATA) || ty != REG_SZ || len == 0 {
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
        let u16s: Vec<u16> = buf[..len as usize]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&w| w != 0)
            .collect();
        Some(String::from_utf16_lossy(&u16s))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_id_layout_matches_upstream() {
        // ASCII "HIDMAESTRO" base + 16-bit controller index in the last 2 bytes.
        // Verbatim from SwdDeviceFactory.ContainerIdFor — a wrong nibble here
        // silently re-breaks the xinput slot-0-skip fix.
        assert_eq!(
            container_id_for(0),
            "{48494430-4D41-4553-5452-4F0000000000}"
        );
        assert_eq!(
            container_id_for(1),
            "{48494430-4D41-4553-5452-4F0000000001}"
        );
        assert_eq!(
            container_id_for(0x0102),
            "{48494430-4D41-4553-5452-4F0000000102}"
        );
        assert_eq!(
            container_id_for(0xFFFF),
            "{48494430-4D41-4553-5452-4F000000FFFF}"
        );
    }

    #[test]
    fn container_id_is_per_controller() {
        // Distinct indices → distinct containers (so multi-controller setups
        // don't collapse into one PnP container).
        assert_ne!(container_id_for(0), container_id_for(1));
        assert_ne!(container_id_for(2), container_id_for(3));
    }

    #[test]
    fn swd_suffix_is_unique_and_well_formed() {
        // Fresh suffix every call (sticky-container fast-path dodge) and the
        // controller index is preserved in the human-readable tail.
        let a = next_swd_suffix(0);
        let b = next_swd_suffix(0);
        assert_ne!(a, b, "consecutive suffixes must differ");
        assert!(a.ends_with("_0000"), "suffix tail encodes ctrl index: {a}");
        let c = next_swd_suffix(3);
        assert!(c.ends_with("_0003"), "suffix tail encodes ctrl index: {c}");
    }

    #[test]
    fn removal_batch_removes_absent_node_as_noop() {
        // A RemovalBatch must construct (it opens a shared ALLCLASSES set, or
        // degrades gracefully if that fails) and removing a node that doesn't
        // exist is a safe no-op reporting "gone" — the same contract as
        // remove_device_node. Uses a bogus-but-well-formed ROOT id so no real
        // device is touched and no elevation is needed. This guards the shared-
        // set reuse path (the exit-hang fix) against regressions.
        let batch = RemovalBatch::new();
        let bogus = r"ROOT\HIDMAESTRO_TEST_DOES_NOT_EXIST\0000";
        assert!(
            matches!(batch.remove(bogus), Ok(true)),
            "absent ROOT node must report removed (no-op)"
        );
        // A second removal on the same batch must reuse the set and behave
        // identically — proving the set stays usable across calls.
        assert!(
            matches!(batch.remove(bogus), Ok(true)),
            "batch set must remain usable for repeated removals"
        );
    }
}
