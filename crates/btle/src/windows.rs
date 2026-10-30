//! Which driver each Bluetooth adapter runs on Windows, from the device tree
//! (CfgMgr32). WinUSB means FlexInput's: bound by Zadig, or handed over from
//! FlexInput's Bluetooth window (`flexinput-hidmaestro`'s `winusb_bind`).
//! Anything else is Windows' Bluetooth, which runs one radio at a time: the
//! one it uses has its stack's devices (RFCOMM, PAN, …) as children.

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Child, CM_Get_DevNode_Registry_PropertyW, CM_Get_DevNode_Status, CM_Get_Device_ID_ListW,
    CM_Get_Device_ID_List_SizeW, CM_Get_Parent, CM_Locate_DevNodeW, CM_DEVCAP_REMOVABLE, CM_DRP_CAPABILITIES,
    CM_DRP_COMPATIBLEIDS, CM_DRP_DEVICEDESC, CM_DRP_FRIENDLYNAME, CM_DRP_SERVICE,
    CM_GETIDLIST_FILTER_ENUMERATOR, CM_GETIDLIST_FILTER_PRESENT, CM_LOCATE_DEVNODE_NORMAL, CR_SUCCESS, DN_STARTED,
};

/// The compatible id a USB Bluetooth adapter's function carries, from its
/// class codes: wireless controller, RF, Bluetooth.
const BLUETOOTH_CLASS: &str = r"USB\Class_E0&SubClass_01&Prot_01";

/// An adapter's Bluetooth function in the device tree.
#[derive(Debug, Clone)]
pub struct Node {
    /// Its device instance id, which handing it over or back names.
    pub instance_id: String,
    /// The driver's service (`BTHUSB`, a vendor's, `WinUSB`), if one is bound.
    pub service: Option<String>,
    /// Windows' Bluetooth runs on it: started, with its stack's devices under it.
    pub active: bool,
    /// Built in: the USB device isn't removable.
    pub built_in: bool,
    /// Windows' name for it (its friendly name or device description), for
    /// one Windows owns, whose USB strings can't be read without opening it.
    pub description: Option<String>,
}

impl Node {
    /// Whether it runs WinUSB, that is, it's FlexInput's.
    pub fn is_winusb(&self) -> bool {
        self.service.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("WinUSB"))
    }
}

/// Every USB Bluetooth adapter Windows has, as `(vid, pid, its Bluetooth
/// function)`: the device itself, or interface 0 of a composite one
/// (Bluetooth's HCI interface by spec). Found by the class every adapter
/// declares ([`BLUETOOTH_CLASS`]), whatever driver it runs — Windows', WinUSB,
/// one that failed to start, or none.
///
/// ⛔ Not from libusb's device list. An adapter whose driver failed to start —
/// a second radio, beside the one Windows runs — may never have been
/// configured, libusb then can't read its interfaces, and it dropped out of
/// the list: nothing to hand over, after giving it back to Windows.
pub fn adapters() -> Vec<(u16, u16, Node)> {
    let ids = present_usb_ids();
    ids.iter()
        .filter_map(|id| {
            let up = id.to_ascii_uppercase();
            // A composite device's other interfaces (MI_01: the SCO audio) carry
            // the class too; its HCI interface is MI_00.
            if up.contains("&MI_") && !up.contains("&MI_00") {
                return None;
            }
            let dev = locate(id)?;
            if !multi_string_property(dev, CM_DRP_COMPATIBLEIDS).iter().any(|c| c.eq_ignore_ascii_case(BLUETOOTH_CLASS)) {
                return None;
            }
            // A composite parent stands aside for its MI_00.
            if service(dev).is_some_and(|s| s.eq_ignore_ascii_case("usbccgp")) {
                return None;
            }
            let (vid, pid) = vid_pid(&up)?;
            // Built in or not is the USB device's, a composite interface's parent.
            let whole = if up.contains("&MI_") { parent(dev)? } else { dev };
            let built_in = !capabilities(whole).is_some_and(|c| c & CM_DEVCAP_REMOVABLE != 0);
            let description = string_property(dev, CM_DRP_FRIENDLYNAME).or_else(|| string_property(dev, CM_DRP_DEVICEDESC));
            let node = Node {
                instance_id: id.clone(),
                service: service(dev),
                active: started(dev) && has_child(dev),
                built_in,
                description,
            };
            Some((vid, pid, node))
        })
        .collect()
}

/// `VID_xxxx&PID_yyyy` out of an upper-cased USB instance id.
fn vid_pid(up: &str) -> Option<(u16, u16)> {
    let rest = up.strip_prefix("USB\\VID_")?;
    let vid = u16::from_str_radix(rest.get(..4)?, 16).ok()?;
    let pid = u16::from_str_radix(rest.get(4..)?.strip_prefix("&PID_")?.get(..4)?, 16).ok()?;
    Some((vid, pid))
}

fn parent(dev: u32) -> Option<u32> {
    let mut p = 0u32;
    // SAFETY: FFI into an out-param.
    (unsafe { CM_Get_Parent(&mut p, dev, 0) } == CR_SUCCESS).then_some(p)
}

/// Every present device the USB bus enumerates.
fn present_usb_ids() -> Vec<String> {
    let filter = wide("USB");
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    let mut len = 0u32;
    // SAFETY: FFI; `len` receives the list size in characters.
    if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, filter.as_ptr(), flags) } != CR_SUCCESS || len == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: `buf` holds `len` characters.
    if unsafe { CM_Get_Device_ID_ListW(filter.as_ptr(), buf.as_mut_ptr(), len, flags) } != CR_SUCCESS {
        return Vec::new();
    }
    buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

fn locate(instance_id: &str) -> Option<u32> {
    let id = wide(instance_id);
    let mut dev = 0u32;
    // SAFETY: FFI with a NUL-terminated id.
    (unsafe { CM_Locate_DevNodeW(&mut dev, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS).then_some(dev)
}

fn service(dev: u32) -> Option<String> {
    string_property(dev, CM_DRP_SERVICE)
}

fn string_property(dev: u32, property: u32) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut len = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `buf` holds `len` bytes.
    let cr = unsafe {
        CM_Get_DevNode_Registry_PropertyW(dev, property, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len, 0)
    };
    if cr != CR_SUCCESS {
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..(len as usize / 2).min(buf.len())]);
    let s = s.trim_end_matches('\0').trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// A `REG_MULTI_SZ` property, as its strings.
fn multi_string_property(dev: u32, property: u32) -> Vec<String> {
    let mut buf = [0u16; 2048];
    let mut len = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `buf` holds `len` bytes.
    let cr = unsafe {
        CM_Get_DevNode_Registry_PropertyW(dev, property, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len, 0)
    };
    if cr != CR_SUCCESS {
        return Vec::new();
    }
    buf[..(len as usize / 2).min(buf.len())]
        .split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

fn capabilities(dev: u32) -> Option<u32> {
    let mut caps = 0u32;
    let mut len = std::mem::size_of::<u32>() as u32;
    // SAFETY: `caps` is `len` bytes.
    let cr = unsafe {
        CM_Get_DevNode_Registry_PropertyW(dev, CM_DRP_CAPABILITIES, std::ptr::null_mut(), (&mut caps as *mut u32).cast(), &mut len, 0)
    };
    (cr == CR_SUCCESS).then_some(caps)
}

fn started(dev: u32) -> bool {
    let (mut status, mut problem) = (0u32, 0u32);
    // SAFETY: FFI into two out-params.
    let cr = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, dev, 0) };
    cr == CR_SUCCESS && status & DN_STARTED != 0 && problem == 0
}

fn has_child(dev: u32) -> bool {
    let mut child = 0u32;
    // SAFETY: FFI into an out-param.
    unsafe { CM_Get_Child(&mut child, dev, 0) == CR_SUCCESS }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_come_out_of_instance_ids() {
        assert_eq!(vid_pid(r"USB\VID_0BDA&PID_A728\00E04C239987"), Some((0x0BDA, 0xA728)));
        assert_eq!(vid_pid(r"USB\VID_13D3&PID_3608&MI_00\7&D0B2222&0&0000"), Some((0x13D3, 0x3608)));
        assert_eq!(vid_pid(r"BTHENUM\{0000}"), None);
    }
}
