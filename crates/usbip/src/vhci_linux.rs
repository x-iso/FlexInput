//! The Linux kernel's USB/IP host controller, `vhci-hcd`, through its sysfs
//! files (`drivers/usb/usbip/vhci_sysfs.c`) — the counterpart of usbip-win2's
//! ioctls in `vhci.rs`, with the same calls.
//!
//! Attaching is what the `usbip attach` tool does: connect to the server, run
//! the import handshake on that socket, then write `<port> <socket fd> <devid>
//! <speed>` to `attach`. The kernel takes the socket from the writer's own fd
//! table and keeps it, so the fd can be closed right after; from then on the
//! kernel's host controller and this process's server talk over it, and the
//! kernel binds its own drivers (snd-usb-audio, usbhid, hid-playstation) as to
//! a pad on a USB port.
//!
//! The `attach` and `detach` files are root's (0200). FlexInput's one-time
//! setup (`flexinput_virtual`'s Linux `access`) hands them to the user and
//! loads the module at every boot, so attaching needs no root here. When the
//! process dies its server's sockets close, and the kernel unplugs the device
//! itself: there's nothing stale to sweep, unlike usbip-win2.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::protocol::{write_op_common, BUS_ID_SIZE, OP_REQ_IMPORT, ST_OK, USB_DEVICE_SIZE};

/// The first (with `CONFIG_USBIP_VHCI_NR_HCS=1`, the only) controller: the one
/// that has the `attach` and `detach` files.
pub const SYSFS: &str = "/sys/devices/platform/vhci_hcd.0";

/// `enum usb_device_speed`: SuperSpeed devices go on the controller's
/// SuperSpeed ports, the rest on its high-speed ones.
const SPEED_SUPER: u32 = 5;
/// `VDEV_ST_NULL`: a free port.
const STATUS_FREE: u32 = 4;
/// `EBUSY`: the port went in the meantime.
const EBUSY: i32 = 16;

#[derive(Debug)]
pub enum VhciError {
    /// No `vhci_hcd.0`: the module isn't loaded, or the kernel has none.
    Absent,
    /// The import handshake with the server failed.
    Import(std::io::Error),
    /// The server refused the import (an `OP_REP_IMPORT` status).
    Refused(u32),
    /// Every port of the device's speed is taken.
    NoFreePort,
    /// Reading or writing one of the controller's files failed.
    Sysfs(&'static str, std::io::Error),
}

impl std::fmt::Display for VhciError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VhciError::Absent => write!(f, "the kernel's USB/IP host controller (vhci-hcd) isn't loaded"),
            VhciError::Import(e) => write!(f, "USB/IP import handshake: {e}"),
            VhciError::Refused(st) => write!(f, "the USB/IP server refused the import (status {st})"),
            VhciError::NoFreePort => write!(f, "every port of the USB/IP host controller is in use"),
            VhciError::Sysfs(what, e) => write!(f, "{SYSFS}/{what}: {e}"),
        }
    }
}

impl std::error::Error for VhciError {}

/// Whether the controller is loaded and this process may attach and detach.
pub fn is_available() -> bool {
    let writable = |f: &str| std::fs::OpenOptions::new().write(true).open(Path::new(SYSFS).join(f)).is_ok();
    writable("attach") && writable("detach")
}

/// Attach the device `busid` that the server at `host:port` exports. Returns
/// the controller port, for [`detach`].
pub fn attach(host: &str, port: u16, busid: &str) -> Result<i32, VhciError> {
    if !Path::new(SYSFS).exists() {
        return Err(VhciError::Absent);
    }
    let mut sock = TcpStream::connect((host, port)).map_err(VhciError::Import)?;
    let _ = sock.set_nodelay(true);
    let (devid, speed) = import(&mut sock, busid)?;
    let free = free_ports(&std::fs::read_to_string(Path::new(SYSFS).join("status")).map_err(|e| VhciError::Sysfs("status", e))?, speed);
    for vport in free {
        let line = format!("{vport} {} {devid} {speed}", sock.as_raw_fd());
        match write_attr("attach", &line) {
            Ok(()) => return Ok(vport as i32),
            Err(e) if e.raw_os_error() == Some(EBUSY) => continue,
            Err(e) => return Err(VhciError::Sysfs("attach", e)),
        }
    }
    Err(VhciError::NoFreePort)
}

/// Detach the device on a controller port. The kernel shuts its socket down
/// first, so the server sees the connection end.
pub fn detach(vport: i32) -> Result<(), VhciError> {
    write_attr("detach", &vport.to_string()).map_err(|e| VhciError::Sysfs("detach", e))
}

/// Nothing to do on Linux: see the module notes.
pub fn sweep_stale(_keep_port: Option<u16>) {}

fn write_attr(name: &str, value: &str) -> std::io::Result<()> {
    std::fs::OpenOptions::new().write(true).open(Path::new(SYSFS).join(name))?.write_all(value.as_bytes())
}

/// `OP_REQ_IMPORT` on `sock`; the device's devid (bus << 16 | dev) and speed.
fn import(sock: &mut TcpStream, busid: &str) -> Result<(u32, u32), VhciError> {
    let mut req = vec![0u8; 8 + BUS_ID_SIZE];
    write_op_common(&mut req, OP_REQ_IMPORT, 0);
    let n = busid.len().min(BUS_ID_SIZE - 1);
    req[8..8 + n].copy_from_slice(&busid.as_bytes()[..n]);
    sock.write_all(&req).map_err(VhciError::Import)?;
    let mut op = [0u8; 8];
    sock.read_exact(&mut op).map_err(VhciError::Import)?;
    let status = u32::from_be_bytes([op[4], op[5], op[6], op[7]]);
    if status != ST_OK {
        return Err(VhciError::Refused(status));
    }
    let mut dev = [0u8; USB_DEVICE_SIZE];
    sock.read_exact(&mut dev).map_err(VhciError::Import)?;
    Ok(device_ids(&dev))
}

/// From a `usbip_usb_device` block: (devid, speed). The path and busid come
/// first (256 + 32 bytes), then busnum, devnum and speed, big-endian.
fn device_ids(dev: &[u8]) -> (u32, u32) {
    let be = |at: usize| u32::from_be_bytes([dev[at], dev[at + 1], dev[at + 2], dev[at + 3]]);
    let (busnum, devnum, speed) = (be(288), be(292), be(296));
    ((busnum << 16) | (devnum & 0xFFFF), speed)
}

/// The free ports of the hub a device of `speed` goes on, from the `status`
/// file: a header, then `hs  0000 004 000 00000000 000000 0-0` per port (hub,
/// port, status, ...).
fn free_ports(status: &str, speed: u32) -> Vec<u32> {
    let hub = if speed == SPEED_SUPER { "ss" } else { "hs" };
    status
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (h, port, st) = (f.next()?, f.next()?, f.next()?);
            (h == hub && st.parse::<u32>().ok()? == STATUS_FREE).then(|| port.parse().ok()).flatten()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{write_usb_device, UsbDeviceBlock, SPEED_HIGH};

    #[test]
    fn free_ports_are_read_per_hub() {
        let status = "hub port sta spd dev      sockfd local_busid\n\
                      hs  0000 006 003 00010002 000003 1-1\n\
                      hs  0001 004 000 00000000 000000 0-0\n\
                      hs  0002 004 000 00000000 000000 0-0\n\
                      ss  0008 004 000 00000000 000000 0-0\n";
        assert_eq!(free_ports(status, SPEED_HIGH), vec![1, 2]);
        assert_eq!(free_ports(status, SPEED_SUPER), vec![8]);
    }

    #[test]
    fn the_import_reply_gives_devid_and_speed() {
        let mut block = vec![0u8; USB_DEVICE_SIZE];
        write_usb_device(&mut block, &UsbDeviceBlock {
            path: "/flexinput/1-2",
            busid: "1-2",
            busnum: 1,
            devnum: 2,
            speed: SPEED_HIGH,
            vid: 0x054C,
            pid: 0x0CE6,
            bcd_device: 0x0100,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            configuration_value: 1,
            num_configurations: 1,
            num_interfaces: 4,
        });
        assert_eq!(device_ids(&block), ((1 << 16) | 2, SPEED_HIGH));
    }
}
