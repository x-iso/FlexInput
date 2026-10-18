//! Deploys usbip-win2, the transport FlexInput's composite USB personas sit
//! behind (see `flexinput-usbip`). Runs in the elevated helper, the first time
//! a virtual pad's "Audio & Haptics" is turned on.
//!
//! Silent, idempotent: a machine with the host controller running is a no-op.
//! usbip-win2's root-hub filter matches the generic USB root-hub hardware ID,
//! so installing it re-enumerates the root hubs once and USB devices blink.
//!
//! A machine where usbip-win2 is installed but its controller isn't running is
//! left alone rather than "repaired" by running the installer again: run over
//! an existing install, it first uninstalls the old copy, which detaches the
//! filter from every root hub and can hold the PnP lock for many minutes
//! (measured by HIDMaestro, whose installer flow this follows).

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use flexinput_usbip::vhci;

use crate::deploy::{system32, DeployError, StagingDir};
use crate::usbip_bundled as bundled;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
/// The controller's own device arrives quickly once the installer returns; the
/// root-hub re-enumeration its filter triggers takes longer and doesn't gate us.
const APPEAR_TIMEOUT: Duration = Duration::from_secs(60);

/// Make sure usbip-win2's host controller is installed and running.
/// `Ok(true)` when this call installed it, `Ok(false)` when it already was.
/// **Requires elevation.**
pub fn ensure_usbip_installed() -> Result<bool, DeployError> {
    if vhci::is_available() {
        return Ok(false);
    }
    if product_installed() {
        return Err(DeployError::UsbipNotRunning);
    }
    if !bundled::is_pinned_installer(bundled::INSTALLER) {
        return Err(DeployError::UsbipUnverified);
    }
    let dir = StagingDir::create()?;
    let exe = dir.path().join(bundled::INSTALLER_NAME);
    std::fs::write(&exe, bundled::INSTALLER)?;
    // The BSD-2 notice travels with the binary it covers.
    std::fs::write(dir.path().join("usbip-win2-LICENSE.txt"), bundled::LICENSE)?;
    // The directory is Administrators/SYSTEM-only, but check what is about to
    // run rather than what was written.
    if !bundled::is_pinned_installer(&std::fs::read(&exe)?) {
        return Err(DeployError::UsbipUnverified);
    }
    run_installer(&exe)?;
    let start = Instant::now();
    while start.elapsed() < APPEAR_TIMEOUT {
        if vhci::is_available() {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(DeployError::UsbipDidNotAppear)
}

fn run_installer(exe: &Path) -> Result<(), DeployError> {
    let mut child = Command::new(exe)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            // Inno Setup: 0 success, 3010 success with a reboot pending. The
            // driver works before that reboot; the availability poll decides.
            return match status.code() {
                Some(0) | Some(3010) => Ok(()),
                code => Err(DeployError::UsbipInstaller(code.unwrap_or(-1))),
            };
        }
        if start.elapsed() > INSTALL_TIMEOUT {
            let _ = child.kill();
            return Err(DeployError::UsbipInstaller(-1));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Whether usbip-win2's host controller package is in the DriverStore.
fn product_installed() -> bool {
    Command::new(system32().join("pnputil.exe"))
        .arg("/enum-drivers")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_ascii_lowercase().contains("usbip2_ude.inf"))
        .unwrap_or(false)
}
