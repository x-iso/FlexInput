//! HIDMaestro v2.0's machine-wide ownership, FlexInput's half (upstream
//! `docs/design/coexistence.md`, `MachineClaims.cs`, `OwnerCensus.cs`).
//!
//! A program built on HIDMaestro 2.0's SDK sweeps, whenever it starts or
//! installs its driver, every HIDMaestro device whose owner it can prove gone.
//! A device belongs to a running program when a record whose owner claim
//! exists lists its transport index, or when that index's claim is held.
//! Failing both, a device whose index has a `Global\HIDMaestroInputAck<N>`
//! section reads as a crashed v1.12 program's, and the sweep removes it. That
//! was every FlexInput device once it made InputAck for the version contract,
//! so starting PadForge removed FlexInput's keyboard and mouse.
//!
//! A claim is a named pipe under `ProtectedPrefix\Administrators\`, made with
//! `FILE_FLAG_FIRST_PIPE_INSTANCE` and one instance: making it is taking it, a
//! second create fails while anyone holds it, and Windows deletes it with its
//! last handle, which the system closes when the process ends however it ends.
//! Anyone can test a claim with `WaitNamedPipe`, which opens nothing, but only
//! a token with Administrators enabled can make one under that prefix, so an
//! unelevated program can't squat it.
//!
//! The helper holds one owner claim for its life, beside its record
//! `HKLM\SOFTWARE\HIDMaestroOwners\<owner id>`, and one index claim per live
//! controller. The record lists each controller under `Controllers\<token>`,
//! written before any PnP work and removed after its teardown.

use std::sync::Mutex;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_FILE_NOT_FOUND, FILETIME, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, WaitNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

use crate::identity;
use crate::orchestrator::registry;

const RECORDS_ROOT: &str = r"SOFTWARE\HIDMaestroOwners";
const CONTROLLERS_KEY: &str = "Controllers";
const CLAIM_PREFIX: &str = r"ProtectedPrefix\Administrators\";
/// Owner Administrators; SYNCHRONIZE and FILE_READ_ATTRIBUTES for SYSTEM,
/// Administrators and LocalService, upstream's `ClaimSddl`. Nobody can read or
/// write a claim, and probing one needs no access at all.
const CLAIM_SDDL: &str = "O:BAD:P(A;;0x00100080;;;SY)(A;;0x00100080;;;BA)(A;;0x00100080;;;LS)";
/// Transport indices the SDK's allocator reaches before it gives up.
pub const INDEX_LIMIT: u32 = 4096;

fn owner_pipe(owner_id: &str) -> String {
    format!("{CLAIM_PREFIX}HIDMaestro.Owner.{owner_id}")
}

fn index_pipe(index: u32) -> String {
    format!("{CLAIM_PREFIX}HIDMaestro.Index.{index}")
}

/// The per-index owner pipe every release from v1.12 serves while it has a
/// controller at that index, made with no prefix (any program can make it, so
/// it only ever counts as a reason to keep off the index).
fn legacy_owner_pipe(index: u32) -> String {
    format!("HIDMaestroOwner{index}")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A held claim. Dropping it lets the claim go.
pub struct Claim(HANDLE);

// SAFETY: a pipe handle has no thread affinity, and nothing but Drop uses it.
unsafe impl Send for Claim {}

impl Drop for Claim {
    fn drop(&mut self) {
        // SAFETY: a handle CreateNamedPipeW returned, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Why a claim wasn't taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimRefused {
    /// Someone holds it, this process included: a one-instance pipe refuses
    /// its own holder a second instance too.
    Held,
    /// The pipe couldn't be made for another reason, with the Win32 error: a
    /// token without Administrators enabled, or a pipe system without the
    /// prefix. Nothing can be arbitrated then.
    Failed(u32),
}

/// Take the named claim.
fn take(name: &str) -> Result<Claim, ClaimRefused> {
    let sddl = wide(CLAIM_SDDL);
    let mut sd = std::ptr::null_mut();
    // SAFETY: `sddl` is NUL-terminated; `sd` receives a LocalAlloc'd
    // descriptor, freed below.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(ClaimRefused::Failed(unsafe { GetLastError() }));
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let path = wide(&format!(r"\\.\pipe\{name}"));
    // SAFETY: `path` is NUL-terminated and `sa` outlives the call.
    let h = unsafe {
        CreateNamedPipeW(
            path.as_ptr(),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
            1, 0, 0, 0, &sa,
        )
    };
    let err = unsafe { GetLastError() };
    // SAFETY: the descriptor ConvertString… allocated; the pipe keeps its own copy.
    unsafe { LocalFree(sd) };
    if h == INVALID_HANDLE_VALUE {
        // ERROR_ACCESS_DENIED (the first instance exists) and ERROR_PIPE_BUSY
        // (the one instance is taken) mean held, but an unelevated token gets
        // ERROR_ACCESS_DENIED too. The probe tells them apart.
        return Err(if is_held(name) { ClaimRefused::Held } else { ClaimRefused::Failed(err) });
    }
    Ok(Claim(h))
}

/// Whether a live process holds the named claim. A timeout of 0 would mean the
/// server's default wait, so the probe passes 1 ms. A true return and
/// ERROR_SEM_TIMEOUT both mean a holder, ERROR_FILE_NOT_FOUND none, and any
/// other error counts as a holder.
fn is_held(name: &str) -> bool {
    let path = wide(&format!(r"\\.\pipe\{name}"));
    // SAFETY: `path` is NUL-terminated.
    if unsafe { WaitNamedPipeW(path.as_ptr(), 1) } != 0 {
        return true;
    }
    let err = unsafe { GetLastError() };
    err != ERROR_FILE_NOT_FOUND
}

/// Take transport index `index`'s claim, held for the controller's life.
pub fn claim_index(index: u32) -> Result<Claim, ClaimRefused> {
    take(&index_pipe(index))
}

/// Whether a program on HIDMaestro v1.12 or later serves `index`'s owner pipe:
/// it has a controller there, or is making one, whose devnode may not exist yet.
pub fn legacy_owner_holds(index: u32) -> bool {
    is_held(&legacy_owner_pipe(index))
}

// ── The owner record ─────────────────────────────────────────────────────────

struct Owner {
    id: String,
    _claim: Claim,
}

static OWNER: Mutex<Option<Owner>> = Mutex::new(None);

fn owner_id() -> Option<String> {
    OWNER.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|o| o.id.clone())
}

fn record_path(owner_id: &str) -> String {
    format!(r"{RECORDS_ROOT}\{owner_id}")
}

fn controller_path(owner_id: &str, token: &str) -> String {
    format!(r"{RECORDS_ROOT}\{owner_id}\{CONTROLLERS_KEY}\{token}")
}

/// This process's creation time as a UTC FILETIME, the record's `StartTime`.
fn start_time() -> u64 {
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the pseudo-handle for this process; four out-params.
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return 0;
    }
    (created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64
}

/// Take the helper's owner claim and write its record, before its first
/// machine-wide write. Does nothing once taken. `Err` carries the Win32 error
/// when the claim couldn't be made; FlexInput then runs unarbitrated, as it did
/// before claims, and other programs' sweeps may take its devices.
pub fn take_owner() -> Result<(), u32> {
    let mut owner = OWNER.lock().unwrap_or_else(|e| e.into_inner());
    if owner.is_some() {
        return Ok(());
    }
    // Upstream draws a GUID; any 32 hex digits read as one. This process's id
    // and creation time name it uniquely for as long as it can hold the claim.
    let started = start_time();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let id = format!("{:08x}{started:016x}{nanos:08x}", std::process::id());
    let claim = take(&owner_pipe(&id)).map_err(|e| match e {
        ClaimRefused::Held => 0,
        ClaimRefused::Failed(code) => code,
    })?;
    let rec = record_path(&id);
    let image = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let version = crate::shm::SDK_VERSION.map(|p| p.to_string()).join(".");
    let _ = registry::write_dword(registry::HKLM, &rec, "Pid", std::process::id());
    let _ = registry::write_string(registry::HKLM, &rec, "Image", &image);
    let _ = registry::write_qword(registry::HKLM, &rec, "StartTime", started);
    let _ = registry::write_string(registry::HKLM, &rec, "SdkVersion", &version);
    *owner = Some(Owner { id, _claim: claim });
    Ok(())
}

/// List a controller in the record, before any PnP work for it. `profile` is
/// only read by people: the FlexInput device id.
pub fn add_controller(token: &str, index: u32, profile: &str) {
    let Some(id) = owner_id() else { return };
    let key = controller_path(&id, token);
    let _ = registry::write_dword(registry::HKLM, &key, "TransportIndex", index);
    let _ = registry::write_string(registry::HKLM, &key, "Backend", "umdf");
    let _ = registry::write_string(registry::HKLM, &key, "Profile", profile);
}

/// Name a listed controller's devnode once it exists: a sweep of a dead
/// record removes the devices by it.
pub fn set_controller_instance(token: &str, instance_id: &str) {
    let Some(id) = owner_id() else { return };
    let _ = registry::write_string(registry::HKLM, &controller_path(&id, token), "InstanceId", instance_id);
}

/// Drop a controller from the record, once its devices are gone.
pub fn remove_controller(token: &str) {
    if let Some(id) = owner_id() {
        registry::delete_tree(registry::HKLM, &controller_path(&id, token));
    }
}

/// Delete the helper's record, at exit once its devices are gone. While
/// devices stay (persistence on), so does the record: once the claim goes with
/// the process it names them as a closed program's, which is what lets the next
/// sweep, any program's, remove them.
pub fn delete_own_record() {
    if let Some(id) = owner_id() {
        registry::delete_tree(registry::HKLM, &record_path(&id));
        delete_root_if_empty();
    }
}

fn delete_root_if_empty() {
    if registry::enum_subkeys(registry::HKLM, RECORDS_ROOT).is_some_and(|k| k.is_empty()) {
        registry::delete_tree(registry::HKLM, RECORDS_ROOT);
    }
}

/// One record as another program reads it.
struct Record {
    id: String,
    pid: u32,
    image: String,
    sdk_version: String,
    tokens: Vec<String>,
}

fn records() -> Vec<Record> {
    registry::enum_subkeys(registry::HKLM, RECORDS_ROOT)
        .unwrap_or_default()
        .into_iter()
        .map(|id| {
            let rec = record_path(&id);
            Record {
                pid: registry::read_dword(registry::HKLM, &rec, "Pid").unwrap_or(0),
                image: registry::read_string(registry::HKLM, &rec, "Image").unwrap_or_default(),
                sdk_version: registry::read_string(registry::HKLM, &rec, "SdkVersion").unwrap_or_default(),
                tokens: registry::enum_subkeys(registry::HKLM, &format!(r"{rec}\{CONTROLLERS_KEY}"))
                    .unwrap_or_default(),
                id,
            }
        })
        .collect()
}

/// The first running program, other than this helper, whose record lists a
/// controller, described for the log: "process 1234 (C:\…\PadForge.exe,
/// HIDMaestro 2.0.1)". Its devices are bound to the installed driver, which
/// therefore mustn't be replaced under them.
pub fn other_live_owner() -> Option<String> {
    let me = owner_id();
    records()
        .into_iter()
        .filter(|r| me.as_deref() != Some(r.id.as_str()))
        .find(|r| !r.tokens.is_empty() && is_held(&owner_pipe(&r.id)))
        .map(|r| format!("process {} ({}, HIDMaestro {})", r.pid, r.image, r.sdk_version))
}

/// Delete the records an earlier FlexInput helper left when it was killed or
/// kept devices at exit: owner gone, same executable name, and only
/// FlexInput's tokens. Another program's dead record is its own SDK's to
/// sweep. Returns how many went.
pub fn sweep_own_dead_records() -> usize {
    let me = owner_id();
    let exe_name = |path: &str| {
        std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_ascii_lowercase())
    };
    let Some(mine) = std::env::current_exe().ok().and_then(|p| exe_name(&p.display().to_string())) else {
        return 0;
    };
    let mut removed = 0;
    for r in records() {
        if me.as_deref() == Some(r.id.as_str())
            || exe_name(&r.image).as_deref() != Some(mine.as_str())
            || !r.tokens.iter().all(|t| identity::is_ours(t))
            || is_held(&owner_pipe(&r.id))
        {
            continue;
        }
        registry::delete_tree(registry::HKLM, &record_path(&r.id));
        removed += 1;
    }
    if removed > 0 {
        delete_root_if_empty();
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_names_match_upstream() {
        assert_eq!(owner_pipe("abc"), r"ProtectedPrefix\Administrators\HIDMaestro.Owner.abc");
        assert_eq!(index_pipe(7), r"ProtectedPrefix\Administrators\HIDMaestro.Index.7");
        assert_eq!(legacy_owner_pipe(0), "HIDMaestroOwner0");
    }

    // A probe needs no rights, so it runs unelevated: a name nobody made is free.
    #[test]
    fn an_unmade_claim_is_not_held() {
        assert!(!is_held(&index_pipe(INDEX_LIMIT + 12345)));
        assert!(!legacy_owner_holds(INDEX_LIMIT + 12345));
    }
}
