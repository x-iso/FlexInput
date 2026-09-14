//! Driver deployment: sign the driver packages on this machine, trust the
//! signer, install the INFs. Everything here needs elevation and runs in the
//! helper process.
//!
//! FlexInput vendors HIDMaestro's driver binaries and INFs (MIT; see
//! `crates/hidmaestro/driver/`) but no catalogs and no certificates. To install,
//! the helper:
//!   1. stages the payload in a fresh directory only Administrators and SYSTEM
//!      can write to ([`StagingDir`]),
//!   2. signs both packages with this machine's own cert ([`crate::signing`]),
//!   3. trusts that cert in `Root` + `TrustedPublisher`, and
//!   4. runs the OS-builtin `pnputil /add-driver <inf> /install` for each INF.
//!
//! Each machine trusts only a key it generated itself, and that key never leaves
//! it. Earlier builds instead shipped catalogs pre-signed on one build machine
//! and trusted those certs on every install.
//!
//! An installed driver is only ever replaced deliberately: at helper startup,
//! [`reconcile_installed_driver`] reinstalls when [`installed_driver`] finds a
//! different driver version than this build vendors, or catalogs signed by
//! anything but this machine's cert, then removes the legacy certs.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{GetLastError, LocalFree, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;

use crate::install::{driver_state, inf_driver_version, installed_inf_names, published_packages, DriverState};
use crate::orchestrator::registry;
use crate::signing::{self, DriverPackage, Scope, SignError, Thumbprint};

/// The vendored driver payload (HIDMaestro v1.7.3, unmodified), embedded at
/// compile time so the helper can stage it regardless of cwd. The binaries ship
/// unsigned; they're signed and catalogued on the target machine. Each INF's
/// `DriverVer` is what [`installed_driver`] compares an install against, so a
/// driver update must change it.
pub mod payload {
    pub const HIDMAESTRO_DLL: &[u8] = include_bytes!("../driver/HIDMaestro.dll");
    pub const HIDMAESTRO_INF: &[u8] = include_bytes!("../driver/hidmaestro.inf");
    /// The XUSB companion.
    pub const HMXINPUT_DLL: &[u8] = include_bytes!("../driver/HMXInput.dll");
    pub const HIDMAESTRO_XUSB_INF: &[u8] = include_bytes!("../driver/hidmaestro_xusb.inf");
}

/// The main HID driver package. `hardware_ids` must track the INF's models
/// section; a test checks it.
pub const MAIN_PACKAGE: DriverPackage<'static> = DriverPackage {
    inf: "hidmaestro.inf",
    binaries: &["HIDMaestro.dll"],
    catalog: "hidmaestro.cat",
    hardware_ids: &[r"root\HIDMaestro"],
};

/// The XUSB companion package.
pub const XUSB_PACKAGE: DriverPackage<'static> = DriverPackage {
    inf: "hidmaestro_xusb.inf",
    binaries: &["HMXInput.dll"],
    catalog: "hidmaestro_xusb.cat",
    hardware_ids: &[
        r"root\VID_045E&PID_028E&XI_00",
        r"root\VID_045E&PID_0291&XI_00",
        r"root\VID_045E&PID_0719&XI_00",
        r"root\HIDMaestroXUSB",
    ],
};

/// The certs that signed the catalogs earlier builds vendored, and that those
/// builds trusted in `Root` + `TrustedPublisher` on every install:
/// `CN=HIDMaestroTestCert` and `CN=FlexInput HIDMaestro Driver`. Matched by exact
/// thumbprint, never by name: HIDMaestro's own SDK generates a per-machine
/// `CN=HIDMaestroTestCert` that isn't ours to remove.
const LEGACY_SIGNERS: [Thumbprint; 2] = [
    thumbprint("4353B1700E42A8484DCD16671EC00A320FD53908"),
    thumbprint("2499E17BDE28C969B426BA81BB3E963CD9532D68"),
];

/// Catalog-database folder for driver catalogs. Installing `oemNN.inf` places
/// its signed catalog here as `oemNN.cat`, and that copy is what Windows checks
/// when it binds the driver to a device.
const DRIVER_CATROOT: &str = r"System32\CatRoot\{F750E6C3-38EE-11D1-85E5-00C04FC295EE}";

/// Where the helper records automatic reinstall attempts: see
/// [`reconcile_installed_driver`].
const RECONCILE_KEY: &str = r"SOFTWARE\FlexInput\Driver";
const RECONCILE_ATTEMPTS: &str = "ReinstallAttempts";
/// Automatic reinstalls give up after this many failed attempts, so a machine
/// where one can't succeed doesn't lose its virtual devices on every launch.
/// "Reinstall drivers" still works, and a success resets the count.
const MAX_RECONCILE_ATTEMPTS: u32 = 3;

#[derive(Debug)]
pub enum DeployError {
    Io(std::io::Error),
    /// Creating the protected staging directory failed (GetLastError).
    Staging(u32),
    /// Creating the signing cert, or signing or trusting a package, failed.
    Sign(SignError),
    /// `pnputil /add-driver` ran but the package isn't in the DriverStore.
    InstallUnverified,
    /// Exactly one of the two packages is present in the DriverStore. A
    /// published-but-unbacked companion INF makes WUDFHost fault (`c0000005`)
    /// on every load, so this is reported distinctly from
    /// [`DeployError::InstallUnverified`] rather than as "not installed".
    PartialInstall { has_main: bool, has_xusb: bool },
    /// pnputil could not be launched.
    Pnputil(std::io::Error),
}

impl std::fmt::Display for DeployError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeployError::Io(e) => write!(f, "io error: {e}"),
            DeployError::Staging(e) => write!(f, "could not create driver staging directory (err {e})"),
            DeployError::Sign(e) => write!(f, "driver signing failed: {e}"),
            DeployError::InstallUnverified => {
                write!(f, "pnputil ran but HIDMaestro not present in DriverStore")
            }
            DeployError::PartialInstall { has_main, has_xusb } => {
                let present = if *has_main { "hidmaestro.inf" } else { "hidmaestro_xusb.inf" };
                let missing = if *has_main { "hidmaestro_xusb.inf" } else { "hidmaestro.inf" };
                let _ = has_xusb;
                write!(
                    f,
                    "HIDMaestro is half-installed: {present} is in the DriverStore but \
                     {missing} is not. Run 'Reinstall drivers' to remove both and \
                     reinstall cleanly."
                )
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

impl From<SignError> for DeployError {
    fn from(e: SignError) -> Self {
        DeployError::Sign(e)
    }
}

/// Idempotent full deploy: if the driver is already in the store, no-op;
/// otherwise sign, trust and install both packages. Returns `Ok(true)` if a
/// fresh install happened, `Ok(false)` if already present. **Requires elevation.**
///
/// Deliberately never replaces an installed driver, even an outdated or
/// foreign-signed one: this runs on every device create, where tearing devices
/// down isn't acceptable. The helper does that once at startup instead
/// ([`reconcile_installed_driver`]).
pub fn ensure_driver_installed() -> Result<bool, DeployError> {
    match driver_state() {
        DriverState::Complete => return Ok(false),
        // Installing over a half-installed state leaves the stranded package in
        // place; the caller must go through `reinstall_driver_force` (which
        // removes both first) rather than silently stacking on top of it.
        DriverState::Partial { has_main, has_xusb } => {
            return Err(DeployError::PartialInstall { has_main, has_xusb })
        }
        DriverState::Missing => {}
    }
    install_signed_packages()?;
    Ok(true)
}

/// Force a clean reinstall: remove every installed HIDMaestro driver package
/// from the DriverStore, then run a fresh install. Unlike
/// [`ensure_driver_installed`] this does NOT short-circuit when the driver is
/// present — it's the "Reinstall drivers" path for recovering from a corrupt or
/// mismatched package, and how an install is upgraded or re-signed. **Requires
/// elevation.**
/// Callers must tear down any live virtual device nodes first (a bound driver can
/// refuse removal).
///
/// Removal has to come first even when only the signature changes: the
/// DriverStore identifies a package by its INF, so re-adding the same INF with a
/// newly signed catalog would be taken as the package already present.
///
/// Returns `Ok(())` on a verified fresh install. The uninstall step is
/// best-effort (a package that's already gone, or pinned by a node we missed, is
/// logged-but-not-fatal); the post-install DriverStore check is authoritative.
pub fn reinstall_driver_force() -> Result<(), DeployError> {
    uninstall_all_hidmaestro_packages();
    install_signed_packages()
}

/// Remove the HIDMaestro driver entirely: delete every installed package from the
/// DriverStore, then withdraw the trust FlexInput added — this machine's signing
/// cert (and its private key) and any legacy certs. Nothing is reinstalled; a
/// later install creates a new cert. **Requires elevation.** Callers must tear
/// down any live virtual device nodes first (a bound driver can refuse removal).
///
/// Returns `Ok(())` once no HIDMaestro package remains in the DriverStore;
/// `Err(InstallUnverified)` if a package is still present (e.g. pinned by a node we
/// missed). Trust is only withdrawn after that check passes: a package still in
/// the store would otherwise stop validating. Per-`pnputil` errors are
/// best-effort/logged; the DriverStore check is authoritative.
pub fn uninstall_driver() -> Result<(), DeployError> {
    uninstall_all_hidmaestro_packages();
    // `Missing` is the only success here. Checking `!hidmaestro_available()`
    // instead would report a half-removed state as a clean uninstall — the
    // exact failure that strands the companion INF and crashes WUDFHost.
    match driver_state() {
        DriverState::Missing => {
            retire_signing_certs(None);
            remove_legacy_trust();
            Ok(())
        }
        DriverState::Partial { has_main, has_xusb } => {
            Err(DeployError::PartialInstall { has_main, has_xusb })
        }
        DriverState::Complete => Err(DeployError::InstallUnverified),
    }
}

/// How the HIDMaestro packages Windows has installed compare with the ones this
/// build ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledDriver {
    /// No HIDMaestro package is installed.
    NotInstalled,
    /// Same driver version as this build's payload, and every catalog is signed by
    /// this machine's current cert. Nothing to do.
    Current,
    /// A different driver version than this build ships — typically an older
    /// one, after FlexInput updated its vendored HIDMaestro.
    OtherVersion,
    /// The right version, but a catalog is signed by something other than this
    /// machine's current cert: an earlier build's vendored cert, or a machine cert
    /// that has since been replaced.
    ForeignSigner,
    /// Half-installed, or the installed INFs or catalogs couldn't be read. Left
    /// alone rather than guessed at: "Reinstall drivers" resolves it.
    Indeterminate,
}

impl InstalledDriver {
    /// Whether the helper should replace the installed packages.
    pub fn needs_reinstall(self) -> bool {
        matches!(self, InstalledDriver::OtherVersion | InstalledDriver::ForeignSigner)
    }
}

/// Classify the installed packages. Version is checked first, against each
/// published INF's `DriverVer`; then the signer of the catalog Windows validates
/// against (see [`DRIVER_CATROOT`]). Read-only; never creates a cert.
pub fn installed_driver() -> InstalledDriver {
    match driver_state() {
        DriverState::Missing => return InstalledDriver::NotInstalled,
        DriverState::Partial { .. } => return InstalledDriver::Indeterminate,
        DriverState::Complete => {}
    }
    let published = published_packages();
    if published.is_empty() {
        return InstalledDriver::Indeterminate;
    }
    for pkg in &published {
        let payload = if pkg.is_xusb { payload::HIDMAESTRO_XUSB_INF } else { payload::HIDMAESTRO_INF };
        // A version this parser can't read is left alone rather than treated as a
        // mismatch: guessing wrong would reinstall on every launch.
        let (Some(installed), Some(shipped)) = (&pkg.version, inf_driver_version(payload)) else {
            return InstalledDriver::Indeterminate;
        };
        if *installed != shipped {
            return InstalledDriver::OtherVersion;
        }
    }
    let ours = signing::find_signing_cert(Scope::LocalMachine).and_then(|c| c.thumbprint().ok());
    let catroot = windows_dir().join(DRIVER_CATROOT);
    for pkg in &published {
        let catalog = catroot.join(Path::new(&pkg.name).with_extension("cat"));
        if !catalog.exists() {
            return InstalledDriver::Indeterminate;
        }
        match signing::signer_thumbprint(&catalog) {
            Ok(Some(signer)) if Some(signer) == ours => {}
            _ => return InstalledDriver::ForeignSigner,
        }
    }
    InstalledDriver::Current
}

/// What [`reconcile_installed_driver`] did.
#[derive(Debug)]
pub enum Reconcile {
    /// Already current, or nothing installed. Any leftover legacy certs were
    /// removed; the count says how many store entries went.
    NotNeeded { legacy_certs_removed: usize },
    /// The install was out of date or foreign-signed (`from`) and has been
    /// replaced with this build's packages, signed by this machine.
    Reinstalled { from: InstalledDriver },
    /// A reinstall was needed but automatic attempts are exhausted.
    GaveUp { from: InstalledDriver },
    /// Half-installed or unreadable; left for "Reinstall drivers".
    Skipped,
    Failed { from: InstalledDriver, error: DeployError },
}

/// Bring the installed driver in line with this build: the vendored version,
/// signed by this machine's own cert. The helper calls this at startup, before it
/// accepts any request, so no device of this session exists yet. **Requires
/// elevation.**
///
/// That covers both an update to the vendored HIDMaestro — without this an
/// installed driver would never be replaced, because [`ensure_driver_installed`]
/// only installs when nothing is present — and the move off earlier builds'
/// shared signing certs.
///
/// When a reinstall is needed, `clear_devices` runs first and should block until
/// no HIDMaestro device node remains: packages can't be replaced cleanly while
/// nodes are bound to them, and a driver host still holding the old DLL can make
/// the old package's removal fail. Virtual devices, persisted ones included, are
/// recreated afterwards. That happens once per change; later starts find the
/// install current and only confirm no legacy cert is still trusted.
///
/// Failed attempts are counted in the registry, and after
/// [`MAX_RECONCILE_ATTEMPTS`] the helper stops trying, so a machine where the
/// reinstall can't succeed doesn't lose its devices on every launch.
pub fn reconcile_installed_driver(clear_devices: impl FnOnce()) -> Reconcile {
    let from = installed_driver();
    match from {
        InstalledDriver::NotInstalled | InstalledDriver::Current => {
            // A manual reinstall may have succeeded after automatic attempts ran
            // out; clear the count so the next needed reinstall (a later driver
            // update, a replaced cert) starts fresh instead of giving up at once.
            if registry::read_dword(registry::HKLM, RECONCILE_KEY, RECONCILE_ATTEMPTS).unwrap_or(0) != 0 {
                let _ = registry::write_dword(registry::HKLM, RECONCILE_KEY, RECONCILE_ATTEMPTS, 0);
            }
            return Reconcile::NotNeeded { legacy_certs_removed: remove_legacy_trust() };
        }
        InstalledDriver::Indeterminate => return Reconcile::Skipped,
        InstalledDriver::OtherVersion | InstalledDriver::ForeignSigner => {}
    }
    let attempts = registry::read_dword(registry::HKLM, RECONCILE_KEY, RECONCILE_ATTEMPTS).unwrap_or(0);
    if attempts >= MAX_RECONCILE_ATTEMPTS {
        return Reconcile::GaveUp { from };
    }
    let _ = registry::write_dword(registry::HKLM, RECONCILE_KEY, RECONCILE_ATTEMPTS, attempts + 1);
    clear_devices();
    match reinstall_driver_force() {
        Ok(()) if installed_driver() == InstalledDriver::Current => {
            let _ = registry::write_dword(registry::HKLM, RECONCILE_KEY, RECONCILE_ATTEMPTS, 0);
            Reconcile::Reinstalled { from }
        }
        Ok(()) => Reconcile::Failed { from, error: DeployError::InstallUnverified },
        Err(error) => Reconcile::Failed { from, error },
    }
}

/// Stage, sign, trust and install both packages, then verify. On success, retire
/// every other FlexInput signing cert and any legacy cert: nothing installed
/// depends on them any more.
fn install_signed_packages() -> Result<(), DeployError> {
    let staging = StagingDir::create()?;
    let dir = staging.path();
    for (name, bytes) in [
        (MAIN_PACKAGE.binaries[0], payload::HIDMAESTRO_DLL),
        (MAIN_PACKAGE.inf, payload::HIDMAESTRO_INF),
        (XUSB_PACKAGE.binaries[0], payload::HMXINPUT_DLL),
        (XUSB_PACKAGE.inf, payload::HIDMAESTRO_XUSB_INF),
    ] {
        std::fs::write(dir.join(name), bytes)?;
    }

    let cert = signing::ensure_signing_cert(Scope::LocalMachine)?;
    // Read before anything is signed: retiring the other certs later must know
    // for certain which one to keep, or it could remove the signer just used.
    let thumb = cert.thumbprint()?;
    signing::sign_driver_package(dir, &MAIN_PACKAGE, &cert)?;
    signing::sign_driver_package(dir, &XUSB_PACKAGE, &cert)?;
    for store in ["ROOT", "TrustedPublisher"] {
        signing::add_to_store(Scope::LocalMachine, store, cert.der())?;
    }

    install_inf(&dir.join(MAIN_PACKAGE.inf))?;
    install_inf(&dir.join(XUSB_PACKAGE.inf))?;
    match driver_state() {
        DriverState::Complete => {}
        DriverState::Partial { has_main, has_xusb } => {
            return Err(DeployError::PartialInstall { has_main, has_xusb })
        }
        DriverState::Missing => return Err(DeployError::InstallUnverified),
    }

    retire_signing_certs(Some(thumb));
    remove_legacy_trust();
    Ok(())
}

/// Remove FlexInput signing certs from `Root`, `TrustedPublisher` and `My`, and
/// destroy their keys — all of them, or all but `keep`. Best-effort.
fn retire_signing_certs(keep: Option<Thumbprint>) {
    for thumb in signing::signing_cert_thumbprints(Scope::LocalMachine) {
        if Some(thumb) == keep {
            continue;
        }
        for store in ["ROOT", "TrustedPublisher"] {
            let _ = signing::remove_from_store(Scope::LocalMachine, store, &thumb);
        }
        let _ = signing::delete_cert_and_key(Scope::LocalMachine, &thumb);
    }
}

/// Remove the [`LEGACY_SIGNERS`] from `LocalMachine\Root` and `TrustedPublisher`,
/// returning how many store entries were removed. Best-effort.
///
/// A legacy cert whose private key is on this machine is left alone: this is the
/// machine it was generated on (a FlexInput or HIDMaestro build box), where it may
/// still sign things that matter. Everywhere else FlexInput only ever added the
/// public cert, so nothing but FlexInput's old packages relied on it.
///
/// Only call once no installed package is signed by a legacy cert.
pub fn remove_legacy_trust() -> usize {
    let mut removed = 0;
    for thumb in &LEGACY_SIGNERS {
        if signing::has_private_key(Scope::LocalMachine, thumb)
            || signing::has_private_key(Scope::CurrentUser, thumb)
        {
            continue;
        }
        for store in ["ROOT", "TrustedPublisher"] {
            if matches!(signing::remove_from_store(Scope::LocalMachine, store, thumb), Ok(true)) {
                removed += 1;
            }
        }
    }
    removed
}

/// `pnputil /delete-driver <oemNN.inf> /uninstall /force` for every published
/// HIDMaestro package. Best-effort: discovers the published `oemNN.inf` names by
/// scanning `%SystemRoot%\INF` (same content sniff as `installed_inf_path`) and
/// deletes each. Errors are swallowed — a missing/locked package shouldn't abort
/// the reinstall (the post-install verify catches a real failure).
fn uninstall_all_hidmaestro_packages() {
    let pnputil = system32().join("pnputil.exe");
    for inf in installed_inf_names() {
        let _ = std::process::Command::new(&pnputil)
            .arg("/delete-driver")
            .arg(&inf)
            .arg("/uninstall")
            .arg("/force")
            .status();
    }
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

/// A fresh, randomly named directory under `%windir%\Temp` that only
/// Administrators and SYSTEM can open, removed again on drop.
///
/// The protection is load-bearing now that packages are signed here. Anyone able
/// to write to the staging directory could swap a driver binary between staging
/// and cataloguing, and the helper would sign the replacement into a trusted
/// catalog — code that then loads as a driver. The old staging spot, the user's
/// `%TEMP%`, is writable by the unelevated user; it was only safe while the
/// catalogs arrived pre-signed, when a swapped file just failed validation.
///
/// The DACL is set as the directory is created, so there's no window where it's
/// open; the random name means no one can create it first; and creation fails
/// rather than reusing a directory that already exists. It also keeps the path
/// plain ASCII, which the catalog builder needs — a user profile path may not be.
struct StagingDir(PathBuf);

impl StagingDir {
    /// Protected DACL: full control for SYSTEM and Administrators, inherited by
    /// everything created inside; no other principal gets any access.
    const SDDL: &'static str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

    fn create() -> Result<StagingDir, DeployError> {
        let parent = windows_dir().join("Temp");
        let sddl: Vec<u16> = Self::SDDL.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: *mut c_void = std::ptr::null_mut();
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut descriptor, std::ptr::null_mut())
        };
        if converted == 0 {
            return Err(DeployError::Staging(unsafe { GetLastError() }));
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let result = (|| {
            // A collision is astronomically unlikely, but retrying costs nothing.
            for _ in 0..4 {
                let path = parent.join(format!("FlexInputDriver-{}", random_hex(16)?));
                let wide = wide_nul(&path);
                if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } != 0 {
                    return Ok(StagingDir(path));
                }
                let err = unsafe { GetLastError() };
                if err != ERROR_ALREADY_EXISTS {
                    return Err(DeployError::Staging(err));
                }
            }
            Err(DeployError::Staging(ERROR_ALREADY_EXISTS))
        })();
        unsafe {
            LocalFree(descriptor);
        }
        result
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        // pnputil has copied what it needs into the DriverStore by now.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wide_nul(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

fn random_hex(bytes: usize) -> Result<String, DeployError> {
    let mut buf = vec![0u8; bytes];
    let status = unsafe {
        BCryptGenRandom(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG)
    };
    if status < 0 {
        return Err(DeployError::Staging(status as u32));
    }
    Ok(signing::hex(&buf))
}

/// The Windows directory, from the OS rather than `%SystemRoot%`. The helper runs
/// elevated but inherits its environment from the launching user, who can shadow
/// that variable; it decides which `pnputil.exe` runs and where packages are staged.
fn windows_dir() -> PathBuf {
    let mut buf = [0u16; 260];
    let n = unsafe { GetSystemWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n > buf.len() {
        return PathBuf::from(r"C:\Windows");
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..n]))
}

fn system32() -> PathBuf {
    windows_dir().join("System32")
}

const fn thumbprint(hex: &str) -> Thumbprint {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'A'..=b'F' => c - b'A' + 10,
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("thumbprint: not a hex digit"),
        }
    }
    let b = hex.as_bytes();
    assert!(b.len() == 40, "thumbprint: expected 40 hex digits");
    let mut out = [0u8; 20];
    let mut i = 0;
    while i < 20 {
        out[i] = (nibble(b[2 * i]) << 4) | nibble(b[2 * i + 1]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_embedded_and_nonempty() {
        // The driver files must be present at compile time.
        assert!(payload::HIDMAESTRO_DLL.len() > 10_000);
        assert!(payload::HMXINPUT_DLL.len() > 10_000);
        assert!(!payload::HIDMAESTRO_INF.is_empty());
        assert!(!payload::HIDMAESTRO_XUSB_INF.is_empty());
    }

    /// INF text, whether the file is UTF-16LE or UTF-8.
    fn inf_text(inf: &[u8]) -> String {
        if inf.starts_with(&[0xFF, 0xFE]) {
            let units: Vec<u16> = inf[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&units)
        } else {
            String::from_utf8_lossy(inf).into_owned()
        }
    }

    /// Hardware IDs declared in an INF's `[Standard.*]` models sections, in order.
    fn inf_hardware_ids(inf: &[u8]) -> Vec<String> {
        let text = inf_text(inf);
        let mut ids = Vec::new();
        let mut in_models = false;
        for line in text.lines() {
            let line = line.split(';').next().unwrap_or("").trim();
            if line.starts_with('[') {
                in_models = line.to_ascii_lowercase().starts_with("[standard.");
                continue;
            }
            if in_models {
                if let Some((_, rhs)) = line.split_once('=') {
                    ids.extend(rhs.split(',').skip(1).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
                }
            }
        }
        ids
    }

    #[test]
    fn package_hardware_ids_match_their_infs() {
        for (pkg, inf) in [(&MAIN_PACKAGE, payload::HIDMAESTRO_INF), (&XUSB_PACKAGE, payload::HIDMAESTRO_XUSB_INF)] {
            let declared: Vec<String> = inf_hardware_ids(inf).iter().map(|s| s.to_ascii_lowercase()).collect();
            let listed: Vec<String> = pkg.hardware_ids.iter().map(|s| s.to_ascii_lowercase()).collect();
            assert_eq!(listed, declared, "{} hardware ids drifted from the INF", pkg.inf);
        }
    }

    #[test]
    fn package_inf_names_its_catalog() {
        for (pkg, inf) in [(&MAIN_PACKAGE, payload::HIDMAESTRO_INF), (&XUSB_PACKAGE, payload::HIDMAESTRO_XUSB_INF)] {
            let text: String = inf_text(inf).to_ascii_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
            assert!(
                text.contains(&format!("catalogfile={}", pkg.catalog)),
                "{} doesn't name {}",
                pkg.inf,
                pkg.catalog
            );
        }
    }

    #[test]
    fn payload_driver_versions_are_readable() {
        // installed_driver() treats an unreadable version as Indeterminate and
        // never upgrades, so the shipped INFs must always parse. Pinned to the
        // vendored release; update alongside the driver files.
        assert_eq!(inf_driver_version(payload::HIDMAESTRO_INF).as_deref(), Some("1.4.7.48"));
        assert_eq!(inf_driver_version(payload::HIDMAESTRO_XUSB_INF).as_deref(), Some("1.4.7.48"));
    }

    #[test]
    fn only_version_or_signer_mismatches_reinstall() {
        assert!(InstalledDriver::OtherVersion.needs_reinstall());
        assert!(InstalledDriver::ForeignSigner.needs_reinstall());
        for state in [InstalledDriver::NotInstalled, InstalledDriver::Current, InstalledDriver::Indeterminate] {
            assert!(!state.needs_reinstall(), "{state:?}");
        }
    }

    #[test]
    fn legacy_thumbprints_parse() {
        assert_eq!(LEGACY_SIGNERS[0][0], 0x43);
        assert_eq!(LEGACY_SIGNERS[1][19], 0x68);
    }
}
