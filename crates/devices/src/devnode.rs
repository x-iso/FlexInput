//! Device-tree lookups through CfgMgr32: which physical device a HID interface
//! or an audio endpoint belongs to, and whether a HID interface is one of
//! FlexInput's own composite USB personas.

#![cfg(windows)]

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Device_IDW, CM_Get_Device_Interface_PropertyW, CM_Get_Parent,
    CM_Locate_DevNodeW, CM_LOCATE_DEVNODE_NORMAL, CR_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_ContainerId, DEVPKEY_Device_InstanceId, DEVPROP_TYPE_GUID, DEVPROP_TYPE_STRING,
};

/// The GUID Windows gives every devnode of one physical device. A USB
/// DualSense's HID interfaces and its audio endpoints share one, so it names
/// the endpoint that belongs to the pad being driven, where the endpoint's
/// name only says some DualSense (or a virtual one) has it.
pub type ContainerId = [u8; 16];

/// `{00000000-0000-0000-FFFF-FFFFFFFFFFFF}`: what Windows gives a device it
/// can't place in a container (ROOT-enumerated ones). Says nothing about which
/// pad an endpoint belongs to, so it never counts as a match.
const NO_CONTAINER: ContainerId = [0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The device instance id behind a device interface path (hidapi's
/// `DeviceInfo::path`).
fn interface_instance_id(path: &str) -> Option<String> {
    let path = to_wide(path);
    let mut ty = 0u32;
    let mut buf = [0u16; 512];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `path` is NUL-terminated; `buf` is `size` bytes.
    let r = unsafe {
        CM_Get_Device_Interface_PropertyW(
            path.as_ptr(), &DEVPKEY_Device_InstanceId, &mut ty,
            buf.as_mut_ptr().cast(), &mut size, 0,
        )
    };
    if r != CR_SUCCESS || ty != DEVPROP_TYPE_STRING {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// The container of the device behind a HID interface path, or `None` if
/// Windows can't resolve one.
pub fn container_of_interface(path: &str) -> Option<ContainerId> {
    container_of_instance(&interface_instance_id(path)?)
}

/// The container of an audio endpoint, from its MMDevice id. Endpoints are
/// software devnodes at `SWD\MMDEVAPI\<id>`.
pub(crate) fn container_of_endpoint(endpoint_id: &str) -> Option<ContainerId> {
    container_of_instance(&format!(r"SWD\MMDEVAPI\{endpoint_id}"))
}

fn container_of_instance(instance_id: &str) -> Option<ContainerId> {
    let id = to_wide(instance_id);
    let mut inst = 0u32;
    // SAFETY: `id` is NUL-terminated.
    if unsafe { CM_Locate_DevNodeW(&mut inst, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) } != CR_SUCCESS {
        return None;
    }
    let mut container: ContainerId = [0; 16];
    let mut ty = 0u32;
    let mut size = container.len() as u32;
    // SAFETY: `container` is `size` bytes, a DEVPROP_TYPE_GUID's size.
    let r = unsafe {
        CM_Get_DevNode_PropertyW(
            inst, &DEVPKEY_Device_ContainerId, &mut ty, container.as_mut_ptr(), &mut size, 0,
        )
    };
    (r == CR_SUCCESS && ty == DEVPROP_TYPE_GUID && container != NO_CONTAINER).then_some(container)
}

/// The instance ids of `instance_id`'s first `levels` ancestors, nearest first.
fn ancestors(instance_id: &str, levels: usize) -> Vec<String> {
    let id = to_wide(instance_id);
    let mut inst = 0u32;
    // SAFETY: `id` is NUL-terminated.
    if unsafe { CM_Locate_DevNodeW(&mut inst, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) } != CR_SUCCESS {
        return Vec::new();
    }
    let mut out = Vec::new();
    for _ in 0..levels {
        let mut parent = 0u32;
        // SAFETY: FFI on a located devnode.
        if unsafe { CM_Get_Parent(&mut parent, inst, 0) } != CR_SUCCESS {
            break;
        }
        let mut buf = [0u16; 512];
        // SAFETY: `buf` holds its length in characters.
        if unsafe { CM_Get_Device_IDW(parent, buf.as_mut_ptr(), buf.len() as u32, 0) } != CR_SUCCESS {
            break;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        out.push(String::from_utf16_lossy(&buf[..len]));
        inst = parent;
    }
    out
}

/// Whether a HID interface path belongs to one of FlexInput's own composite
/// USB personas. To Windows those are real Sony pads at every level — the HID
/// path looks like a wired DualSense's — so the tell is the USB device they
/// hang off, `USB\VID_054C&PID_xxxx\<serial>`, whose serial
/// `flexinput_usbip::serial_for` made.
pub fn is_own_composite_interface(path: &str) -> bool {
    // HID interface → USB interface (MI_03) → the composite USB device.
    let Some(id) = interface_instance_id(path) else { return false };
    ancestors(&id, 3)
        .iter()
        .any(|a| usb_device_serial(a).is_some_and(flexinput_usbip::is_own_serial))
}

/// The serial segment of a USB device's instance id (`USB\VID_…&PID_…\<serial>`),
/// or `None` for anything else — a USB interface's id carries `&MI_`.
fn usb_device_serial(instance_id: &str) -> Option<&str> {
    let mut parts = instance_id.splitn(3, '\\');
    let (bus, hw, serial) = (parts.next()?, parts.next()?, parts.next()?);
    let hw = hw.to_ascii_uppercase();
    (bus.eq_ignore_ascii_case("USB") && hw.starts_with("VID_") && !hw.contains("&MI_")).then_some(serial)
}

#[cfg(test)]
mod tests {
    use super::*;

    // CfgMgr32 hands a container back as a GUID's in-memory bytes; the sentinel
    // must be compared in that layout (little-endian data1-3, then data4).
    #[test]
    fn no_container_is_the_sentinel_guid_in_memory_order() {
        let g = windows_sys::core::GUID::from_u128(0x00000000_0000_0000_FFFF_FFFFFFFFFFFF);
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&g.data1.to_le_bytes());
        bytes[4..6].copy_from_slice(&g.data2.to_le_bytes());
        bytes[6..8].copy_from_slice(&g.data3.to_le_bytes());
        bytes[8..16].copy_from_slice(&g.data4);
        assert_eq!(bytes, NO_CONTAINER);
    }

    #[test]
    fn the_serial_comes_from_a_usb_device_id_not_an_interface() {
        assert_eq!(usb_device_serial(r"USB\VID_054C&PID_0CE6\FX0123456789AB"), Some("FX0123456789AB"));
        assert_eq!(usb_device_serial(r"USB\VID_054C&PID_0CE6&MI_03\7&1234&0&0003"), None);
        assert_eq!(usb_device_serial(r"HID\VID_054C&PID_0CE6&MI_03\8&abc&0&0000"), None);
        assert_eq!(usb_device_serial(r"ROOT\USB\0000"), None);
    }

    // A real HID device present on any Windows machine is never ours.
    #[test]
    fn present_hid_devices_are_not_own_composites() {
        let Ok(api) = hidapi::HidApi::new() else { return };
        for d in api.device_list().take(16) {
            assert!(!is_own_composite_interface(&d.path().to_string_lossy()));
        }
    }
}
