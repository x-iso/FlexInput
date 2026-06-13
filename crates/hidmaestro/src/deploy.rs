//! Driver deployment — trust the signing cert + install the INFs (elevated).
//!
//! Lighter-weight alternative to HIDMaestro's `DriverBuilder.FullDeploy`: rather
//! than embed signtool/inf2cat and sign on the user's machine, FlexInput vendors
//! the **already-signed** driver package (DLLs + INFs + `.cat` catalogs, MIT,
//! ~247 KB; see `crates/hidmaestro/driver/`) plus the signer's public cert. At
//! runtime the elevated helper:
//!   1. adds the cert to `Root` + `TrustedPublisher` (so Windows trusts the
//!      catalog-signed package), then
//!   2. runs the OS-builtin `pnputil /add-driver <inf> /install` for each INF.
//!
//! No external signing toolchain is redistributed. Everything here requires
//! elevation and is intended to run inside the helper process.
//!
//! **Distribution note:** the vendored binaries are HIDMaestro's, signed with
//! `CN=HIDMaestroTestCert`. For a shipping product you would re-sign the driver
//! package with your own cert at *build* time and swap the vendored cert; the
//! runtime logic here is unchanged.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use crate::install::hidmaestro_available;

/// The vendored, pre-signed driver payload, embedded at compile time so the
/// helper can stage it to a temp dir regardless of cwd.
pub mod payload {
    pub const HIDMAESTRO_DLL: &[u8] = include_bytes!("../driver/HIDMaestro.dll");
    pub const HIDMAESTRO_INF: &[u8] = include_bytes!("../driver/hidmaestro.inf");
    pub const HIDMAESTRO_CAT: &[u8] = include_bytes!("../driver/hidmaestro.cat");
    pub const HMXINPUT_DLL: &[u8] = include_bytes!("../driver/HMXInput.dll");
    pub const HIDMAESTRO_XUSB_INF: &[u8] = include_bytes!("../driver/hidmaestro_xusb.inf");
    pub const HIDMAESTRO_XUSB_CAT: &[u8] = include_bytes!("../driver/hidmaestro_xusb.cat");
    pub const SIGNER_CERT: &[u8] = include_bytes!("../driver/HIDMaestroTestCert.cer");
}

#[derive(Debug)]
pub enum DeployError {
    Io(std::io::Error),
    /// Adding the cert to a system store failed (store name + GetLastError).
    CertStore(&'static str, u32),
    /// `pnputil /add-driver` ran but the package isn't in the DriverStore.
    InstallUnverified,
    /// pnputil could not be launched.
    Pnputil(std::io::Error),
}

impl std::fmt::Display for DeployError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeployError::Io(e) => write!(f, "io error: {e}"),
            DeployError::CertStore(s, e) => write!(f, "cert store '{s}' failed (err {e})"),
            DeployError::InstallUnverified => {
                write!(f, "pnputil ran but HIDMaestro not present in DriverStore")
            }
            DeployError::Pnputil(e) => write!(f, "could not run pnputil: {e}"),
        }
    }
}
impl std::error::Error for DeployError {}

impl From<std::io::Error> for DeployError {
    fn from(e: std::io::Error) -> Self {
        DeployError::Io(e)
    }
}

/// Idempotent full deploy: if the driver is already in the store, no-op;
/// otherwise trust the cert, stage the payload, and install both INFs. Returns
/// `Ok(true)` if a fresh install happened, `Ok(false)` if already present.
/// **Requires elevation.**
pub fn ensure_driver_installed() -> Result<bool, DeployError> {
    if hidmaestro_available() {
        return Ok(false);
    }
    trust_signer_cert()?;
    let dir = stage_payload()?;
    install_inf(&dir.join("hidmaestro.inf"))?;
    install_inf(&dir.join("hidmaestro_xusb.inf"))?;
    if !hidmaestro_available() {
        return Err(DeployError::InstallUnverified);
    }
    Ok(true)
}

/// Force a clean reinstall: remove every installed HIDMaestro driver package
/// from the DriverStore, then run a fresh install. Unlike
/// [`ensure_driver_installed`] this does NOT short-circuit when the driver is
/// present — it's the "Reinstall drivers" path for recovering from a corrupt or
/// mismatched package. **Requires elevation.** Callers must tear down any live
/// virtual device nodes first (a bound driver can refuse removal).
///
/// Returns `Ok(())` on a verified fresh install. The uninstall step is
/// best-effort (a package that's already gone, or pinned by a node we missed, is
/// logged-but-not-fatal); the post-install DriverStore check is authoritative.
pub fn reinstall_driver_force() -> Result<(), DeployError> {
    uninstall_all_hidmaestro_packages();
    // Re-add the cert + INFs unconditionally (the cert add is idempotent).
    trust_signer_cert()?;
    let dir = stage_payload()?;
    install_inf(&dir.join("hidmaestro.inf"))?;
    install_inf(&dir.join("hidmaestro_xusb.inf"))?;
    if !hidmaestro_available() {
        return Err(DeployError::InstallUnverified);
    }
    Ok(())
}

/// `pnputil /delete-driver <oemNN.inf> /uninstall /force` for every published
/// HIDMaestro package. Best-effort: discovers the published `oemNN.inf` names by
/// scanning `%SystemRoot%\INF` (same content sniff as `installed_inf_path`) and
/// deletes each. Errors are swallowed — a missing/locked package shouldn't abort
/// the reinstall (the post-install verify catches a real failure).
fn uninstall_all_hidmaestro_packages() {
    let pnputil = system32().join("pnputil.exe");
    for inf in crate::install::installed_inf_names() {
        let _ = std::process::Command::new(&pnputil)
            .arg("/delete-driver")
            .arg(&inf)
            .arg("/uninstall")
            .arg("/force")
            .status();
    }
}

/// Stage the embedded driver payload into a temp dir and return it. pnputil
/// needs the INF, its referenced DLL, and the `.cat` to sit together.
fn stage_payload() -> Result<PathBuf, DeployError> {
    let dir = std::env::temp_dir().join("flexinput_hidmaestro_driver");
    std::fs::create_dir_all(&dir)?;
    let write = |name: &str, bytes: &[u8]| -> Result<(), DeployError> {
        std::fs::write(dir.join(name), bytes).map_err(DeployError::Io)
    };
    write("HIDMaestro.dll", payload::HIDMAESTRO_DLL)?;
    write("hidmaestro.inf", payload::HIDMAESTRO_INF)?;
    write("hidmaestro.cat", payload::HIDMAESTRO_CAT)?;
    write("HMXInput.dll", payload::HMXINPUT_DLL)?;
    write("hidmaestro_xusb.inf", payload::HIDMAESTRO_XUSB_INF)?;
    write("hidmaestro_xusb.cat", payload::HIDMAESTRO_XUSB_CAT)?;
    Ok(dir)
}

/// `pnputil /add-driver <inf> /install`. Success is verified by the caller via
/// the DriverStore check (pnputil's own rc/output is locale-keyed and
/// unreliable — matches the C# note in `InstallDrivers`).
fn install_inf(inf: &Path) -> Result<(), DeployError> {
    let pnputil = system32().join("pnputil.exe");
    let status = std::process::Command::new(pnputil)
        .arg("/add-driver")
        .arg(inf)
        .arg("/install")
        .status()
        .map_err(DeployError::Pnputil)?;
    // Don't treat a non-zero rc as fatal; the DriverStore post-check is
    // authoritative. Log-worthy only.
    let _ = status;
    Ok(())
}

/// Add the vendored signer cert to LocalMachine `Root` + `TrustedPublisher` so
/// Windows accepts the catalog-signed driver package. Idempotent (adding an
/// existing cert is a no-op / benign).
fn trust_signer_cert() -> Result<(), DeployError> {
    add_cert_to_store("ROOT", payload::SIGNER_CERT)?;
    add_cert_to_store("TrustedPublisher", payload::SIGNER_CERT)?;
    Ok(())
}

// ── Win32 cert-store FFI (crypt32) ──────────────────────────────────────────
const CERT_STORE_PROV_SYSTEM_W: usize = 10;
const CERT_SYSTEM_STORE_LOCAL_MACHINE: u32 = 2 << 16;
const X509_ASN_ENCODING: u32 = 0x0000_0001;
const PKCS_7_ASN_ENCODING: u32 = 0x0001_0000;
const CERT_STORE_ADD_REPLACE_EXISTING: u32 = 3;

#[link(name = "crypt32")]
extern "system" {
    fn CertOpenStore(
        store_provider: usize,
        encoding: u32,
        crypt_prov: *mut c_void,
        flags: u32,
        para: *const u16,
    ) -> *mut c_void;
    fn CertAddEncodedCertificateToStore(
        cert_store: *mut c_void,
        encoding: u32,
        cert_encoded: *const u8,
        cert_encoded_len: u32,
        add_disposition: u32,
        cert_context: *mut *const c_void,
    ) -> i32;
    fn CertCloseStore(cert_store: *mut c_void, flags: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
}

fn add_cert_to_store(store_name: &'static str, cert_der: &[u8]) -> Result<(), DeployError> {
    let wname: Vec<u16> = store_name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let store = CertOpenStore(
            CERT_STORE_PROV_SYSTEM_W,
            0,
            std::ptr::null_mut(),
            CERT_SYSTEM_STORE_LOCAL_MACHINE,
            wname.as_ptr(),
        );
        if store.is_null() {
            return Err(DeployError::CertStore(store_name, GetLastError()));
        }
        let ok = CertAddEncodedCertificateToStore(
            store,
            X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
            cert_der.as_ptr(),
            cert_der.len() as u32,
            CERT_STORE_ADD_REPLACE_EXISTING,
            std::ptr::null_mut(),
        );
        let err = if ok == 0 { GetLastError() } else { 0 };
        CertCloseStore(store, 0);
        if ok == 0 {
            return Err(DeployError::CertStore(store_name, err));
        }
    }
    Ok(())
}

fn system32() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_embedded_and_nonempty() {
        // The driver files must be present at compile time.
        assert!(payload::HIDMAESTRO_DLL.len() > 10_000);
        assert!(payload::HIDMAESTRO_INF.windows(4).any(|w| w == b".dll" || w == b".DLL")
            || !payload::HIDMAESTRO_INF.is_empty());
        assert!(payload::SIGNER_CERT.len() > 100);
        assert!(payload::HMXINPUT_DLL.len() > 10_000);
    }
}
