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
//! (`Dongle::open`), so Linux's Bluetooth loses that adapter until FlexInput
//! lets it go.

use std::path::Path;

/// The rule file, early enough (< 73) for logind's `uaccess` to apply.
pub const RULES_PATH: &str = "/etc/udev/rules.d/62-flexinput-bluetooth.rules";

/// The adapters handed to FlexInput, as `vid:pid`.
pub fn dedicated() -> Vec<(u16, u16)> {
    parse(&std::fs::read_to_string(RULES_PATH).unwrap_or_default())
}

/// Hand the adapter `vid:pid` to FlexInput (`on`) or back to Linux, through
/// pkexec (the desktop's password prompt). Blocks until it's answered, so run
/// it off the UI thread. Handing one back takes effect once FlexInput lets go
/// of it, at the latest when it closes.
pub fn set_dedicated(vid: u16, pid: u16, on: bool) -> Result<(), String> {
    let mut ids = dedicated();
    ids.retain(|&id| id != (vid, pid));
    if on {
        ids.push((vid, pid));
    }
    let status = std::process::Command::new("pkexec")
        .arg("/bin/sh")
        .arg("-c")
        .arg(script(&ids, vid, pid))
        .status()
        .map_err(|e| format!("couldn't run pkexec: {e}"))?;
    if !status.success() {
        return Err(match status.code() {
            Some(126) => "the password prompt was dismissed".to_string(),
            Some(c) => format!("pkexec exited with {c} (not authorized, or no polkit agent running)"),
            None => "pkexec was killed".to_string(),
        });
    }
    Ok(())
}

/// The adapter's manufacturer and product, from sysfs: no need to open it,
/// which an adapter Linux is using must not be.
pub fn names(bus: u8, ports: &[u8]) -> (Option<String>, Option<String>) {
    if ports.is_empty() {
        return (None, None);
    }
    let path: Vec<String> = ports.iter().map(u8::to_string).collect();
    let dir = Path::new("/sys/bus/usb/devices").join(format!("{bus}-{}", path.join(".")));
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    (read("manufacturer"), read("product"))
}

/// The root half of [`set_dedicated`]: the whole rule file for `ids`, then
/// the change applied to the adapter `vid:pid` at once.
fn script(ids: &[(u16, u16)], vid: u16, pid: u16) -> String {
    format!(
        "set -e\n\
         mkdir -p /etc/udev/rules.d\n\
         cat > {RULES_PATH} <<'FLEXINPUT_RULES'\n{}FLEXINPUT_RULES\n\
         udevadm control --reload-rules || true\n\
         udevadm trigger --action=change --subsystem-match=usb \
         --attr-match=idVendor={vid:04x} --attr-match=idProduct={pid:04x} || true\n\
         udevadm settle --timeout=3 || true\n",
        rules(ids)
    )
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
        let s = script(&[(0x0BDA, 0xA728)], 0x0BDA, 0xA728);
        let check = std::process::Command::new("sh").arg("-n").arg("-c").arg(&s).status().unwrap();
        assert!(check.success(), "{s}");
        assert!(s.contains("--attr-match=idVendor=0bda --attr-match=idProduct=a728"), "{s}");
    }
}
