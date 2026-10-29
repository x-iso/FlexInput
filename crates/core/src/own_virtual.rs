//! How FlexInput tells its own Linux virtual devices (uinput / UHID) from a
//! controller in someone's hands — the Linux counterpart of the HIDMaestro
//! instance-path test on Windows. Reading our own output back as input loops.
//!
//! Every device `flexinput-virtual` makes on Linux carries both marks:
//! - [`VERSION`] as its input id's version. gilrs only shows a pad's uuid, which
//!   holds the version, so this is the mark it can see.
//! - A `phys` starting with [`PHYS_PREFIX`]. SDL and hidapi show a node path,
//!   and sysfs gives that node's `phys`; [`node_is_own`] reads it.
//!
//! Except the pads with USB audio, which are real USB devices to the kernel
//! (attached over USB/IP), with a real pad's version and phys: they're told by
//! the USB serial FlexInput gave them ([`is_own_usb_serial`]), on Windows too.
//! The vendored gilrs-core turns that serial into [`VERSION`] in its uuid.

/// The input id version of FlexInput's own virtual devices. A real pad's
/// bcdDevice; nothing that drives a pad keys on it (SDL matches mappings with
/// and without it, the kernel drivers not at all).
pub const VERSION: u16 = 0xF1E0;

/// The `phys` prefix of FlexInput's own virtual devices.
pub const PHYS_PREFIX: &str = "flexinput/";

/// Whether a uuid in SDL's GUID layout (gilrs's on Linux) has [`VERSION`]:
/// bytes 12..14, little-endian.
pub fn uuid_is_own(uuid: &[u8; 16]) -> bool {
    u16::from_le_bytes([uuid[12], uuid[13]]) == VERSION
}

/// Whether a USB serial is one FlexInput gives its pads with USB audio: "FX"
/// and twelve uppercase hex digits (`flexinput_usbip::serial_for`).
pub fn is_own_usb_serial(serial: &str) -> bool {
    serial.len() == 14
        && serial.starts_with("FX")
        && serial[2..].bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
}

/// Whether the device node at `path` (`/dev/hidrawN`, `/dev/input/eventN` or
/// `/dev/input/jsN`) is one of ours: by its `phys` in sysfs, or by the USB
/// device it hangs off. False for any other path, or when sysfs can't say.
#[cfg(target_os = "linux")]
pub fn node_is_own(path: &str) -> bool {
    let Some(name) = path.rsplit('/').next() else { return false };
    let device = if name.starts_with("hidraw") {
        format!("/sys/class/hidraw/{name}/device")
    } else if name.starts_with("event") || name.starts_with("js") {
        format!("/sys/class/input/{name}/device")
    } else {
        return false;
    };
    let phys_is_own = if name.starts_with("hidraw") {
        // A HID device's phys is in its uevent, not a file of its own.
        std::fs::read_to_string(format!("{device}/uevent"))
            .is_ok_and(|u| u.lines().any(|l| l.strip_prefix("HID_PHYS=").is_some_and(|p| p.starts_with(PHYS_PREFIX))))
    } else {
        std::fs::read_to_string(format!("{device}/phys")).is_ok_and(|p| p.starts_with(PHYS_PREFIX))
    };
    phys_is_own || sysfs_usb_is_own(std::path::Path::new(&device))
}

/// Whether the sysfs device `dir` (or a symlink to it) hangs off a USB device
/// with one of our serials: a node of a pad with USB audio.
#[cfg(target_os = "linux")]
pub fn sysfs_usb_is_own(dir: &std::path::Path) -> bool {
    let Ok(mut dir) = std::fs::canonicalize(dir) else { return false };
    loop {
        // Only a USB device has a `serial` and an `idVendor` beside it.
        if dir.join("idVendor").exists() {
            if let Ok(serial) = std::fs::read_to_string(dir.join("serial")) {
                return is_own_usb_serial(serial.trim());
            }
        }
        if !dir.pop() || dir.as_os_str() == "/sys/devices" {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_mark_is_read_where_sdl_puts_it() {
        // 03000000 5e04 0000 8e02 0000 e0f1 0000: an Xbox 360 pad on USB with ours.
        let mut uuid = [0u8; 16];
        uuid[0] = 0x03;
        uuid[4..6].copy_from_slice(&0x045Eu16.to_le_bytes());
        uuid[8..10].copy_from_slice(&0x028Eu16.to_le_bytes());
        uuid[12..14].copy_from_slice(&VERSION.to_le_bytes());
        assert!(uuid_is_own(&uuid));
        uuid[12..14].copy_from_slice(&0x0114u16.to_le_bytes());
        assert!(!uuid_is_own(&uuid), "a real pad's version");
    }

    #[test]
    fn own_usb_serials_are_recognised() {
        assert!(is_own_usb_serial("FX0123456789AB"));
        assert!(!is_own_usb_serial("FX12"));
        assert!(!is_own_usb_serial("HM0123456789AB"));
        assert!(!is_own_usb_serial("FX0123456789ab"), "uppercase only, as made");
    }
}
