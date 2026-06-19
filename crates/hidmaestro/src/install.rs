//! Driver presence detection + installed-INF discovery.
//!
//! Port of the cheap, locale-stable filesystem checks from HIDMaestro's
//! `DriverBuilder.DriverStoreContainsHidMaestro` / `IsDriverInstalled`, plus
//! discovery of the published OEM INF path that `orchestrator::create_device_node`
//! needs (so callers stop hardcoding `oem62.inf`).
//!
//! Detection is read-only and needs no elevation; the actual deploy
//! (cert + sign + pnputil install) lives in [`deploy`](crate::deploy) and runs in
//! the elevated helper.

use std::path::{Path, PathBuf};

/// Returns true if the HIDMaestro driver is present in the Windows DriverStore.
///
/// Mirrors `DriverStoreContainsHidMaestro`: both `hidmaestro.inf_amd64_<hash>`
/// and `hidmaestro_xusb.inf_amd64_<hash>` directories must exist under
/// `%SystemRoot%\System32\DriverStore\FileRepository`. (The XUSB companion INF is
/// part of the standard install even though FlexInput's plain-HID path doesn't
/// use it yet — its presence is the same completeness signal the SDK checks.)
pub fn hidmaestro_available() -> bool {
    let Some(repo) = driverstore_file_repository() else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(&repo) else {
        return false;
    };
    let mut has_main = false;
    let mut has_xusb = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let lower = name.to_ascii_lowercase();
        if !has_main && lower.starts_with("hidmaestro.inf_") {
            has_main = true;
        } else if !has_xusb && lower.starts_with("hidmaestro_xusb.inf_") {
            has_xusb = true;
        }
        if has_main && has_xusb {
            return true;
        }
    }
    has_main && has_xusb
}

/// Discover the published OEM INF path for the HIDMaestro driver
/// (`C:\Windows\INF\oemNN.inf`) by scanning `%SystemRoot%\INF` for an oem*.inf
/// whose text identifies HIDMaestro. Returns the first match, or `None`.
///
/// `orchestrator::create_device_node` passes this to
/// `UpdateDriverForPlugAndPlayDevicesW`; the published INF (not the DriverStore
/// copy) is what newdev expects.
pub fn installed_inf_path() -> Option<PathBuf> {
    let inf_dir = windir().join("INF");
    let entries = std::fs::read_dir(&inf_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy().to_ascii_lowercase();
        if !(name.starts_with("oem") && name.ends_with(".inf")) {
            continue;
        }
        // The plain-HID node binds the MAIN INF; exclude the XUSB companion INF
        // (both contain "hidmaestro", so the generic sniff alone is ambiguous).
        if inf_is_hidmaestro(&path) && !inf_is_hidmaestro_xusb(&path) {
            return Some(path);
        }
    }
    None
}

/// Discover the published OEM INF path for the HIDMaestro **XUSB companion**
/// driver (`hidmaestro_xusb.inf`), used by
/// `orchestrator::create_xusb_companion_node` to bind the XInput companion node.
/// Identified by markers unique to the companion INF (`HMXInput.dll` /
/// `XusbMode`). Returns the first match, or `None`.
pub fn installed_xusb_inf_path() -> Option<PathBuf> {
    let inf_dir = windir().join("INF");
    let entries = std::fs::read_dir(&inf_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy().to_ascii_lowercase();
        if !(name.starts_with("oem") && name.ends_with(".inf")) {
            continue;
        }
        if inf_is_hidmaestro_xusb(&path) {
            return Some(path);
        }
    }
    None
}

/// All published HIDMaestro OEM INF **names** (e.g. `["oem83.inf",
/// "oem84.inf"]`) found under `%SystemRoot%\INF`. These are the names
/// `pnputil /delete-driver <name> /uninstall` expects (not full paths). Used by
/// the force-reinstall path to remove every installed package. Empty when none
/// are present.
pub fn installed_inf_names() -> Vec<String> {
    let inf_dir = windir().join("INF");
    let Ok(entries) = std::fs::read_dir(&inf_dir) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let file = entry.file_name();
        let lower = file.to_string_lossy().to_ascii_lowercase();
        if !(lower.starts_with("oem") && lower.ends_with(".inf")) {
            continue;
        }
        if inf_is_hidmaestro(&path) {
            names.push(file.to_string_lossy().into_owned());
        }
    }
    names
}

/// Cheap content sniff: an INF belongs to HIDMaestro if its (ASCII-ish) text
/// mentions the provider/driver. INF files are small; a substring scan is fine.
fn inf_is_hidmaestro(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    // INF is typically UTF-16LE or ANSI; scan the lossy decode for the marker.
    let text = decode_inf(&bytes);
    let lower = text.to_ascii_lowercase();
    lower.contains("hidmaestro")
}

/// Stronger sniff for the XUSB companion INF specifically. The main and
/// companion INFs both mention "hidmaestro", so we key off markers unique to the
/// companion: its UMDF binary (`HMXInput.dll`) or the `XusbMode` AddReg value.
fn inf_is_hidmaestro_xusb(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let text = decode_inf(&bytes);
    let lower = text.to_ascii_lowercase();
    lower.contains("hidmaestro") && (lower.contains("hmxinput.dll") || lower.contains("xusbmode"))
}

/// Decode an INF's bytes to a String, handling the common UTF-16LE (BOM) and
/// ANSI/UTF-8 cases well enough for a substring sniff.
fn decode_inf(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        // UTF-16LE with BOM.
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn windir() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
}

fn driverstore_file_repository() -> Option<PathBuf> {
    let p = windir().join("System32").join("DriverStore").join("FileRepository");
    if p.is_dir() {
        Some(p)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windir_resolves() {
        // Just exercises the path builders; result depends on the host.
        let _ = windir();
        let _ = driverstore_file_repository();
    }

    #[test]
    fn decode_handles_utf16_bom() {
        // "HIDMaestro" UTF-16LE with BOM.
        let mut bytes = vec![0xFF, 0xFE];
        for ch in "xHIDMaestrox".encode_utf16() {
            bytes.extend_from_slice(&ch.to_le_bytes());
        }
        assert!(decode_inf(&bytes).to_ascii_lowercase().contains("hidmaestro"));
    }
}
