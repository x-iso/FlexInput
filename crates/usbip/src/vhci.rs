//! usbip-win2's virtual host controller, through its device-interface ioctls
//! (`include/usbip/vhci.h`).
//!
//! The request layouts aren't stable across releases, so this speaks the
//! three HIDMaestro measured and asks the driver which one it has: 0.9.8.0
//! appended a serial and a flag to the attach request and to each imported
//! row; 0.9.8.1 put a `location_hash` after `port` in the location, moving
//! busid, service and host 4 bytes on. The driver checks every request's size
//! field against its own `sizeof` and refuses a mismatch before acting, so a
//! probe with the wrong size is harmless.
//!
//! The controller's device object grants everyone read/write
//! (`SDDL_DEVOBJ_SYS_ALL_ADM_RWX_WORLD_RW_RES_R` in `drivers/ude/vhci.cpp`), so
//! no elevation is needed to attach or detach.

use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW,
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CR_SUCCESS,
};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
use windows_sys::Win32::System::IO::DeviceIoControl;

/// `GUID_DEVINTERFACE_USB_HOST_CONTROLLER` (`GUID_DEVINTERFACE_USBIP_VHCI`
/// since 0.9.8.1, same value).
const VHCI_INTERFACE: GUID = GUID::from_u128(0xB4030C06_DC5F_4FCC_87EB_E5515A0935C0);

// CTL_CODE(FILE_DEVICE_UNKNOWN, fn, METHOD_BUFFERED, FILE_READ_DATA | FILE_WRITE_DATA).
const PLUGOUT_HARDWARE: u32 = 0x0022_E004; // fn 0x801
const GET_IMPORTED_DEVICES: u32 = 0x0022_E008; // fn 0x802
const STOP_ATTACH_ATTEMPTS: u32 = 0x0022_E014; // fn 0x805
/// One attempt, no background retry: this app owns the server's lifecycle, so
/// a failed attach should fail, not become the driver's persistent device.
const PLUGIN_HARDWARE_ONCE: u32 = 0x0022_E018; // fn 0x806

const BUS_ID_SIZE: usize = 32;
const SERVICE_SIZE: usize = 32; // NI_MAXSERV
const HOST_SIZE: usize = 1025; // NI_MAXHOST
/// Every request opens with a ULONG size; the location after it opens with the
/// port, so an attach's returned port and each row's port sit at offset 4.
const HEADER_SIZE: usize = 4;
const PLUGOUT_SIZE: usize = 8;
/// MAX_TOTAL_PORTS. A listing buffer this size can't fail for being small.
const MAX_PORTS: usize = 255;

/// One release family's request layout: sizeof plugin_hardware,
/// stop_attach_attempts and imported_device, and where busid starts inside
/// the location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub name: &'static str,
    plugin_size: usize,
    stop_size: usize,
    row_size: usize,
    busid_offset: usize,
}

/// Newest first, the order the probe tries them.
const LAYOUTS: [Layout; 3] = [
    Layout { name: "0.9.8.1", plugin_size: 1124, stop_size: 1108, row_size: 1132, busid_offset: 8 },
    Layout { name: "0.9.8.0", plugin_size: 1120, stop_size: 1104, row_size: 1128, busid_offset: 4 },
    Layout { name: "0.9.7.x", plugin_size: 1100, stop_size: 1104, row_size: 1108, busid_offset: 4 },
];

#[derive(Debug)]
pub enum VhciError {
    /// No usbip-win2 host controller is present.
    Absent,
    /// The controller accepted none of the known request layouts.
    UnknownLayout(u32),
    Win32(&'static str, u32),
}

impl std::fmt::Display for VhciError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VhciError::Absent => write!(f, "the usbip-win2 virtual host controller isn't installed"),
            VhciError::UnknownLayout(e) => write!(
                f,
                "the usbip-win2 host controller accepted none of the request layouts this build knows (0.9.8.1, 0.9.8.0, 0.9.7.x); last error {e:#010x}"
            ),
            VhciError::Win32(what, e) => write!(f, "{what} failed: {e:#010x}"),
        }
    }
}

impl std::error::Error for VhciError {}

/// One device the controller has imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub port: i32,
    pub busid: String,
    pub service: String,
    pub host: String,
}

/// The controller's interface path, or `None` when usbip-win2 isn't present
/// and running. Needs no elevation.
pub fn interface_path() -> Option<Vec<u16>> {
    let mut len = 0u32;
    // SAFETY: FFI; `len` receives the list size in characters.
    let cr = unsafe {
        CM_Get_Device_Interface_List_SizeW(&mut len, &VHCI_INTERFACE, std::ptr::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT)
    };
    if cr != CR_SUCCESS || len <= 1 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: `buf` holds `len` characters.
    let cr = unsafe {
        CM_Get_Device_Interface_ListW(&VHCI_INTERFACE, std::ptr::null(), buf.as_mut_ptr(), len, CM_GET_DEVICE_INTERFACE_LIST_PRESENT)
    };
    if cr != CR_SUCCESS {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0)?;
    if end == 0 {
        return None;
    }
    buf.truncate(end);
    buf.push(0);
    Some(buf)
}

/// Whether usbip-win2's host controller is present and running.
pub fn is_available() -> bool {
    interface_path().is_some()
}

struct Controller(HANDLE);

impl Drop for Controller {
    fn drop(&mut self) {
        // SAFETY: our own handle.
        unsafe { CloseHandle(self.0) };
    }
}

impl Controller {
    fn open() -> Result<Self, VhciError> {
        let path = interface_path().ok_or(VhciError::Absent)?;
        // SAFETY: `path` is NUL-terminated.
        let h = unsafe {
            CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut())
        };
        if h == INVALID_HANDLE_VALUE {
            // SAFETY: FFI.
            return Err(VhciError::Win32("CreateFile(vhci)", unsafe { GetLastError() }));
        }
        Ok(Controller(h))
    }

    /// One buffered ioctl, `buf` both in and out. The bytes returned, or the
    /// Win32 error.
    fn ioctl(&self, code: u32, buf: &mut [u8], out: bool) -> Result<u32, u32> {
        let mut returned = 0u32;
        let (out_ptr, out_len) = if out { (buf.as_mut_ptr(), buf.len() as u32) } else { (std::ptr::null_mut(), 0) };
        // SAFETY: `buf` is valid for its length in both directions.
        let ok = unsafe {
            DeviceIoControl(self.0, code, buf.as_ptr().cast(), buf.len() as u32, out_ptr.cast(), out_len, &mut returned, std::ptr::null_mut())
        };
        if ok == 0 {
            // SAFETY: FFI.
            Err(unsafe { GetLastError() })
        } else {
            Ok(returned)
        }
    }

    /// The layout the driver speaks. GET_IMPORTED_DEVICES succeeds only when
    /// its size field is the driver's own sizeof (the header plus one row),
    /// and changes nothing, so the first layout that succeeds is the one. Its
    /// listing comes back too.
    fn probe(&self) -> Result<(Layout, Vec<u8>, u32), VhciError> {
        let mut last = 0;
        for layout in LAYOUTS {
            let mut buf = vec![0u8; HEADER_SIZE + layout.row_size * MAX_PORTS];
            buf[..4].copy_from_slice(&((HEADER_SIZE + layout.row_size) as u32).to_le_bytes());
            match self.ioctl(GET_IMPORTED_DEVICES, &mut buf, true) {
                Ok(n) => return Ok((layout, buf, n)),
                Err(e) => last = e,
            }
        }
        Err(VhciError::UnknownLayout(last))
    }
}

fn write_location(buf: &mut [u8], at: usize, busid: &str, service: &str, host: &str) {
    let put = |buf: &mut [u8], at: usize, s: &str, cap: usize| {
        let n = s.len().min(cap - 1);
        buf[at..at + n].copy_from_slice(&s.as_bytes()[..n]);
    };
    put(buf, at, busid, BUS_ID_SIZE);
    put(buf, at + BUS_ID_SIZE, service, SERVICE_SIZE);
    put(buf, at + BUS_ID_SIZE + SERVICE_SIZE, host, HOST_SIZE);
}

fn read_str(buf: &[u8], at: usize, cap: usize) -> String {
    let field = &buf[at..at + cap];
    let end = field.iter().position(|&b| b == 0).unwrap_or(cap);
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// The layout the installed driver speaks, for diagnostics.
pub fn installed_layout() -> Option<&'static str> {
    Controller::open().ok()?.probe().ok().map(|(l, _, _)| l.name)
}

/// Attach the device `busid` that the server at `host:port` exports. Blocks
/// until the driver has connected, run the import handshake and plugged the
/// device in. Returns the controller port, for [`detach`].
pub fn attach(host: &str, port: u16, busid: &str) -> Result<i32, VhciError> {
    let c = Controller::open()?;
    let (layout, _, _) = c.probe()?;
    let mut buf = vec![0u8; layout.plugin_size];
    buf[..4].copy_from_slice(&(layout.plugin_size as u32).to_le_bytes());
    // 0.9.8.x's serial and wsk_events stay zero: an empty serial keeps the
    // device's own, and no WSK events keeps the IRP receive path.
    write_location(&mut buf, HEADER_SIZE + layout.busid_offset, busid, &port.to_string(), host);
    c.ioctl(PLUGIN_HARDWARE_ONCE, &mut buf, true)
        .map_err(|e| VhciError::Win32("usbip-win2 attach", e))?;
    let vport = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if vport < 1 {
        return Err(VhciError::Win32("usbip-win2 attach (port)", vport as u32));
    }
    Ok(vport)
}

/// Detach the device on a controller port. Detaching first makes the driver
/// close the socket before it unplugs the device.
pub fn detach(vport: i32) -> Result<(), VhciError> {
    let c = Controller::open()?;
    let mut buf = [0u8; PLUGOUT_SIZE];
    buf[..4].copy_from_slice(&(PLUGOUT_SIZE as u32).to_le_bytes());
    buf[4..8].copy_from_slice(&vport.to_le_bytes());
    c.ioctl(PLUGOUT_HARDWARE, &mut buf, false).map(|_| ()).map_err(|e| VhciError::Win32("usbip-win2 detach", e))
}

/// Cancel the driver's re-attach attempts for one exact location. When a
/// connection drops without a plug-out (a crashed process), the driver keeps
/// re-attaching against the dead server. Harmless when there are none.
pub fn stop_attach_attempts(host: &str, port: u16, busid: &str) {
    let Ok(c) = Controller::open() else { return };
    let Ok((layout, _, _)) = c.probe() else { return };
    let mut buf = vec![0u8; layout.stop_size];
    buf[..4].copy_from_slice(&(layout.stop_size as u32).to_le_bytes());
    write_location(&mut buf, HEADER_SIZE + layout.busid_offset, busid, &port.to_string(), host);
    let _ = c.ioctl(STOP_ATTACH_ATTEMPTS, &mut buf, true);
}

/// Every device the controller has imported.
pub fn imported() -> Result<Vec<Imported>, VhciError> {
    let c = Controller::open()?;
    let (layout, buf, written) = c.probe()?;
    let rows = (written as usize).saturating_sub(HEADER_SIZE) / layout.row_size;
    Ok((0..rows)
        .filter_map(|i| {
            let off = HEADER_SIZE + i * layout.row_size;
            let port = i32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
            let b = off + layout.busid_offset;
            (port >= 1).then(|| Imported {
                port,
                busid: read_str(&buf, b, BUS_ID_SIZE),
                service: read_str(&buf, b + BUS_ID_SIZE, SERVICE_SIZE),
                host: read_str(&buf, b + BUS_ID_SIZE + SERVICE_SIZE, HOST_SIZE),
            })
        })
        .collect())
}

/// Plug out every device this app's server range exported, and cancel the
/// re-attach attempts a crashed session left. Run once per process before
/// serving the first device: a crashed session most likely used the same port,
/// and its re-attach loop would otherwise import the new device before this
/// process's own attach. `keep_port` spares a live server's devices.
pub fn sweep_stale(keep_port: Option<u16>) {
    if let Ok(rows) = imported() {
        for r in rows {
            if crate::is_our_location(&r.host, &r.service) && Some(r.service.as_str()) != keep_port.map(|p| p.to_string()).as_deref() {
                let _ = detach(r.port);
            }
        }
    }
    for port in crate::BASE_PORT..crate::BASE_PORT + crate::PORT_RANGE {
        if Some(port) == keep_port {
            continue;
        }
        for n in 1..=8 {
            stop_attach_attempts(crate::HOST, port, &format!("1-{n}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_layout_puts_busid_service_host_in_order() {
        let l = LAYOUTS[0];
        let mut buf = vec![0u8; l.plugin_size];
        write_location(&mut buf, HEADER_SIZE + l.busid_offset, "1-2", "18008", "127.0.0.1");
        let b = HEADER_SIZE + l.busid_offset;
        assert_eq!(read_str(&buf, b, BUS_ID_SIZE), "1-2");
        assert_eq!(read_str(&buf, b + BUS_ID_SIZE, SERVICE_SIZE), "18008");
        assert_eq!(read_str(&buf, b + BUS_ID_SIZE + SERVICE_SIZE, HOST_SIZE), "127.0.0.1");
    }

    // The sizes HIDMaestro measured against each tag's header: the location
    // (port, [hash], busid, service, host) fits inside every request.
    #[test]
    fn layouts_hold_a_whole_location() {
        for l in LAYOUTS {
            let location_end = HEADER_SIZE + l.busid_offset + BUS_ID_SIZE + SERVICE_SIZE + HOST_SIZE;
            assert!(location_end <= l.plugin_size, "{}", l.name);
            assert!(location_end <= l.stop_size, "{}", l.name);
            assert!(l.busid_offset + BUS_ID_SIZE + SERVICE_SIZE + HOST_SIZE <= l.row_size, "{}", l.name);
        }
    }
}
