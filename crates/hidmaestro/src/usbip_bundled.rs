//! usbip-win2's signed x64 installer, shipped inside FlexInput so a composite
//! persona needs nothing for a user to go find. Run elevated, silently, by the
//! helper the first time one is turned on (`usbip_install`). Unmodified,
//! BSD-2-Clause; see `usbip-win2/README.md`.

use sha2::{Digest, Sha256};

pub const VERSION: &str = "0.9.8.1";
pub const INSTALLER_NAME: &str = "USBip-0.9.8.1-x64.exe";
/// The release asset's SHA-256, as published with v.0.9.8.1.
pub const INSTALLER_SHA256: &str = "38cad6d4432b52d5bb9409d9ad03b72fdffc4ada4cd3a48fbeca1a2752a8518a";
pub static INSTALLER: &[u8] = include_bytes!("../usbip-win2/USBip-0.9.8.1-x64.exe");
/// The licence the installer is redistributed under; it ships beside it.
pub const LICENSE: &str = include_str!("../usbip-win2/LICENSE.txt");

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `bytes` are the pinned installer.
pub fn is_pinned_installer(bytes: &[u8]) -> bool {
    sha256_hex(bytes) == INSTALLER_SHA256
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_installer_is_the_pinned_release() {
        assert!(is_pinned_installer(INSTALLER));
        assert!(LICENSE.starts_with("BSD 2-Clause License"));
    }
}
