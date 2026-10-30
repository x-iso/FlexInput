//! Which Bluetooth adapters FlexInput may take, on Linux.
//!
//! Windows marks them by driver: an adapter bound to WinUSB (with Zadig) is
//! FlexInput's, and every other one belongs to Windows. Linux has no such
//! step — the kernel's btusb drives every adapter and BlueZ uses them all — so
//! the user hands one over in the Bluetooth panel instead. That writes a udev
//! rule giving the person at the computer the adapter's USB node (`uaccess`),
//! and the rule file doubles as the record of which adapters are FlexInput's:
//! one it doesn't list is never opened, even when it could be.
//!
//! While FlexInput holds a handed-over adapter, libusb detaches btusb from it
//! (`Dongle::open`), so Linux's Bluetooth loses that adapter.
//!
//! ⛔ **Giving one back re-plugs it** (`authorized` off and on, as root). The
//! kernel never re-attaches a driver on its own when a program lets go of a
//! USB device — not when FlexInput closes it, not when FlexInput exits — so
//! a given-back adapter sat with no driver at all: FlexInput could still see
//! it, and Linux's Bluetooth had nothing. The re-plug also ends FlexInput's
//! hold on it while it runs (`radio` notices the adapter going).

use std::path::Path;

/// The rule file, early enough (< 73) for logind's `uaccess` to apply.
pub const RULES_PATH: &str = "/etc/udev/rules.d/62-flexinput-bluetooth.rules";

/// The adapters handed to FlexInput, as `vid:pid`.
pub fn dedicated() -> Vec<(u16, u16)> {
    parse(&std::fs::read_to_string(RULES_PATH).unwrap_or_default())
}

/// Hand the adapter `vid:pid` to FlexInput (`on`) or back to Linux, through
/// pkexec (the desktop's password prompt). Blocks until it's answered, so run
/// it off the UI thread. Handing one back takes effect at once: the adapter is
/// re-plugged, and btusb takes it again.
pub fn set_dedicated(vid: u16, pid: u16, on: bool) -> Result<(), String> {
    let mut ids = dedicated();
    ids.retain(|&id| id != (vid, pid));
    if on {
        ids.push((vid, pid));
    }
    let out = std::process::Command::new("pkexec")
        .arg("/bin/sh")
        .arg("-c")
        .arg(script(&ids, vid, pid, on))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("couldn't run pkexec: {e}"))?;
    if !out.status.success() {
        // pkexec's own failures are 126 and 127; anything else is the
        // script's, which says what went wrong on stderr.
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(match out.status.code() {
            Some(126) => "the password prompt was dismissed".to_string(),
            Some(127) => "not authorized (or no polkit agent is running)".to_string(),
            Some(c) if said.is_empty() => format!("the change failed (exit {c})"),
            Some(_) => said,
            None => "pkexec was killed".to_string(),
        });
    }
    Ok(())
}

/// What sysfs says about the USB adapter at `bus` / `ports`: no need to open
/// it, which an adapter Linux is using must not be.
#[derive(Debug, Default)]
pub struct SysfsInfo {
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    /// The `hciN` Linux's Bluetooth runs on it, while btusb drives it.
    pub hci: Option<String>,
    /// Its `removable` attribute says `fixed`: built into the machine.
    pub built_in: bool,
}

pub fn sysfs_info(bus: u8, ports: &[u8]) -> SysfsInfo {
    if ports.is_empty() {
        return SysfsInfo::default();
    }
    let path: Vec<String> = ports.iter().map(u8::to_string).collect();
    let name = format!("{bus}-{}", path.join("."));
    let dir = Path::new("/sys/bus/usb/devices").join(&name);
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    // `<device>/<device>:<config>.<interface>/bluetooth/hciN`.
    let hci = std::fs::read_dir(&dir).ok().and_then(|entries| {
        entries.flatten().find_map(|e| {
            let iface = e.file_name().to_string_lossy().into_owned();
            if !iface.starts_with(&format!("{name}:")) {
                return None;
            }
            std::fs::read_dir(e.path().join("bluetooth")).ok()?.flatten().find_map(|h| {
                let h = h.file_name().to_string_lossy().into_owned();
                h.starts_with("hci").then_some(h)
            })
        })
    });
    SysfsInfo {
        manufacturer: read("manufacturer"),
        product: read("product"),
        hci,
        built_in: read("removable").as_deref() == Some("fixed"),
    }
}

/// The `hciN` Linux's Bluetooth tools use when not told which: the lowest
/// numbered (bluetoothd's default adapter, `bluetoothctl`'s first).
pub fn default_hci() -> Option<String> {
    std::fs::read_dir("/sys/class/bluetooth")
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n: u32 = name.strip_prefix("hci")?.parse().ok()?;
            Some((n, name))
        })
        .min()
        .map(|(_, name)| name)
}

/// The root half of [`set_dedicated`]: the whole rule file for `ids`, then
/// the change applied to the adapter `vid:pid` at once — handed back (`!on`),
/// re-plugged, so btusb probes it again.
fn script(ids: &[(u16, u16)], vid: u16, pid: u16, on: bool) -> String {
    let mut s = format!(
        "set -e\n\
         mkdir -p /etc/udev/rules.d\n\
         cat > {RULES_PATH} <<'FLEXINPUT_RULES'\n{}FLEXINPUT_RULES\n\
         udevadm control --reload-rules || true\n\
         udevadm trigger --action=change --subsystem-match=usb \
         --attr-match=idVendor={vid:04x} --attr-match=idProduct={pid:04x} || true\n\
         udevadm settle --timeout=3 || true\n",
        rules(ids)
    );
    if !on {
        // The USB devices, not their interfaces: only a device has the ids.
        //
        // ❗ Retried until the adapter is configured again. Authorizing it can
        // fail half way — authorized, but its configuration refused — and it
        // then sits with no interfaces for any driver to take: seen once, on a
        // built-in Qualcomm adapter. An empty `bConfigurationValue` is that.
        s.push_str(&format!(
            "for d in /sys/bus/usb/devices/*; do\n\
             [ -f \"$d/idVendor\" ] && [ -f \"$d/authorized\" ] || continue\n\
             [ \"$(cat \"$d/idVendor\")\" = {vid:04x} ] && [ \"$(cat \"$d/idProduct\")\" = {pid:04x} ] || continue\n\
             back=\n\
             for try in 1 2 3; do\n\
             echo 0 > \"$d/authorized\" || true\n\
             sleep 1\n\
             echo 1 > \"$d/authorized\" || true\n\
             sleep 1\n\
             if [ -n \"$(cat \"$d/bConfigurationValue\" 2>/dev/null)\" ]; then back=1; break; fi\n\
             done\n\
             [ -n \"$back\" ] || {{ echo \"the adapter didn't come back after re-plugging it — replug it, or restart if it's built in\" >&2; exit 3; }}\n\
             done\n"
        ));
    }
    s
}

fn rules(ids: &[(u16, u16)]) -> String {
    let mut r = String::from(
        "# Written by FlexInput: Bluetooth adapters handed to its own Bluetooth stack\n\
         # (its Bluetooth panel). The person at the computer may open them, and\n\
         # FlexInput takes them from Linux's Bluetooth while it holds them.\n",
    );
    for (v, p) in ids {
        r.push_str(&format!(
            "SUBSYSTEM==\"usb\", ENV{{DEVTYPE}}==\"usb_device\", ATTR{{idVendor}}==\"{v:04x}\", \
             ATTR{{idProduct}}==\"{p:04x}\", TAG+=\"uaccess\"\n"
        ));
    }
    r
}

/// The ids in a rule file [`rules`] wrote.
fn parse(text: &str) -> Vec<(u16, u16)> {
    let attr = |line: &str, key: &str| {
        let at = line.find(key)? + key.len();
        u16::from_str_radix(line.get(at..at + 4)?, 16).ok()
    };
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| Some((attr(l, "ATTR{idVendor}==\"")?, attr(l, "ATTR{idProduct}==\"")?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_file_reads_back_what_it_lists() {
        let ids = [(0x0BDA, 0xA728), (0x0A12, 0x0001)];
        assert_eq!(parse(&rules(&ids)), ids);
        assert!(parse(&rules(&[])).is_empty());
        assert!(rules(&ids).contains(
            "SUBSYSTEM==\"usb\", ENV{DEVTYPE}==\"usb_device\", ATTR{idVendor}==\"0bda\", \
             ATTR{idProduct}==\"a728\", TAG+=\"uaccess\""
        ));
    }

    #[test]
    fn the_script_parses() {
        for on in [true, false] {
            let s = script(&[(0x0BDA, 0xA728)], 0x0BDA, 0xA728, on);
            let check = std::process::Command::new("sh").arg("-n").arg("-c").arg(&s).status().unwrap();
            assert!(check.success(), "{s}");
            assert!(s.contains("--attr-match=idVendor=0bda --attr-match=idProduct=a728"), "{s}");
            // ⛔ Only handing back re-plugs; handing over leaves btusb to libusb.
            assert_eq!(s.contains("authorized"), !on, "{s}");
        }
        let back = script(&[], 0x0BDA, 0xA728, false);
        assert!(back.contains("= 0bda ] && [ \"$(cat \"$d/idProduct\")\" = a728 ]"), "{back}");
    }
}
