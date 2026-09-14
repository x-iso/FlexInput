//! Per-machine driver package signing, using in-box Windows components only.
//!
//! FlexInput used to vendor catalogs pre-signed by one machine's self-signed
//! test cert and add that cert to `Root` on every install: a single root-trust
//! anchor shared by every user, with its private key sitting wherever the
//! package was built. Instead the elevated helper now signs the package on the
//! machine it installs onto:
//!
//! 1. [`ensure_signing_cert`] finds or creates a self-signed code-signing cert
//!    backed by a **non-exportable** CNG key. The key never leaves the machine,
//!    and not even an elevated process can export it afterwards.
//! 2. [`sign_driver_package`] Authenticode-signs the driver binaries, builds the
//!    package catalog, and signs that too.
//!
//! No SDK or WDK tooling is involved: SignTool is explicitly non-redistributable
//! and Inf2Cat ships only with the WDK, so neither exists on a user's machine.
//! Everything here calls `mssign32`, `wintrust`, `crypt32` and `ncrypt`, which
//! are OS-serviced components of every Windows install.
//!
//! The catalog comes from wintrust's CDF engine, the one MakeCat and
//! PowerShell's `New-FileCatalog` drive, carrying the attributes Inf2Cat writes:
//! `OS` and `HWIDn` on the catalog, `File` and `OSAttr` on each member. A catalog
//! with those attributes, signed by a locally generated cert, was accepted by
//! `pnputil /add-driver`; the same package with the catalog left unsigned was
//! rejected with `TRUST_E_NOSIGNATURE`.

use std::cell::RefCell;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::core::{PCWSTR, PSTR, PWSTR};
use windows_sys::Win32::Foundation::{GetLastError, LocalFree, FILETIME, SYSTEMTIME};
use windows_sys::Win32::Security::Cryptography::Catalog::{
    CryptCATCDFClose, CryptCATCDFEnumAttributesWithCDFTag, CryptCATCDFEnumCatAttributes,
    CryptCATCDFEnumMembersByCDFTagEx, CryptCATCDFOpen, CRYPTCATATTRIBUTE, CRYPTCATMEMBER,
};
use windows_sys::Win32::Security::Cryptography::{
    CertAddCertificateContextToStore, CertAddEncodedCertificateToStore, CertCloseStore,
    CertCreateSelfSignCertificate, CertDeleteCertificateFromStore,
    CertDuplicateCertificateContext, CertEnumCertificatesInStore, CertFindCertificateInStore,
    CertFreeCertificateContext, CertGetCertificateContextProperty, CertGetNameStringW,
    CertOpenStore, CertStrToNameW, CryptAcquireCertificatePrivateKey, CryptEncodeObjectEx,
    CryptMsgClose, CryptMsgGetParam, CryptQueryObject, NCryptCreatePersistedKey, NCryptDeleteKey,
    NCryptFinalizeKey, NCryptFreeObject, NCryptOpenStorageProvider, NCryptSetProperty,
    SignerFreeSignerContext, SignerSignEx2, CALG_SHA_256, CERT_CONTEXT, CERT_EXTENSION,
    CERT_EXTENSIONS, CERT_FIND_SHA1_HASH, CERT_FIND_SUBJECT_CERT, CERT_INFO,
    CERT_KEY_PROV_INFO_PROP_ID, CERT_NAME_SIMPLE_DISPLAY_TYPE,
    CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED, CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
    CERT_QUERY_FORMAT_FLAG_ALL, CERT_QUERY_OBJECT_FILE, CERT_SHA1_HASH_PROP_ID,
    CERT_STORE_ADD_NEW, CERT_STORE_ADD_REPLACE_EXISTING, CERT_STORE_PROV_SYSTEM_W,
    CERT_SYSTEM_STORE_CURRENT_USER, CERT_SYSTEM_STORE_LOCAL_MACHINE, CERT_X500_NAME_STR,
    CMSG_SIGNER_CERT_INFO_PARAM, CRYPT_ACQUIRE_ONLY_NCRYPT_KEY_FLAG, CRYPT_ACQUIRE_SILENT_FLAG,
    CRYPT_ALGORITHM_IDENTIFIER, CRYPT_BIT_BLOB, CRYPT_ENCODE_ALLOC_FLAG, CRYPT_INTEGER_BLOB,
    CRYPT_KEY_PROV_INFO, CRYPT_MACHINE_KEYSET, CTL_USAGE, HCERTSTORE, MS_KEY_STORAGE_PROVIDER,
    NCRYPT_ALLOW_SIGNING_FLAG, NCRYPT_EXPORT_POLICY_PROPERTY, NCRYPT_KEY_HANDLE,
    NCRYPT_KEY_USAGE_PROPERTY, NCRYPT_LENGTH_PROPERTY, NCRYPT_MACHINE_KEY_FLAG,
    NCRYPT_PROV_HANDLE, NCRYPT_RSA_ALGORITHM, NCRYPT_SILENT_FLAG, PKCS_7_ASN_ENCODING,
    SIGNER_CERT, SIGNER_CERT_0, SIGNER_CERT_POLICY_CHAIN, SIGNER_CERT_STORE,
    SIGNER_CERT_STORE_INFO, SIGNER_CONTEXT, SIGNER_FILE_INFO, SIGNER_NO_ATTR,
    SIGNER_SIGNATURE_INFO, SIGNER_SIGNATURE_INFO_0, SIGNER_SUBJECT_FILE, SIGNER_SUBJECT_INFO,
    SIGNER_SUBJECT_INFO_0, X509_ASN_ENCODING, X509_ENHANCED_KEY_USAGE, X509_KEY_USAGE,
    szOID_ENHANCED_KEY_USAGE, szOID_KEY_USAGE, szOID_PKIX_KP_CODE_SIGNING, szOID_RSA_SHA256RSA,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemTime, GetSystemTimeAsFileTime};

/// Common name of the per-machine signing cert, and how an existing one is found.
pub const SIGNING_CERT_NAME: &str = "FlexInput Local Driver Signing";

/// RSA modulus size. Generous for a key meant to anchor local trust for decades;
/// the one-off generation cost doesn't matter.
const KEY_BITS: u32 = 3072;

/// Validity of a newly created cert. Catalogs are signed without a timestamp
/// (the trust is purely local, and a timestamp would need the network), so a
/// signature is only valid while its cert is.
const CERT_VALIDITY_YEARS: u16 = 30;

/// An existing cert is reused only while it stays valid at least this much
/// longer, so a package is never signed by a cert about to lapse. A lapsed or
/// replaced cert shows up as a signer mismatch and the package is re-signed.
const MIN_REMAINING_VALIDITY_DAYS: u64 = 365;

/// `CRYPTCAT_ATTR_AUTHENTICATED | CRYPTCAT_ATTR_NAMEASCII | CRYPTCAT_ATTR_DATAASCII`,
/// the flags Inf2Cat writes on every attribute.
const CAT_ATTR_FLAGS: &str = "0x10010001";
/// Catalog-level `OS` attribute Inf2Cat writes for an x64 Windows 10+ package.
const CAT_OS: &str = "_v100_X64";
/// Per-member `OSAttr` Inf2Cat writes alongside [`CAT_OS`].
const CAT_OS_ATTR: &str = "2:10.0";

/// SHA-1 certificate thumbprint, the identity Windows uses for a cert in a store.
pub type Thumbprint = [u8; 20];

/// Which certificate and key store a signing cert lives in.
///
/// Production always uses [`Scope::LocalMachine`]: the helper runs elevated and
/// driver trust is machine-wide. [`Scope::CurrentUser`] lets a non-elevated probe
/// exercise cert creation, signing and cataloguing without touching machine state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    LocalMachine,
    CurrentUser,
}

impl Scope {
    fn store_location(self) -> u32 {
        match self {
            Scope::LocalMachine => CERT_SYSTEM_STORE_LOCAL_MACHINE,
            Scope::CurrentUser => CERT_SYSTEM_STORE_CURRENT_USER,
        }
    }
}

#[derive(Debug)]
pub enum SignError {
    /// A Win32 or CNG call failed: the call, and its `GetLastError` code or HRESULT.
    Api(&'static str, u32),
    /// wintrust's CDF engine rejected lines while building a catalog.
    Catalog(Vec<String>),
    /// A path that must go into a CDF isn't ASCII, or contains `=` or a line
    /// break. CDF text is parsed as ASCII, so such a path would silently name the
    /// wrong file; stage the package somewhere with a plain path instead.
    UnsupportedPath(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for SignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignError::Api(call, code) => write!(f, "{call} failed (0x{code:08X})"),
            SignError::Catalog(lines) => write!(f, "catalog build failed: {}", lines.join("; ")),
            SignError::UnsupportedPath(p) => write!(
                f,
                "path can't be catalogued (needs plain ASCII without '='): {}",
                p.display()
            ),
            SignError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for SignError {}

impl From<std::io::Error> for SignError {
    fn from(e: std::io::Error) -> Self {
        SignError::Io(e)
    }
}

// ── Certificate ─────────────────────────────────────────────────────────────

/// A certificate context this module holds a reference to.
pub struct SigningCert(*const CERT_CONTEXT);

// A CERT_CONTEXT is reference-counted and immutable after creation.
unsafe impl Send for SigningCert {}

impl SigningCert {
    pub fn thumbprint(&self) -> Result<Thumbprint, SignError> {
        thumbprint_of(self.0)
    }

    /// DER encoding, for adding the cert to `Root` and `TrustedPublisher`.
    pub fn der(&self) -> &[u8] {
        // SAFETY: the context outlives the borrow and its encoded bytes never change.
        unsafe { std::slice::from_raw_parts((*self.0).pbCertEncoded, (*self.0).cbCertEncoded as usize) }
    }
}

impl Drop for SigningCert {
    fn drop(&mut self) {
        unsafe {
            CertFreeCertificateContext(self.0);
        }
    }
}

/// Find this machine's signing cert in `My`, or create one. See [`ensure_cert_named`].
pub fn ensure_signing_cert(scope: Scope) -> Result<SigningCert, SignError> {
    ensure_cert_named(scope, SIGNING_CERT_NAME)
}

/// This machine's usable signing cert, if it has one. Never creates a cert —
/// for callers that only need to compare against it.
pub fn find_signing_cert(scope: Scope) -> Option<SigningCert> {
    let my = Store::open(scope, "MY").ok()?;
    find_usable_cert(&my, SIGNING_CERT_NAME)
}

/// Thumbprints of every cert named [`SIGNING_CERT_NAME`] in `scope`'s `My`,
/// usable or not: for retiring superseded certs, and for uninstall.
pub fn signing_cert_thumbprints(scope: Scope) -> Vec<Thumbprint> {
    let Ok(my) = Store::open(scope, "MY") else { return Vec::new() };
    let mut out = Vec::new();
    let mut ctx: *mut CERT_CONTEXT = null_mut();
    loop {
        ctx = unsafe { CertEnumCertificatesInStore(my.0, ctx) };
        if ctx.is_null() {
            break;
        }
        if simple_name(ctx).as_deref() == Some(SIGNING_CERT_NAME) {
            if let Ok(t) = thumbprint_of(ctx) {
                out.push(t);
            }
        }
    }
    out
}

/// Find a usable code-signing cert with common name `name` in `scope`'s `My`
/// store, or create one there.
///
/// "Usable" means the name matches exactly, its private key can actually be
/// opened (a cert whose key was deleted would fail at signing time), and it
/// stays valid for [`MIN_REMAINING_VALIDITY_DAYS`]. Among several, the one valid
/// longest wins. Production goes through [`ensure_signing_cert`]; a distinct
/// name lets a probe run without ever touching the production cert.
pub fn ensure_cert_named(scope: Scope, name: &str) -> Result<SigningCert, SignError> {
    let my = Store::open(scope, "MY")?;
    if let Some(cert) = find_usable_cert(&my, name) {
        return Ok(cert);
    }
    create_cert(scope, &my, name)
}

fn find_usable_cert(store: &Store, name: &str) -> Option<SigningCert> {
    let now = now_filetime();
    let min_remaining = MIN_REMAINING_VALIDITY_DAYS * FILETIME_TICKS_PER_DAY;
    let mut best: Option<(u64, SigningCert)> = None;
    let mut ctx: *mut CERT_CONTEXT = null_mut();
    loop {
        // Passing the previous context frees it, so anything kept is duplicated.
        ctx = unsafe { CertEnumCertificatesInStore(store.0, ctx) };
        if ctx.is_null() {
            break;
        }
        if simple_name(ctx).as_deref() != Some(name) {
            continue;
        }
        let not_after = unsafe { filetime_u64(&(*(*ctx).pCertInfo).NotAfter) };
        if not_after < now.saturating_add(min_remaining) || !private_key_opens(ctx) {
            continue;
        }
        if best.as_ref().is_none_or(|(t, _)| not_after > *t) {
            let dup = unsafe { CertDuplicateCertificateContext(ctx) };
            best = Some((not_after, SigningCert(dup)));
        }
    }
    best.map(|(_, cert)| cert)
}

fn create_cert(scope: Scope, my: &Store, name: &str) -> Result<SigningCert, SignError> {
    let key_name = format!("{name} {}", unique_suffix());
    let key = NewKey::create(scope, &key_name)?;

    let subject = encode_x500_name(&format!("CN={name}"))?;
    // digitalSignature only; the extension is non-critical, as HIDMaestro's own
    // generator writes it.
    let mut key_usage_bits = [0x80u8];
    let key_usage = crypt_encode(
        X509_KEY_USAGE,
        &CRYPT_BIT_BLOB { cbData: 1, pbData: key_usage_bits.as_mut_ptr(), cUnusedBits: 0 } as *const _ as *const c_void,
    )?;
    let mut eku_oids = [szOID_PKIX_KP_CODE_SIGNING as PSTR];
    let eku = crypt_encode(
        X509_ENHANCED_KEY_USAGE,
        &CTL_USAGE { cUsageIdentifier: 1, rgpszUsageIdentifier: eku_oids.as_mut_ptr() } as *const _ as *const c_void,
    )?;
    let mut extensions = [
        CERT_EXTENSION { pszObjId: szOID_KEY_USAGE as PSTR, fCritical: 0, Value: key_usage.blob() },
        CERT_EXTENSION { pszObjId: szOID_ENHANCED_KEY_USAGE as PSTR, fCritical: 0, Value: eku.blob() },
    ];
    let extensions = CERT_EXTENSIONS { cExtension: extensions.len() as u32, rgExtension: extensions.as_mut_ptr() };

    let mut container = wide(&key_name);
    // Recorded on the cert so the signer can find the key again: provider type 0
    // marks a CNG key storage provider rather than a legacy CSP.
    let key_prov_info = CRYPT_KEY_PROV_INFO {
        pwszContainerName: container.as_mut_ptr(),
        pwszProvName: MS_KEY_STORAGE_PROVIDER as PWSTR,
        dwProvType: 0,
        dwFlags: if scope == Scope::LocalMachine { CRYPT_MACHINE_KEYSET } else { 0 },
        cProvParam: 0,
        rgProvParam: null_mut(),
        dwKeySpec: 0,
    };
    let algorithm = CRYPT_ALGORITHM_IDENTIFIER {
        pszObjId: szOID_RSA_SHA256RSA as PSTR,
        Parameters: CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() },
    };
    let not_after = years_from_now(CERT_VALIDITY_YEARS);

    let created = unsafe {
        CertCreateSelfSignCertificate(
            key.handle,
            &subject.blob(),
            0,
            &key_prov_info,
            &algorithm,
            null(), // valid from now
            &not_after,
            &extensions,
        )
    };
    if created.is_null() {
        return Err(SignError::Api("CertCreateSelfSignCertificate", last_error()));
    }
    let mut stored: *mut CERT_CONTEXT = null_mut();
    let added = unsafe { CertAddCertificateContextToStore(my.0, created, CERT_STORE_ADD_NEW, &mut stored) };
    let err = last_error();
    unsafe {
        CertFreeCertificateContext(created);
    }
    if added == 0 {
        return Err(SignError::Api("CertAddCertificateContextToStore", err));
    }
    // Only now does a cert reference the key; until here, dropping `key` deletes it.
    key.keep();
    Ok(SigningCert(stored))
}

/// A freshly created CNG key, deleted again on drop unless [`NewKey::keep`] is
/// called — so a failure partway through cert creation leaves no orphaned key.
struct NewKey {
    provider: NCRYPT_PROV_HANDLE,
    handle: NCRYPT_KEY_HANDLE,
    finalized: bool,
    keep: bool,
}

impl NewKey {
    fn create(scope: Scope, name: &str) -> Result<NewKey, SignError> {
        let mut key = NewKey { provider: 0, handle: 0, finalized: false, keep: false };
        hr("NCryptOpenStorageProvider", unsafe {
            NCryptOpenStorageProvider(&mut key.provider, MS_KEY_STORAGE_PROVIDER, 0)
        })?;
        let wname = wide(name);
        let flags = if scope == Scope::LocalMachine { NCRYPT_MACHINE_KEY_FLAG } else { 0 };
        hr("NCryptCreatePersistedKey", unsafe {
            NCryptCreatePersistedKey(key.provider, &mut key.handle, NCRYPT_RSA_ALGORITHM, wname.as_ptr(), 0, flags)
        })?;
        key.set_u32(NCRYPT_LENGTH_PROPERTY, KEY_BITS)?;
        // Export policy 0: the private key can never be exported, by anyone.
        key.set_u32(NCRYPT_EXPORT_POLICY_PROPERTY, 0)?;
        key.set_u32(NCRYPT_KEY_USAGE_PROPERTY, NCRYPT_ALLOW_SIGNING_FLAG)?;
        hr("NCryptFinalizeKey", unsafe { NCryptFinalizeKey(key.handle, NCRYPT_SILENT_FLAG) })?;
        key.finalized = true;
        Ok(key)
    }

    fn set_u32(&self, property: PCWSTR, value: u32) -> Result<(), SignError> {
        let bytes = value.to_le_bytes();
        hr("NCryptSetProperty", unsafe {
            NCryptSetProperty(self.handle, property, bytes.as_ptr(), bytes.len() as u32, 0)
        })
    }

    fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for NewKey {
    fn drop(&mut self) {
        unsafe {
            if self.handle != 0 {
                if self.finalized && !self.keep {
                    // Deletes the persisted key and frees the handle.
                    NCryptDeleteKey(self.handle, 0);
                } else {
                    NCryptFreeObject(self.handle);
                }
            }
            if self.provider != 0 {
                NCryptFreeObject(self.provider);
            }
        }
    }
}

fn private_key_opens(ctx: *const CERT_CONTEXT) -> bool {
    let mut handle = 0usize;
    let mut key_spec = 0u32;
    let mut caller_frees = 0;
    let ok = unsafe {
        CryptAcquireCertificatePrivateKey(
            ctx,
            CRYPT_ACQUIRE_ONLY_NCRYPT_KEY_FLAG | CRYPT_ACQUIRE_SILENT_FLAG,
            null(),
            &mut handle,
            &mut key_spec,
            &mut caller_frees,
        )
    };
    if ok != 0 && caller_frees != 0 && handle != 0 {
        unsafe {
            NCryptFreeObject(handle);
        }
    }
    ok != 0
}

// ── Signing ─────────────────────────────────────────────────────────────────

/// Authenticode-sign `path` in place (a PE binary or a `.cat`) with SHA-256,
/// replacing any signature it already carries.
pub fn sign_file(path: &Path, cert: &SigningCert) -> Result<(), SignError> {
    let wpath = wide_path(path);
    let mut index = 0u32; // must point at zero
    let mut file_info = SIGNER_FILE_INFO {
        cbSize: size_u32::<SIGNER_FILE_INFO>(),
        pwszFileName: wpath.as_ptr(),
        hFile: null_mut(),
    };
    let subject = SIGNER_SUBJECT_INFO {
        cbSize: size_u32::<SIGNER_SUBJECT_INFO>(),
        pdwIndex: &mut index,
        dwSubjectChoice: SIGNER_SUBJECT_FILE,
        Anonymous: SIGNER_SUBJECT_INFO_0 { pSignerFileInfo: &mut file_info },
    };
    let mut store_info = SIGNER_CERT_STORE_INFO {
        cbSize: size_u32::<SIGNER_CERT_STORE_INFO>(),
        pSigningCert: cert.0,
        dwCertPolicy: SIGNER_CERT_POLICY_CHAIN,
        hCertStore: null_mut(),
    };
    let signer = SIGNER_CERT {
        cbSize: size_u32::<SIGNER_CERT>(),
        dwCertChoice: SIGNER_CERT_STORE,
        Anonymous: SIGNER_CERT_0 { pCertStoreInfo: &mut store_info },
        hwnd: null_mut(),
    };
    let signature = SIGNER_SIGNATURE_INFO {
        cbSize: size_u32::<SIGNER_SIGNATURE_INFO>(),
        algidHash: CALG_SHA_256,
        dwAttrChoice: SIGNER_NO_ATTR,
        Anonymous: SIGNER_SIGNATURE_INFO_0 { pAttrAuthcode: null_mut() },
        psAuthenticated: null_mut(),
        psUnauthenticated: null_mut(),
    };
    let mut context: *mut SIGNER_CONTEXT = null_mut();
    let result = unsafe {
        SignerSignEx2(
            0,
            &subject,
            &signer,
            &signature,
            null(),
            0,
            null(),
            null(), // no timestamp: local trust, and no network dependency
            null(),
            null(),
            &mut context,
            null(),
            null(),
        )
    };
    if !context.is_null() {
        unsafe {
            SignerFreeSignerContext(context);
        }
    }
    hr("SignerSignEx2", result)
}

/// The SHA-1 thumbprint of whoever signed `path` (a signed PE or `.cat`), or
/// `None` if it carries no signature. Used to tell whether an installed package
/// was signed by this machine's current cert.
pub fn signer_thumbprint(path: &Path) -> Result<Option<Thumbprint>, SignError> {
    let wpath = wide_path(path);
    let mut store: HCERTSTORE = null_mut();
    let mut msg: *mut c_void = null_mut();
    let found = unsafe {
        CryptQueryObject(
            CERT_QUERY_OBJECT_FILE,
            wpath.as_ptr() as *const c_void,
            // PKCS#7 content types only. A `.cat` is PKCS#7 SignedData wrapping a
            // CTL; offered the CTL type too, crypt32 classifies it as a CTL
            // context and returns no message handle to read the signer from.
            CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED | CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
            CERT_QUERY_FORMAT_FLAG_ALL,
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut store,
            &mut msg,
            null_mut(),
        )
    };
    if found == 0 {
        return Ok(None);
    }
    let result = (|| {
        if msg.is_null() {
            return Err(SignError::Api("CryptQueryObject (no message handle)", 0));
        }
        let mut len = 0u32;
        if unsafe { CryptMsgGetParam(msg, CMSG_SIGNER_CERT_INFO_PARAM, 0, null_mut(), &mut len) } == 0 {
            return Ok(None);
        }
        // CERT_INFO holds pointers, so the buffer must be pointer-aligned.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        if unsafe { CryptMsgGetParam(msg, CMSG_SIGNER_CERT_INFO_PARAM, 0, buf.as_mut_ptr() as *mut c_void, &mut len) } == 0 {
            return Err(SignError::Api("CryptMsgGetParam", last_error()));
        }
        let info = buf.as_ptr() as *const CERT_INFO;
        let ctx = unsafe {
            CertFindCertificateInStore(store, X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, 0, CERT_FIND_SUBJECT_CERT, info as *const c_void, null())
        };
        if ctx.is_null() {
            return Ok(None);
        }
        let thumb = thumbprint_of(ctx);
        unsafe {
            CertFreeCertificateContext(ctx);
        }
        thumb.map(Some)
    })();
    unsafe {
        if !store.is_null() {
            CertCloseStore(store, 0);
        }
        if !msg.is_null() {
            CryptMsgClose(msg);
        }
    }
    result
}

// ── Catalog ─────────────────────────────────────────────────────────────────

/// One driver package: file names relative to the directory it's staged in.
pub struct DriverPackage<'a> {
    pub inf: &'a str,
    /// Every binary the INF copies. Each is signed, then catalogued.
    pub binaries: &'a [&'a str],
    /// The catalog the INF names in `CatalogFile=`.
    pub catalog: &'a str,
    /// The INF's hardware IDs in declaration order, recorded as `HWID1..n`
    /// catalog attributes the way Inf2Cat records them.
    pub hardware_ids: &'a [&'a str],
}

/// Sign `pkg`'s binaries, build its catalog, and sign the catalog, all in `dir`.
pub fn sign_driver_package(dir: &Path, pkg: &DriverPackage, cert: &SigningCert) -> Result<(), SignError> {
    // Re-signing replaces the signature a binary was shipped with, so nothing
    // installed still names another machine's cert. It doesn't disturb the
    // catalog: Authenticode's PE hash excludes the signature, so the catalog
    // records the same hash either side of this loop.
    for binary in pkg.binaries {
        sign_file(&dir.join(binary), cert)?;
    }
    let mut members = vec![dir.join(pkg.inf)];
    members.extend(pkg.binaries.iter().map(|b| dir.join(b)));
    let catalog = dir.join(pkg.catalog);
    build_catalog(&catalog, pkg.hardware_ids, &members)?;
    sign_file(&catalog, cert)
}

thread_local! {
    /// Errors wintrust reports through the CDF parse callback, which carries no
    /// context pointer of its own.
    static CDF_ERRORS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "system" fn record_cdf_error(area: u32, local_error: u32, line: PCWSTR) {
    let text = if line.is_null() { String::new() } else { unsafe { pcwstr_to_string(line) } };
    CDF_ERRORS.with(|e| e.borrow_mut().push(format!("area {area:#x}, error {local_error:#x}: {text}")));
}

/// Write an unsigned catalog at `catalog` covering `members`, with Inf2Cat's
/// attributes. Sign it afterwards with [`sign_file`].
pub fn build_catalog(catalog: &Path, hardware_ids: &[&str], members: &[PathBuf]) -> Result<(), SignError> {
    let cdf_path = catalog.with_extension("cdf");
    std::fs::write(&cdf_path, cdf_text(catalog, hardware_ids, members)?)?;
    let _ = std::fs::remove_file(catalog);
    let result = run_cdf(&cdf_path, members.len());
    let _ = std::fs::remove_file(&cdf_path);
    result?;
    match std::fs::metadata(catalog) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err(SignError::Catalog(vec![format!("no catalog written at {}", catalog.display())])),
    }
}

/// The CDF: MakeCat's input format, as PowerShell's `New-FileCatalog` writes it,
/// plus the `OS`/`HWIDn`/`File`/`OSAttr` attributes a driver catalog carries.
fn cdf_text(catalog: &Path, hardware_ids: &[&str], members: &[PathBuf]) -> Result<String, SignError> {
    let mut s = String::new();
    s.push_str("[CatalogHeader]\r\n");
    s.push_str(&format!("Name={}\r\n", cdf_path(catalog)?));
    s.push_str("CatalogVersion=2\r\n");
    s.push_str("HashAlgorithms=SHA256\r\n");
    s.push_str(&format!("CATATTR1={CAT_ATTR_FLAGS}:OS:{CAT_OS}\r\n"));
    for (i, hwid) in hardware_ids.iter().enumerate() {
        if !hwid.is_ascii() || hwid.contains(['\r', '\n']) {
            return Err(SignError::Catalog(vec![format!("unusable hardware id {hwid:?}")]));
        }
        s.push_str(&format!("CATATTR{}={CAT_ATTR_FLAGS}:HWID{}:{}\r\n", i + 2, i + 1, hwid.to_ascii_lowercase()));
    }
    s.push_str("\r\n[CatalogFiles]\r\n");
    for member in members {
        let path = cdf_path(member)?;
        let file = member
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .ok_or_else(|| SignError::UnsupportedPath(member.clone()))?;
        // `<HASH>` makes the file's hash the member tag; the ATTRn lines hang
        // attributes off that tag.
        s.push_str(&format!("<HASH>{path}={path}\r\n"));
        s.push_str(&format!("<HASH>{path}ATTR1={CAT_ATTR_FLAGS}:File:{file}\r\n"));
        s.push_str(&format!("<HASH>{path}ATTR2={CAT_ATTR_FLAGS}:OSAttr:{CAT_OS_ATTR}\r\n"));
    }
    Ok(s)
}

fn cdf_path(path: &Path) -> Result<String, SignError> {
    let s = path.to_str().ok_or_else(|| SignError::UnsupportedPath(path.to_path_buf()))?;
    if !s.is_ascii() || s.contains(['=', '\r', '\n']) {
        return Err(SignError::UnsupportedPath(path.to_path_buf()));
    }
    Ok(s.to_string())
}

/// Drive wintrust's CDF engine over `cdf_path`. Entries are hashed and written
/// as they're enumerated, so every catalog attribute, every member, and every
/// attribute of every member must be walked to the end — stopping early (as
/// `New-FileCatalog` can afford to, with one attribute per member) would drop
/// the rest from the catalog.
fn run_cdf(cdf_path: &Path, expected_members: usize) -> Result<(), SignError> {
    CDF_ERRORS.with(|e| e.borrow_mut().clear());
    let wpath = wide_path(cdf_path);
    let cdf = unsafe { CryptCATCDFOpen(wpath.as_ptr(), Some(record_cdf_error)) };
    if cdf.is_null() {
        let mut errors = take_cdf_errors();
        errors.push(format!("CryptCATCDFOpen failed (0x{:08X})", last_error()));
        return Err(SignError::Catalog(errors));
    }

    let mut cat_attr: *mut CRYPTCATATTRIBUTE = null_mut();
    loop {
        cat_attr = unsafe { CryptCATCDFEnumCatAttributes(cdf, cat_attr, Some(record_cdf_error)) };
        if cat_attr.is_null() {
            break;
        }
    }

    let mut members = 0usize;
    let mut tag: PWSTR = null_mut();
    let mut member: *const CRYPTCATMEMBER = null();
    loop {
        tag = unsafe {
            CryptCATCDFEnumMembersByCDFTagEx(
                cdf,
                tag,
                Some(record_cdf_error),
                &mut member as *mut *const CRYPTCATMEMBER as *const *const CRYPTCATMEMBER,
                1, // keep going past a bad entry so every error gets reported
                null(),
            )
        };
        if tag.is_null() {
            break;
        }
        members += 1;
        let mut attr: *mut CRYPTCATATTRIBUTE = null_mut();
        loop {
            attr = unsafe { CryptCATCDFEnumAttributesWithCDFTag(cdf, tag, member, attr, Some(record_cdf_error)) };
            if attr.is_null() {
                break;
            }
        }
    }

    let closed = unsafe { CryptCATCDFClose(cdf) };
    let close_err = last_error();
    let mut errors = take_cdf_errors();
    if closed == 0 {
        errors.push(format!("CryptCATCDFClose failed (0x{close_err:08X})"));
    }
    if members != expected_members {
        errors.push(format!("catalogued {members} of {expected_members} files"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(SignError::Catalog(errors))
    }
}

fn take_cdf_errors() -> Vec<String> {
    CDF_ERRORS.with(|e| std::mem::take(&mut *e.borrow_mut()))
}

// ── Stores ──────────────────────────────────────────────────────────────────

struct Store(HCERTSTORE);

impl Store {
    fn open(scope: Scope, name: &str) -> Result<Store, SignError> {
        let wname = wide(name);
        let handle = unsafe {
            CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, scope.store_location(), wname.as_ptr() as *const c_void)
        };
        if handle.is_null() {
            return Err(SignError::Api("CertOpenStore", last_error()));
        }
        Ok(Store(handle))
    }

    fn find(&self, thumbprint: &Thumbprint) -> *mut CERT_CONTEXT {
        let blob = CRYPT_INTEGER_BLOB { cbData: 20, pbData: thumbprint.as_ptr() as *mut u8 };
        unsafe {
            CertFindCertificateInStore(self.0, X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, 0, CERT_FIND_SHA1_HASH, &blob as *const _ as *const c_void, null())
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        unsafe {
            CertCloseStore(self.0, 0);
        }
    }
}

/// Add a DER cert to a system store (e.g. `ROOT`, `TrustedPublisher`). Idempotent.
pub fn add_to_store(scope: Scope, store_name: &str, der: &[u8]) -> Result<(), SignError> {
    let store = Store::open(scope, store_name)?;
    let ok = unsafe {
        CertAddEncodedCertificateToStore(
            store.0,
            X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
            der.as_ptr(),
            der.len() as u32,
            CERT_STORE_ADD_REPLACE_EXISTING,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(SignError::Api("CertAddEncodedCertificateToStore", last_error()));
    }
    Ok(())
}

/// Remove the cert with `thumbprint` from a system store. `Ok(false)` if absent.
pub fn remove_from_store(scope: Scope, store_name: &str, thumbprint: &Thumbprint) -> Result<bool, SignError> {
    let store = Store::open(scope, store_name)?;
    let ctx = store.find(thumbprint);
    if ctx.is_null() {
        return Ok(false);
    }
    // Frees the context whether or not it succeeds.
    if unsafe { CertDeleteCertificateFromStore(ctx) } == 0 {
        return Err(SignError::Api("CertDeleteCertificateFromStore", last_error()));
    }
    Ok(true)
}

/// Whether `scope`'s `My` store holds the cert with `thumbprint` together with an
/// openable private key — i.e. whether the cert was *generated* on this machine,
/// rather than merely trusted by it.
pub fn has_private_key(scope: Scope, thumbprint: &Thumbprint) -> bool {
    let Ok(my) = Store::open(scope, "MY") else { return false };
    let ctx = my.find(thumbprint);
    if ctx.is_null() {
        return false;
    }
    let has = unsafe { CertGetCertificateContextProperty(ctx, CERT_KEY_PROV_INFO_PROP_ID, null_mut(), &mut 0) } != 0
        && private_key_opens(ctx);
    unsafe {
        CertFreeCertificateContext(ctx);
    }
    has
}

/// Delete the cert with `thumbprint` from `scope`'s `My` store and destroy its
/// private key. `Ok(false)` if no such cert. Key first: once the cert is gone,
/// nothing records where the key lives.
pub fn delete_cert_and_key(scope: Scope, thumbprint: &Thumbprint) -> Result<bool, SignError> {
    let my = Store::open(scope, "MY")?;
    let ctx = my.find(thumbprint);
    if ctx.is_null() {
        return Ok(false);
    }
    let mut handle = 0usize;
    let mut key_spec = 0u32;
    let mut caller_frees = 0;
    let acquired = unsafe {
        CryptAcquireCertificatePrivateKey(
            ctx,
            CRYPT_ACQUIRE_ONLY_NCRYPT_KEY_FLAG | CRYPT_ACQUIRE_SILENT_FLAG,
            null(),
            &mut handle,
            &mut key_spec,
            &mut caller_frees,
        )
    };
    if acquired != 0 && handle != 0 {
        unsafe {
            NCryptDeleteKey(handle, 0); // also frees the handle
        }
    }
    if unsafe { CertDeleteCertificateFromStore(ctx) } == 0 {
        return Err(SignError::Api("CertDeleteCertificateFromStore", last_error()));
    }
    Ok(true)
}

// ── Helpers ─────────────────────────────────────────────────────────────────

const FILETIME_TICKS_PER_DAY: u64 = 24 * 60 * 60 * 10_000_000;

/// Upper-case hex, the form thumbprints appear in throughout Windows tooling.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn thumbprint_of(ctx: *const CERT_CONTEXT) -> Result<Thumbprint, SignError> {
    let mut out = [0u8; 20];
    let mut len = out.len() as u32;
    let ok = unsafe { CertGetCertificateContextProperty(ctx, CERT_SHA1_HASH_PROP_ID, out.as_mut_ptr() as *mut c_void, &mut len) };
    if ok == 0 || len != 20 {
        return Err(SignError::Api("CertGetCertificateContextProperty", last_error()));
    }
    Ok(out)
}

fn simple_name(ctx: *const CERT_CONTEXT) -> Option<String> {
    let mut buf = [0u16; 256];
    let n = unsafe { CertGetNameStringW(ctx, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, null(), buf.as_mut_ptr(), buf.len() as u32) };
    // n counts the terminator; 1 means an empty name.
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

fn encode_x500_name(name: &str) -> Result<OwnedBlob, SignError> {
    let wname = wide(name);
    let mut len = 0u32;
    let first = unsafe { CertStrToNameW(X509_ASN_ENCODING, wname.as_ptr(), CERT_X500_NAME_STR, null(), null_mut(), &mut len, null_mut()) };
    if first == 0 {
        return Err(SignError::Api("CertStrToNameW", last_error()));
    }
    let mut bytes = vec![0u8; len as usize];
    let second = unsafe { CertStrToNameW(X509_ASN_ENCODING, wname.as_ptr(), CERT_X500_NAME_STR, null(), bytes.as_mut_ptr(), &mut len, null_mut()) };
    if second == 0 {
        return Err(SignError::Api("CertStrToNameW", last_error()));
    }
    bytes.truncate(len as usize);
    Ok(OwnedBlob::Vec(bytes))
}

fn crypt_encode(struct_type: windows_sys::core::PCSTR, info: *const c_void) -> Result<OwnedBlob, SignError> {
    let mut out: *mut u8 = null_mut();
    let mut len = 0u32;
    let ok = unsafe {
        CryptEncodeObjectEx(
            X509_ASN_ENCODING,
            struct_type,
            info,
            CRYPT_ENCODE_ALLOC_FLAG,
            null(),
            &mut out as *mut *mut u8 as *mut c_void,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(SignError::Api("CryptEncodeObjectEx", last_error()));
    }
    Ok(OwnedBlob::Local(out, len))
}

/// Encoded bytes handed to crypt32 as a `CRYPT_INTEGER_BLOB`, from either Rust
/// or a `LocalAlloc`'d buffer crypt32 returned.
enum OwnedBlob {
    Vec(Vec<u8>),
    Local(*mut u8, u32),
}

impl OwnedBlob {
    fn blob(&self) -> CRYPT_INTEGER_BLOB {
        match self {
            OwnedBlob::Vec(v) => CRYPT_INTEGER_BLOB { cbData: v.len() as u32, pbData: v.as_ptr() as *mut u8 },
            OwnedBlob::Local(p, n) => CRYPT_INTEGER_BLOB { cbData: *n, pbData: *p },
        }
    }
}

impl Drop for OwnedBlob {
    fn drop(&mut self) {
        if let OwnedBlob::Local(p, _) = *self {
            if !p.is_null() {
                unsafe {
                    LocalFree(p as *mut c_void);
                }
            }
        }
    }
}

fn years_from_now(years: u16) -> SYSTEMTIME {
    let mut t: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe {
        GetSystemTime(&mut t);
    }
    t.wYear += years;
    if t.wMonth == 2 && t.wDay == 29 {
        t.wDay = 28; // the target year may not be a leap year
    }
    t
}

fn now_filetime() -> u64 {
    let mut ft = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    unsafe {
        GetSystemTimeAsFileTime(&mut ft);
    }
    filetime_u64(&ft)
}

fn filetime_u64(ft: &FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{}", std::process::id())
}

fn hr(call: &'static str, result: i32) -> Result<(), SignError> {
    if result < 0 {
        Err(SignError::Api(call, result as u32))
    } else {
        Ok(())
    }
}

fn last_error() -> u32 {
    unsafe { GetLastError() }
}

fn size_u32<T>() -> u32 {
    std::mem::size_of::<T>() as u32
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_path(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

unsafe fn pcwstr_to_string(p: PCWSTR) -> String {
    let mut len = 0usize;
    while len < 4096 && unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdf_carries_inf2cat_attributes() {
        let dir = PathBuf::from(r"C:\Windows\Temp\FlexInputDriver");
        let text = cdf_text(
            &dir.join("hidmaestro.cat"),
            &["root\\HIDMaestro"],
            &[dir.join("hidmaestro.inf"), dir.join("HIDMaestro.dll")],
        )
        .unwrap();
        assert!(text.contains("CATATTR1=0x10010001:OS:_v100_X64\r\n"));
        assert!(text.contains("CATATTR2=0x10010001:HWID1:root\\hidmaestro\r\n"));
        assert!(text.contains(r"<HASH>C:\Windows\Temp\FlexInputDriver\HIDMaestro.dllATTR1=0x10010001:File:hidmaestro.dll"));
        assert!(text.contains(r"<HASH>C:\Windows\Temp\FlexInputDriver\hidmaestro.infATTR2=0x10010001:OSAttr:2:10.0"));
    }

    #[test]
    fn cdf_rejects_paths_it_cannot_express() {
        let bad = PathBuf::from(r"C:\Users\Jürgen\AppData\Local\Temp\x\hidmaestro.inf");
        assert!(matches!(cdf_text(Path::new(r"C:\x\a.cat"), &[], &[bad]), Err(SignError::UnsupportedPath(_))));
        let eq = PathBuf::from(r"C:\a=b\hidmaestro.inf");
        assert!(matches!(cdf_text(Path::new(r"C:\x\a.cat"), &[], &[eq]), Err(SignError::UnsupportedPath(_))));
    }
}
