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

/// Installed state of the HIDMaestro driver package in the Windows DriverStore.
///
/// The distinction that matters is [`DriverState::Partial`]: exactly one of the
/// two packages present. A plain present/absent boolean reports that state as
/// "not installed", which silently mis-reports a broken machine as a clean one
/// (uninstall claims success; reinstall can never verify). Callers that act on
/// the result must handle `Partial` explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverState {
    /// Neither package present — a clean machine.
    Missing,
    /// Exactly one package present. The other was removed or failed to install,
    /// leaving a published-but-unbacked INF behind.
    Partial { has_main: bool, has_xusb: bool },
    /// Both packages present.
    Complete,
}

/// Classify the HIDMaestro driver packages present in the Windows DriverStore.
///
/// Mirrors `DriverStoreContainsHidMaestro`: looks for `hidmaestro.inf_amd64_<hash>`
/// and `hidmaestro_xusb.inf_amd64_<hash>` directories under
/// `%SystemRoot%\System32\DriverStore\FileRepository`. (The XUSB companion INF is
/// part of the standard install even though FlexInput's plain-HID path doesn't
/// use it yet — its presence is the same completeness signal the SDK checks.)
pub fn driver_state() -> DriverState {
    let Some(repo) = driverstore_file_repository() else {
        return DriverState::Missing;
    };
    driver_state_in(&repo)
}

/// [`driver_state`] against an arbitrary FileRepository directory, so the scan
/// can be tested against fixtures instead of the live DriverStore.
fn driver_state_in(repo: &Path) -> DriverState {
    let Ok(entries) = std::fs::read_dir(repo) else {
        return DriverState::Missing;
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
            return DriverState::Complete;
        }
    }
    match (has_main, has_xusb) {
        (false, false) => DriverState::Missing,
        (true, true) => DriverState::Complete,
        (has_main, has_xusb) => DriverState::Partial { has_main, has_xusb },
    }
}

/// Returns true if the HIDMaestro driver is fully present in the Windows
/// DriverStore. Thin wrapper over [`driver_state`]; note that a `Partial`
/// install reads as `false` here, so anything that needs to tell "half
/// installed" apart from "not installed" must call [`driver_state`] directly.
pub fn hidmaestro_available() -> bool {
    matches!(driver_state(), DriverState::Complete)
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
///
/// **Both** the main INF and the XUSB companion are returned, with the
/// **companion first**. Ordering is load-bearing: the companion reads the main
/// driver's per-instance shared memory, so removing the main package first
/// strands the companion — its INF stays published and bound while its backing
/// `HMXInput.dll` is gone, and WUDFHost then faults with `c0000005` on every
/// load attempt.
pub fn installed_inf_names() -> Vec<String> {
    inf_names_in(&windir().join("INF"))
}

/// [`installed_inf_names`] against an arbitrary INF directory, so the scan can be
/// tested against fixtures instead of the live `%SystemRoot%\INF`.
fn inf_names_in(inf_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(inf_dir) else {
        return Vec::new();
    };
    // Companion INFs first, then main INFs — see the ordering note above.
    let mut xusb = Vec::new();
    let mut main = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let file = entry.file_name();
        let lower = file.to_string_lossy().to_ascii_lowercase();
        if !(lower.starts_with("oem") && lower.ends_with(".inf")) {
            continue;
        }
        if !inf_is_hidmaestro(&path) {
            continue;
        }
        if inf_is_hidmaestro_xusb(&path) {
            xusb.push(file.to_string_lossy().into_owned());
        } else {
            main.push(file.to_string_lossy().into_owned());
        }
    }
    xusb.sort();
    main.sort();
    xusb.extend(main);
    xusb
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

    /// Unique temp dir for a fixture; avoids a dev-dependency on `tempfile`.
    fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "flexinput_install_test_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        dir
    }

    const MAIN_INF: &str = "[Version]\nProvider=HIDMaestro\nCatalogFile=hidmaestro.cat\n";
    const XUSB_INF: &str =
        "[Version]\nProvider=HIDMaestro\n[UMDriverCopy]\nHMXInput.dll\n[HW]\nXusbMode\n";

    #[test]
    fn inf_names_lists_both_with_companion_first() {
        let dir = fixture_dir("both");
        // Written so the MAIN inf sorts first by filename — the companion must
        // still come out first, proving the order is by role, not by name.
        std::fs::write(dir.join("oem10.inf"), MAIN_INF).unwrap();
        std::fs::write(dir.join("oem99.inf"), XUSB_INF).unwrap();
        std::fs::write(dir.join("oem50.inf"), "[Version]\nProvider=Unrelated\n").unwrap();

        let names = inf_names_in(&dir);
        assert_eq!(
            names,
            vec!["oem99.inf".to_string(), "oem10.inf".to_string()],
            "companion must be removed before the main package it depends on"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inf_names_includes_lone_companion() {
        // The exact state that stranded the companion: the old filter returned
        // only main INFs, so an orphaned companion was never removed.
        let dir = fixture_dir("lone_xusb");
        std::fs::write(dir.join("oem101.inf"), XUSB_INF).unwrap();

        assert_eq!(inf_names_in(&dir), vec!["oem101.inf".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn driver_state_detects_partial_xusb_only() {
        // Matches the broken machine: companion in the DriverStore, main gone.
        let dir = fixture_dir("store_xusb_only");
        std::fs::create_dir_all(dir.join("hidmaestro_xusb.inf_amd64_e069e15e2f1d9e77")).unwrap();

        assert_eq!(
            driver_state_in(&dir),
            DriverState::Partial { has_main: false, has_xusb: true }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn driver_state_detects_partial_main_only() {
        let dir = fixture_dir("store_main_only");
        std::fs::create_dir_all(dir.join("hidmaestro.inf_amd64_abc123")).unwrap();

        assert_eq!(
            driver_state_in(&dir),
            DriverState::Partial { has_main: true, has_xusb: false }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn driver_state_complete_and_missing() {
        let complete = fixture_dir("store_complete");
        std::fs::create_dir_all(complete.join("hidmaestro.inf_amd64_abc123")).unwrap();
        std::fs::create_dir_all(complete.join("hidmaestro_xusb.inf_amd64_def456")).unwrap();
        assert_eq!(driver_state_in(&complete), DriverState::Complete);
        let _ = std::fs::remove_dir_all(&complete);

        let empty = fixture_dir("store_empty");
        std::fs::create_dir_all(empty.join("some_other_driver.inf_amd64_x")).unwrap();
        assert_eq!(driver_state_in(&empty), DriverState::Missing);
        let _ = std::fs::remove_dir_all(&empty);
    }
}
