//! Write access to `/dev/uinput` and `/dev/uhid`, which every Linux virtual
//! device needs. A desktop doesn't give them to the logged-in user by default
//! (Steam's udev rules grant uinput; nothing grants uhid), so the first device
//! that finds one closed asks once, through polkit (the desktop's own password
//! prompt), to install a udev rule that does and to apply it to this session at
//! once. The Linux counterpart of the one UAC prompt that installs HIDMaestro.
//!
//! The pads with USB audio need the kernel's USB/IP host controller instead
//! ([`ensure_vhci`]): loaded, and its attach / detach files writable by the
//! user. Asked for the same way, the first time one is built, like the one UAC
//! prompt that installs usbip-win2.

use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(crate) const UINPUT: &str = "/dev/uinput";
pub(crate) const UHID: &str = "/dev/uhid";

/// Where the rule goes, and what it says: the person at the computer (logind's
/// `uaccess`, the same grant the Steam rules use) may open both nodes.
const RULES_PATH: &str = "/etc/udev/rules.d/60-flexinput.rules";
const RULES: &str = "\
# Written by FlexInput: lets the person at the computer create virtual
# controllers, keyboards and mice (uinput) and virtual HID devices (uhid).
KERNEL==\"uinput\", SUBSYSTEM==\"misc\", TAG+=\"uaccess\", OPTIONS+=\"static_node=uinput\"
KERNEL==\"uhid\", SUBSYSTEM==\"misc\", TAG+=\"uaccess\", OPTIONS+=\"static_node=uhid\"
";

/// How long a declined or failed request answers for itself before the next
/// device may ask again — long enough that the pad and the keyboard/mouse an
/// Easy-mode pick builds together prompt once, short enough that adding a
/// device again later asks again.
const RETRY_AFTER: Duration = Duration::from_secs(20);

/// Whether this process can open both nodes for reading and writing.
pub(crate) fn granted() -> bool {
    writable(UINPUT) && writable(UHID)
}

fn writable(path: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(path) else { return false };
    // SAFETY: a valid NUL-terminated path; access() only reads it.
    unsafe { libc::access(c.as_ptr(), libc::R_OK | libc::W_OK) == 0 }
}

/// Make sure both nodes are writable, asking through pkexec when they aren't.
/// Blocks while the password prompt is up, so it runs on the device-build
/// worker, never the UI thread. `Err` says what went wrong, for the log.
pub(crate) fn ensure() -> Result<(), String> {
    static LAST: Mutex<Option<(Instant, String)>> = Mutex::new(None);
    if granted() {
        return Ok(());
    }
    // One prompt at a time: a second device waits here, then finds the first
    // one's answer.
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if granted() {
        return Ok(());
    }
    if let Some((at, err)) = last.as_ref() {
        if at.elapsed() < RETRY_AFTER {
            return Err(err.clone());
        }
    }
    let result = grant();
    *last = result.as_ref().err().map(|e| (Instant::now(), e.clone()));
    result
}

/// The module, loaded at every boot, and the rule that hands the controller's
/// attach and detach files to the user (root's alone otherwise). The kernel
/// takes an attach's socket from the writer, so this lets FlexInput attach its
/// own server's devices without root.
const VHCI_MODULES_PATH: &str = "/etc/modules-load.d/flexinput-usbip.conf";
const VHCI_RULES_PATH: &str = "/etc/udev/rules.d/61-flexinput-usbip.rules";
/// The script's exit code when the kernel has no vhci-hcd module.
const NO_VHCI_MODULE: i32 = 3;

/// Make sure the kernel's USB/IP host controller is loaded and this process
/// may attach to it, asking through pkexec when it isn't. Blocks while the
/// password prompt is up, so it runs on the device-build worker.
pub(crate) fn ensure_vhci() -> Result<(), String> {
    use flexinput_usbip::vhci;
    static LAST: Mutex<Option<(Instant, String)>> = Mutex::new(None);
    if vhci::is_available() {
        return Ok(());
    }
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if vhci::is_available() {
        return Ok(());
    }
    if let Some((at, err)) = last.as_ref() {
        if at.elapsed() < RETRY_AFTER {
            return Err(err.clone());
        }
    }
    let result = grant_vhci();
    *last = result.as_ref().err().map(|e| (Instant::now(), e.clone()));
    result
}

fn grant_vhci() -> Result<(), String> {
    use flexinput_usbip::vhci;
    // SAFETY: getuid() has no preconditions.
    let uid = unsafe { libc::getuid() };
    let sysfs = vhci::SYSFS;
    let script = vhci_script(uid);
    eprintln!("[linux-access] the USB/IP host controller isn't set up; asking through pkexec");
    let status = std::process::Command::new("pkexec")
        .arg("/bin/sh")
        .arg("-c")
        .arg(&script)
        .status()
        .map_err(|e| format!("couldn't run pkexec: {e}"))?;
    if !status.success() {
        return Err(match status.code() {
            Some(NO_VHCI_MODULE) => "this kernel has no USB/IP host controller (the vhci-hcd module)".to_string(),
            Some(126) => "the password prompt was dismissed".to_string(),
            Some(c) => format!("pkexec exited with {c} (not authorized, or no polkit agent running)"),
            None => "pkexec was killed".to_string(),
        });
    }
    if !vhci::is_available() {
        return Err(format!("set up, but {sysfs}/attach is still not writable"));
    }
    eprintln!("[linux-access] USB/IP host controller ready");
    Ok(())
}

/// The root half of [`ensure_vhci`], for user `uid`. udev runs RUN programs
/// without a PATH worth the name, so the rule names chown by the full path
/// this shell finds (NixOS keeps it outside /usr).
fn vhci_script(uid: u32) -> String {
    let sysfs = flexinput_usbip::vhci::SYSFS;
    format!(
        "PATH=/run/current-system/sw/bin:/usr/sbin:/usr/bin:/sbin:/bin:$PATH\n\
         modprobe -q vhci-hcd || exit {NO_VHCI_MODULE}\n\
         set -e\n\
         chown_bin=$(command -v chown)\n\
         mkdir -p /etc/modules-load.d /etc/udev/rules.d\n\
         printf '# Written by FlexInput: the USB/IP host controller its pads with USB audio attach to.\\nvhci-hcd\\n' > {VHCI_MODULES_PATH}\n\
         cat > {VHCI_RULES_PATH} <<FLEXINPUT_RULES\n\
         # Written by FlexInput: lets user {uid} attach and detach FlexInput's virtual\n\
         # pads with USB audio (USB/IP).\n\
         ACTION==\"add\", SUBSYSTEM==\"platform\", KERNEL==\"vhci_hcd.0\", RUN+=\"$chown_bin {uid} /sys%p/attach /sys%p/detach\"\n\
         FLEXINPUT_RULES\n\
         udevadm control --reload-rules || true\n\
         $chown_bin {uid} {sysfs}/attach {sysfs}/detach\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vhci_script_parses_and_names_the_user() {
        let script = vhci_script(1000);
        let check = std::process::Command::new("sh").arg("-n").arg("-c").arg(&script).status().unwrap();
        assert!(check.success(), "{script}");
        assert!(script.contains("RUN+=\"$chown_bin 1000 /sys%p/attach /sys%p/detach\""), "{script}");
        assert!(script.contains("$chown_bin 1000 /sys/devices/platform/vhci_hcd.0/attach"), "{script}");
    }
}

fn grant() -> Result<(), String> {
    // SAFETY: getuid() has no preconditions.
    let uid = unsafe { libc::getuid() };
    // The rule covers later sessions and boots; the trigger re-runs it for the
    // live nodes (logind applies `uaccess` on the change event), and setfacl
    // covers a system where that doesn't take. Loading the modules first makes
    // the sysfs devices the trigger needs exist.
    let script = format!(
        "set -e\n\
         modprobe -q uinput || true\n\
         modprobe -q uhid || true\n\
         mkdir -p /etc/udev/rules.d\n\
         cat > {RULES_PATH} <<'FLEXINPUT_RULES'\n{RULES}FLEXINPUT_RULES\n\
         udevadm control --reload-rules || true\n\
         udevadm trigger --action=change --sysname-match=uinput --sysname-match=uhid || true\n\
         udevadm settle --timeout=3 || true\n\
         setfacl -m u:{uid}:rw {UINPUT} {UHID} 2>/dev/null || true\n"
    );
    eprintln!("[linux-access] {UINPUT} / {UHID} not writable; asking through pkexec");
    let status = std::process::Command::new("pkexec")
        .arg("/bin/sh")
        .arg("-c")
        .arg(&script)
        .status()
        .map_err(|e| format!("couldn't run pkexec: {e}"))?;
    if !status.success() {
        // 126: the prompt was dismissed; 127: not authorized, or no polkit agent.
        return Err(match status.code() {
            Some(126) => "the password prompt was dismissed".to_string(),
            Some(c) => format!("pkexec exited with {c} (not authorized, or no polkit agent running)"),
            None => "pkexec was killed".to_string(),
        });
    }
    // The ACL lands when udev processes the change event; give it a moment.
    let deadline = Instant::now() + Duration::from_secs(3);
    while !granted() {
        if Instant::now() >= deadline {
            return Err(format!("the rule is installed, but {UINPUT} / {UHID} are still not writable"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!("[linux-access] granted");
    Ok(())
}
