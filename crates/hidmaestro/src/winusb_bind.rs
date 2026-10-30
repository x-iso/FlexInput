//! Hands a Bluetooth adapter to FlexInput's own Bluetooth stack
//! (`flexinput-btle`) from its Bluetooth window: what Zadig does, without
//! Zadig. Runs in the elevated helper.
//!
//! Zadig's method (libwdi's): a driver package naming just that adapter's
//! hardware ID that installs Windows' own WinUSB (`Include = winusb.inf`, no
//! binaries of its own), signed on this machine and force-installed onto the
//! adapter. Forced, because a vendor's Bluetooth driver naming the same
//! hardware ID with a Microsoft signature can outrank a locally signed package.
//! The adapter keeps WinUSB, FlexInput running or not, until it's given back:
//! deleting the package uninstalls it from the adapter, and Windows puts its
//! own Bluetooth driver back.
//!
//! The package has its own signing cert ([`CERT_NAME`]), apart from
//! HIDMaestro's, so reinstalling HIDMaestro (which retires superseded certs)
//! never strands it; the last give-back retires it.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{UpdateDriverForPlugAndPlayDevicesW, INSTALLFLAG_FORCE};
use windows_sys::Win32::Foundation::GetLastError;

use crate::deploy::{system32, DeployError, StagingDir};
use crate::signing::{self, DriverPackage, Scope};

/// The cert the adapter packages are signed with.
pub const CERT_NAME: &str = "FlexInput Bluetooth Adapter";
/// The `Provider` the packages declare, which is how give-back tells one of
/// FlexInput's from any other WinUSB package.
const PROVIDER: &str = "FlexInput";
/// What Zadig's packages declare: give-back undoes those too.
const ZADIG_PROVIDER: &str = "libwdi";
/// The device interface WinUSB registers, which libusb opens an interface of
/// a composite adapter through. Any fixed GUID does.
const INTERFACE_GUID: &str = "{E0B1F1E0-6C3A-4B2D-9A7E-2F1E0B7E5D01}";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The hardware ID a package for `instance_id` names: the instance id without
/// its instance part (`USB\VID_0BDA&PID_A728\00E04C239987` →
/// `USB\VID_0BDA&PID_A728`), a composite device's `&MI_00` included.
pub fn hardware_id(instance_id: &str) -> Option<String> {
    let (hwid, _) = instance_id.rsplit_once('\\')?;
    let hwid = hwid.to_ascii_uppercase();
    hwid.starts_with("USB\\VID_").then_some(hwid)
}

/// Install WinUSB on the adapter `instance_id`. **Requires elevation.**
pub fn bind(instance_id: &str) -> Result<(), DeployError> {
    let hwid = hardware_id(instance_id)
        .ok_or_else(|| DeployError::WinUsb(format!("{instance_id} isn't a USB device")))?;
    let stem: String = format!("flexinput_bt_{hwid}")
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let (inf, cat) = (format!("{stem}.inf"), format!("{stem}.cat"));
    let staging = StagingDir::create()?;
    let dir = staging.path();
    std::fs::write(dir.join(&inf), inf_text(&hwid, &cat))?;
    let cert = signing::ensure_cert_named(Scope::LocalMachine, CERT_NAME)?;
    let pkg = DriverPackage { inf: &inf, binaries: &[], catalog: &cat, hardware_ids: &[&hwid] };
    signing::sign_driver_package(dir, &pkg, &cert)?;
    for store in ["ROOT", "TrustedPublisher"] {
        signing::add_to_store(Scope::LocalMachine, store, cert.der())?;
    }
    force_install(&hwid, &dir.join(&inf))
}

/// Give the adapter `instance_id` back to Windows: delete the WinUSB package
/// it runs, FlexInput's or Zadig's, which uninstalls it, and Windows installs
/// its own driver again. A device FlexInput holds open keeps WinUSB until it's
/// let go of (replugged, or FlexInput restarted). **Requires elevation.**
pub fn unbind(instance_id: &str) -> Result<(), DeployError> {
    let hwid = hardware_id(instance_id)
        .ok_or_else(|| DeployError::WinUsb(format!("{instance_id} isn't a USB device")))?;
    let packages = winusb_packages_for(&hwid);
    if packages.is_empty() {
        return Err(DeployError::WinUsb(format!(
            "no WinUSB package from FlexInput or Zadig names {hwid}"
        )));
    }
    for published in &packages {
        let _ = Command::new(system32().join("pnputil.exe"))
            .args(["/delete-driver", published, "/uninstall", "/force"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(DeployError::Pnputil)?;
    }
    // Windows reinstalls the next-best driver for a device whose driver was
    // uninstalled once it's re-enumerated; ask for that now.
    let _ = Command::new(system32().join("pnputil.exe"))
        .arg("/scan-devices")
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    if !winusb_packages_for(&hwid).is_empty() {
        return Err(DeployError::WinUsb(format!("the WinUSB package for {hwid} is still installed")));
    }
    if all_flexinput_packages().is_empty() {
        retire_cert();
    }
    Ok(())
}

/// `UpdateDriverForPlugAndPlayDevices` with `INSTALLFLAG_FORCE`: stages the
/// package and installs it on every present device with `hwid`, even where
/// another driver ranks higher.
fn force_install(hwid: &str, inf: &Path) -> Result<(), DeployError> {
    let hwid_w: Vec<u16> = hwid.encode_utf16().chain(std::iter::once(0)).collect();
    let inf_w: Vec<u16> = inf.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    let mut reboot = 0;
    // SAFETY: FFI with NUL-terminated strings and an out-param.
    let ok = unsafe {
        UpdateDriverForPlugAndPlayDevicesW(std::ptr::null_mut(), hwid_w.as_ptr(), inf_w.as_ptr(), INSTALLFLAG_FORCE, &mut reboot)
    };
    if ok == 0 {
        // SAFETY: FFI.
        let err = unsafe { GetLastError() };
        return Err(DeployError::WinUsb(format!("installing WinUSB on {hwid} failed (0x{err:08X})")));
    }
    Ok(())
}

/// The INF: Windows' WinUSB for `hwid`, with a device interface libusb can
/// open, after Microsoft's own WinUSB installation example.
fn inf_text(hwid: &str, catalog: &str) -> String {
    format!(
        "; Written by FlexInput: Windows' own WinUSB driver for one Bluetooth adapter,\r\n\
         ; so FlexInput's own Bluetooth stack can drive it (what Zadig does). Given\r\n\
         ; back from FlexInput's Bluetooth window.\r\n\
         \r\n\
         [Version]\r\n\
         Signature   = \"$Windows NT$\"\r\n\
         Class       = USBDevice\r\n\
         ClassGuid   = {{88BAE032-5A81-49f0-BC3D-A4FF138216D6}}\r\n\
         Provider    = %Provider%\r\n\
         CatalogFile = {catalog}\r\n\
         DriverVer   = 10/29/2026,1.0.0.0\r\n\
         PnpLockdown = 1\r\n\
         \r\n\
         [Manufacturer]\r\n\
         %Provider% = FlexInput,NTamd64\r\n\
         \r\n\
         [FlexInput.NTamd64]\r\n\
         %DeviceName% = Adapter_Install, {hwid}\r\n\
         \r\n\
         [Adapter_Install]\r\n\
         Include = winusb.inf\r\n\
         Needs   = WINUSB.NT\r\n\
         \r\n\
         [Adapter_Install.Services]\r\n\
         Include = winusb.inf\r\n\
         Needs   = WINUSB.NT.Services\r\n\
         \r\n\
         [Adapter_Install.HW]\r\n\
         AddReg = Adapter_AddReg\r\n\
         \r\n\
         [Adapter_AddReg]\r\n\
         HKR,,DeviceInterfaceGUIDs,0x10000,\"{INTERFACE_GUID}\"\r\n\
         \r\n\
         [Strings]\r\n\
         Provider   = \"{PROVIDER}\"\r\n\
         DeviceName = \"Bluetooth adapter for FlexInput\"\r\n"
    )
}

/// Published names (`oemNN.inf`) of the WinUSB packages from FlexInput or
/// Zadig that name `hwid`.
fn winusb_packages_for(hwid: &str) -> Vec<String> {
    published_infs()
        .into_iter()
        .filter(|(_, text)| {
            let lower = text.to_ascii_lowercase();
            let ours = provider_is(&lower, PROVIDER) || provider_is(&lower, ZADIG_PROVIDER);
            ours && lower.contains("winusb") && names_hwid(&lower, hwid)
        })
        .map(|(name, _)| name)
        .collect()
}

/// Whether a lower-cased INF names `hwid`: written out, as FlexInput's are,
/// or as `USB\%DeviceID%` with the rest in `[Strings]`, as Zadig's are.
///
/// ⛔ Zadig's never contain the hardware id in one piece. Looking for it
/// whole found no Zadig package at all, so give-back refused every adapter
/// Zadig had bound.
fn names_hwid(lower_inf: &str, hwid: &str) -> bool {
    let hwid = hwid.to_ascii_lowercase();
    lower_inf.contains(&hwid)
        || hwid
            .strip_prefix("usb\\")
            .is_some_and(|tail| inf_value(lower_inf, "strings", "deviceid").as_deref() == Some(tail))
}

/// A key's value in a section of a lower-cased INF, quotes stripped.
fn inf_value(lower_inf: &str, section: &str, key: &str) -> Option<String> {
    let header = format!("[{section}]");
    let mut inside = false;
    lower_inf.lines().find_map(|l| {
        let l = l.trim();
        if l.starts_with('[') {
            inside = l == header;
            return None;
        }
        let (k, v) = l.split_once('=').filter(|_| inside)?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

/// Every published package from FlexInput's Bluetooth window.
fn all_flexinput_packages() -> Vec<String> {
    published_infs()
        .into_iter()
        .filter(|(_, text)| {
            let lower = text.to_ascii_lowercase();
            provider_is(&lower, PROVIDER) && lower.contains("bluetooth adapter for flexinput")
        })
        .map(|(name, _)| name)
        .collect()
}

/// Whether a lower-cased INF's `[Version]` `Provider` is `provider`: given
/// outright, or through a `%String%` defined in `[Strings]` (`Provider =
/// %ProviderName%`, then `ProviderName = "libwdi"`, as Zadig's are).
fn provider_is(lower_inf: &str, provider: &str) -> bool {
    let Some(mut value) = inf_value(lower_inf, "version", "provider") else { return false };
    if let Some(name) = value.strip_prefix('%').and_then(|v| v.strip_suffix('%')) {
        match inf_value(lower_inf, "strings", name) {
            Some(v) => value = v,
            None => return false,
        }
    }
    value == provider.to_ascii_lowercase()
}

/// `%windir%\INF\oem*.inf`, with their text. INFs are UTF-16 or ANSI.
fn published_infs() -> Vec<(String, String)> {
    let dir = system32().parent().map(|w| w.join("INF")).unwrap_or_else(|| PathBuf::from(r"C:\Windows\INF"));
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_ascii_lowercase();
            if !(lower.starts_with("oem") && lower.ends_with(".inf")) {
                return None;
            }
            let bytes = std::fs::read(e.path()).ok()?;
            Some((name, decode_inf(&bytes)))
        })
        .collect()
}

fn decode_inf(bytes: &[u8]) -> String {
    if let Some(utf16) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = utf16.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Remove [`CERT_NAME`] certs from the trust stores and destroy their keys,
/// once no package needs them. Best-effort.
fn retire_cert() {
    for thumb in signing::cert_thumbprints_named(Scope::LocalMachine, CERT_NAME) {
        for store in ["ROOT", "TrustedPublisher"] {
            let _ = signing::remove_from_store(Scope::LocalMachine, store, &thumb);
        }
        let _ = signing::delete_cert_and_key(Scope::LocalMachine, &thumb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hardware_id_is_the_instance_id_without_its_instance() {
        assert_eq!(hardware_id(r"USB\VID_0BDA&PID_A728\00E04C239987").as_deref(), Some(r"USB\VID_0BDA&PID_A728"));
        assert_eq!(
            hardware_id(r"USB\VID_13D3&PID_3608&MI_00\7&D0B2222&0&0000").as_deref(),
            Some(r"USB\VID_13D3&PID_3608&MI_00")
        );
        assert_eq!(hardware_id(r"BTHENUM\{0000}\7&1"), None);
    }

    #[test]
    fn the_inf_names_the_adapter_and_includes_winusb() {
        let inf = inf_text(r"USB\VID_0BDA&PID_A728", "flexinput_bt.cat");
        assert!(inf.contains("%DeviceName% = Adapter_Install, USB\\VID_0BDA&PID_A728\r\n"), "{inf}");
        assert!(inf.contains("CatalogFile = flexinput_bt.cat\r\n"));
        assert!(inf.contains("Include = winusb.inf\r\nNeeds   = WINUSB.NT\r\n"));
        assert!(inf.contains(&format!("DeviceInterfaceGUIDs,0x10000,\"{INTERFACE_GUID}\"")));
        assert!(provider_is(&inf.to_ascii_lowercase(), PROVIDER));
        assert!(!provider_is(&inf.to_ascii_lowercase(), ZADIG_PROVIDER));
    }

    #[test]
    fn a_zadig_package_is_recognised() {
        // libwdi's WinUSB INF, as Zadig writes it, abridged.
        let zadig = "[Version]\r\nSignature = \"$Windows NT$\"\r\nClass = %ClassName%\r\n\
                     Provider = %ProviderName%\r\n[Strings]\r\nProviderName = \"libwdi\"\r\n\
                     VendorName = \"Realtek\"\r\nDeviceID = \"VID_0BDA&PID_A728\"\r\n";
        assert!(provider_is(&zadig.to_ascii_lowercase(), ZADIG_PROVIDER));
        assert!(!provider_is(&zadig.to_ascii_lowercase(), PROVIDER));
    }

    /// ⛔ libwdi's template, as Zadig writes it: `[Strings]` first, the
    /// provider given outright, and the hardware id only as `USB\%DeviceID%`.
    #[test]
    fn a_zadig_package_names_its_adapter_through_its_strings() {
        let zadig = "[Strings]\r\nDeviceName = \"Bluetooth Radio\"\r\nVendorName = \"Realtek\"\r\n\
                     DeviceID   = \"VID_0BDA&PID_A728\"\r\n\r\n[Version]\r\nSignature   = \"$Windows NT$\"\r\n\
                     Provider    = \"libwdi\"\r\n\r\n[libusbDevice_WinUSB.NTamd64]\r\n\
                     %DeviceName% = USB_Install, USB\\%DeviceID%\r\n\r\n[USB_Install]\r\nInclude = winusb.inf\r\n";
        let lower = zadig.to_ascii_lowercase();
        assert!(provider_is(&lower, ZADIG_PROVIDER));
        assert!(names_hwid(&lower, r"USB\VID_0BDA&PID_A728"));
        assert!(!names_hwid(&lower, r"USB\VID_0BDA&PID_8153"), "never the Ethernet adapter");
        // FlexInput's own name it outright.
        let ours = inf_text(r"USB\VID_0BDA&PID_A728", "x.cat").to_ascii_lowercase();
        assert!(names_hwid(&ours, r"USB\VID_0BDA&PID_A728"));
        assert!(!names_hwid(&ours, r"USB\VID_0BDA&PID_A729"));
    }
}
