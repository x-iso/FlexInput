//! Shared-memory IPC client — Rust port of HIDMaestro's `SharedMemoryIO.cs`.
//!
//! Layout is a verbatim transcription of
//! `sdk/HIDMaestro.Core/Internal/SharedMemoryIO.cs` (`@main`, 2026-06-11), which
//! the file's own header notes must match `driver/driver.h`.
//!
//! ## Input section `Global\HIDMaestroInput{i}` — 362 bytes, seqlock writer
//! ```text
//! off 0   u32   SeqNo               odd while writing, even when settled
//! off 4   u32   DataSize            valid bytes in Data[]
//! off 8   [256] Data[]              HID input report, Report-ID stripped
//! off 264 [14]  GipData[]           Xbox XUSB companion only (else zero)
//! off 278 u32   ExtendedReportSize  0 = legacy; >0 = use ExtendedReportData
//! off 282 [80]  ExtendedReportData[] full RID-included report (Sony BT 0x31/0x11)
//! ```
//! **Correctness rule (the prime suspect for the prior "erratic input" bug):**
//! every *legacy* frame MUST write `ExtendedReportSize = 0`, else the driver can
//! emit stale extended bytes left by a previous arming / foreign writer.
//!
//! ## Output section `Global\HIDMaestroOutput{i}` — 16904 bytes, ring buffer
//! ```text
//! header  off 0  u32  Head            monotonic total writes by the driver
//!         off 4  u32  _Reserved
//! slot[k] off 0  u32  SeqNo           == Head at write time (per-slot seqlock)
//!         off 4  u8   Source
//!         off 5  u8   ReportId
//!         off 6  u16  DataSize
//!         off 8  [256] Data[]
//! ```
//! 64 slots, 264 bytes each, starting at offset 8. This is the rumble/FFB →
//! `poll_outputs()` path.

use std::ffi::c_void;
use std::sync::atomic::{fence, Ordering};

// ── Input layout constants (SharedMemoryIO.cs) ──────────────────────────────
const DATA_OFFSET: usize = 8;
const DATA_CAPACITY: usize = 256;
const GIP_DATA_OFFSET: usize = DATA_OFFSET + DATA_CAPACITY; // 264
const GIP_DATA_LENGTH: usize = 14;
const EXTENDED_SIZE_OFFSET: usize = GIP_DATA_OFFSET + GIP_DATA_LENGTH; // 278
const EXTENDED_DATA_OFFSET: usize = EXTENDED_SIZE_OFFSET + 4; // 282
const EXTENDED_DATA_CAPACITY: usize = 80;
/// Total input section size — 362 bytes.
pub const SHARED_INPUT_SIZE: usize = EXTENDED_DATA_OFFSET + EXTENDED_DATA_CAPACITY;

// ── Output ring layout constants (SharedMemoryIO.cs) ────────────────────────
const OUTPUT_RING_SLOTS: u32 = 64;
const OUTPUT_SLOT_SIZE: usize = 4 + 1 + 1 + 2 + 256; // 264
const OUTPUT_HEADER_SIZE: usize = 4 + 4; // 8
/// Total output section size — 16904 bytes.
pub const SHARED_OUTPUT_SIZE: usize = OUTPUT_HEADER_SIZE + OUTPUT_RING_SLOTS as usize * OUTPUT_SLOT_SIZE;
const OUTPUT_SLOT_OFFSET_SEQNO: usize = 0;
const OUTPUT_SLOT_OFFSET_SOURCE: usize = 4;
const OUTPUT_SLOT_OFFSET_REPORT_ID: usize = 5;
const OUTPUT_SLOT_OFFSET_SIZE: usize = 6;
const OUTPUT_SLOT_OFFSET_DATA: usize = 8;

/// SDDL for the `Global\` sections + events shared between the elevated helper
/// (which CREATES them) and the normally-launched, unelevated app (which OPENS
/// them read/write to drive the device and wait on the event).
///
/// DACL grants GENERIC_ALL to Local System, Builtin Admins, LocalService
/// (WUDFHost runs as LocalService for UMDF2, so the driver companion needs full
/// access) AND to Authenticated Users — the unelevated app opens with
/// FILE_MAP_READ|FILE_MAP_WRITE / EVENT_MODIFY_STATE|SYNCHRONIZE, which the old
/// `(A;;GR;;;WD)` World-READ-only entry denied (write/modify), so on a standard
/// (non-admin) account the app's `open()` failed and the virtual device showed
/// offline while the driver still presented HID.
///
/// The SACL sets a Low mandatory label (`ML;;NW;;;LW`): a `Global\` object made
/// by the high-IL helper otherwise inherits a High label with NO_WRITE_UP, which
/// blocks a medium-IL (normal) process from writing even with DACL access. The
/// label lets the unelevated app write down to it. (The original was verbatim
/// from HIDMaestro's `SharedMemoryIO.cs`, which assumed same-IL create+open.)
const SDDL: &str =
    "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;LS)(A;;GA;;;AU)S:(ML;;NW;;;LW)";

// ── Win32 constants ─────────────────────────────────────────────────────────
const PAGE_READWRITE: u32 = 0x04;
const FILE_MAP_READ: u32 = 0x0002;
const FILE_MAP_WRITE: u32 = 0x0004;
const EVENT_MODIFY_STATE: u32 = 0x0002;
const SYNCHRONIZE: u32 = 0x0010_0000;
const SDDL_REVISION_1: u32 = 1;
const INVALID_HANDLE_VALUE: isize = -1;

#[derive(Debug)]
pub enum ShmError {
    /// `CreateFileMappingW` / `OpenFileMappingW` failed (GetLastError).
    Mapping(u32),
    /// `MapViewOfFile` failed (GetLastError).
    MapView(u32),
    /// `CreateEventExW` / `OpenEventW` failed (GetLastError).
    Event(u32),
    /// SDDL → security descriptor conversion failed (GetLastError).
    Sddl(u32),
}

impl std::fmt::Display for ShmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShmError::Mapping(e) => write!(f, "file-mapping failed (err {e})"),
            ShmError::MapView(e) => write!(f, "MapViewOfFile failed (err {e})"),
            ShmError::Event(e) => write!(f, "event open/create failed (err {e})"),
            ShmError::Sddl(e) => write!(f, "SDDL conversion failed (err {e})"),
        }
    }
}
impl std::error::Error for ShmError {}

// ── Win32 FFI (mirrors the hand-rolled style in flexinput-virtual/windows.rs) ─
#[link(name = "kernel32")]
extern "system" {
    fn CreateFileMappingW(
        file: *mut c_void,
        attrs: *mut c_void,
        protect: u32,
        max_size_high: u32,
        max_size_low: u32,
        name: *const u16,
    ) -> *mut c_void;
    fn OpenFileMappingW(desired_access: u32, inherit: i32, name: *const u16) -> *mut c_void;
    fn MapViewOfFile(
        mapping: *mut c_void,
        desired_access: u32,
        offset_high: u32,
        offset_low: u32,
        bytes: usize,
    ) -> *mut c_void;
    fn UnmapViewOfFile(base: *const c_void) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn CreateEventExW(attrs: *mut c_void, name: *const u16, flags: u32, access: u32) -> *mut c_void;
    fn OpenEventW(desired_access: u32, inherit: i32, name: *const u16) -> *mut c_void;
    fn SetEvent(event: *mut c_void) -> i32;
    fn GetLastError() -> u32;
}

#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl: *const u16,
        revision: u32,
        out_sd: *mut *mut c_void,
        out_size: *mut u32,
    ) -> i32;
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
}

#[repr(C)]
struct SecurityAttributes {
    n_length: u32,
    lp_security_descriptor: *mut c_void,
    b_inherit_handle: i32,
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Build a `SECURITY_ATTRIBUTES` carrying the HIDMaestro SDDL. Returns the
/// attributes plus the raw security-descriptor pointer that the caller must
/// `LocalFree` after the Create call returns.
///
/// # Safety
/// The returned `lp_security_descriptor` must be freed with `LocalFree` exactly
/// once, after the Win32 Create* call that consumes the attributes completes.
unsafe fn build_security_attributes() -> Result<(SecurityAttributes, *mut c_void), ShmError> {
    let sddl = to_wide(SDDL);
    let mut sd: *mut c_void = std::ptr::null_mut();
    let ok = ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl.as_ptr(),
        SDDL_REVISION_1,
        &mut sd,
        std::ptr::null_mut(),
    );
    if ok == 0 {
        return Err(ShmError::Sddl(GetLastError()));
    }
    let sa = SecurityAttributes {
        n_length: std::mem::size_of::<SecurityAttributes>() as u32,
        lp_security_descriptor: sd,
        b_inherit_handle: 0,
    };
    Ok((sa, sd))
}

/// RAII wrapper for a mapped section: owns the mapping handle + view pointer and
/// unmaps/closes on drop.
struct MappedSection {
    handle: *mut c_void,
    view: *mut u8,
    size: usize,
}

impl MappedSection {
    /// Create (or open-existing — `CreateFileMappingW` returns the existing
    /// handle for a matching name) a pagefile-backed named section with the
    /// HIDMaestro SDDL, and map a read/write view. Zeroes the whole view.
    fn create(name: &str, size: usize) -> Result<Self, ShmError> {
        let wname = to_wide(name);
        unsafe {
            let (mut sa, sd) = build_security_attributes()?;
            let handle = CreateFileMappingW(
                INVALID_HANDLE_VALUE as *mut c_void,
                &mut sa as *mut _ as *mut c_void,
                PAGE_READWRITE,
                0,
                size as u32,
                wname.as_ptr(),
            );
            let err = GetLastError();
            LocalFree(sd);
            if handle.is_null() {
                return Err(ShmError::Mapping(err));
            }
            Self::map_view(handle, size, /*zero=*/ true)
        }
    }

    /// Open an existing named section created by another process (e.g.
    /// HIDMaestro's C# app or the elevated helper) and map a read/write view.
    /// Does NOT zero — the creator owns initialization.
    fn open(name: &str, size: usize) -> Result<Self, ShmError> {
        let wname = to_wide(name);
        unsafe {
            let handle = OpenFileMappingW(FILE_MAP_READ | FILE_MAP_WRITE, 0, wname.as_ptr());
            if handle.is_null() {
                return Err(ShmError::Mapping(GetLastError()));
            }
            Self::map_view(handle, size, /*zero=*/ false)
        }
    }

    /// # Safety
    /// `handle` must be a valid section handle of at least `size` bytes.
    unsafe fn map_view(handle: *mut c_void, size: usize, zero: bool) -> Result<Self, ShmError> {
        let view = MapViewOfFile(handle, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, size) as *mut u8;
        if view.is_null() {
            let err = GetLastError();
            CloseHandle(handle);
            return Err(ShmError::MapView(err));
        }
        if zero {
            std::ptr::write_bytes(view, 0, size);
        }
        Ok(MappedSection { handle, view, size })
    }

    // Field accessors use *unaligned* volatile ops. The driver layout places
    // `ExtendedReportSize` (u32) at offset 278, which is NOT 4-aligned, so
    // aligned `write_volatile`/`read_volatile` would be UB (and trips the debug
    // alignment check). C# reaches these via `Marshal.WriteInt32`, which is
    // unaligned-safe; mirror that. `read/write_unaligned` still go through the
    // mapped view (a real memory write to shared memory); we add explicit fences
    // around the seqlock in the writer/reader so ordering is preserved without
    // relying on per-access volatile semantics.
    #[inline]
    unsafe fn write_u32(&self, off: usize, v: u32) {
        debug_assert!(off + 4 <= self.size);
        std::ptr::write_unaligned(self.view.add(off) as *mut u32, v);
    }
    /// Volatile store of the seqlock SeqNo (always at offset 0, page-aligned).
    /// Volatile so the compiler can't elide or reorder the odd→payload→even
    /// publish sequence; the fences in the writer pair with the readers' loads.
    #[inline]
    unsafe fn write_seqno_volatile(&self, v: u32) {
        std::ptr::write_volatile(self.view as *mut u32, v);
    }
    /// Volatile load of a seqlock SeqNo (offset 0 for the input header, or a
    /// 4-aligned slot base for the output ring — both are 4-aligned).
    #[inline]
    unsafe fn read_seqno_volatile(&self, off: usize) -> u32 {
        debug_assert!(off.is_multiple_of(4));
        std::ptr::read_volatile(self.view.add(off) as *const u32)
    }
    #[inline]
    unsafe fn read_u32(&self, off: usize) -> u32 {
        debug_assert!(off + 4 <= self.size);
        std::ptr::read_unaligned(self.view.add(off) as *const u32)
    }
    #[inline]
    unsafe fn read_u16(&self, off: usize) -> u16 {
        debug_assert!(off + 2 <= self.size);
        std::ptr::read_unaligned(self.view.add(off) as *const u16)
    }
    #[inline]
    unsafe fn read_u8(&self, off: usize) -> u8 {
        debug_assert!(off < self.size);
        std::ptr::read_unaligned(self.view.add(off))
    }
    #[inline]
    unsafe fn copy_in(&self, off: usize, src: &[u8]) {
        debug_assert!(off + src.len() <= self.size);
        std::ptr::copy_nonoverlapping(src.as_ptr(), self.view.add(off), src.len());
    }
    #[inline]
    unsafe fn copy_out(&self, off: usize, dst: &mut [u8]) {
        debug_assert!(off + dst.len() <= self.size);
        std::ptr::copy_nonoverlapping(self.view.add(off) as *const u8, dst.as_mut_ptr(), dst.len());
    }
    #[inline]
    unsafe fn zero_range(&self, off: usize, len: usize) {
        debug_assert!(off + len <= self.size);
        std::ptr::write_bytes(self.view.add(off), 0, len);
    }
}

impl Drop for MappedSection {
    fn drop(&mut self) {
        unsafe {
            if !self.view.is_null() {
                UnmapViewOfFile(self.view as *const c_void);
            }
            if !self.handle.is_null() {
                CloseHandle(self.handle);
            }
        }
    }
}

/// RAII wrapper for the input-signaling named event.
struct EventHandle(*mut c_void);

impl EventHandle {
    /// Create a named auto-reset event (`dwFlags = 0`) under the HIDMaestro SDDL.
    fn create(name: &str) -> Result<Self, ShmError> {
        let wname = to_wide(name);
        unsafe {
            let (mut sa, sd) = build_security_attributes()?;
            let ev = CreateEventExW(
                &mut sa as *mut _ as *mut c_void,
                wname.as_ptr(),
                0, // auto-reset, not initially set
                EVENT_MODIFY_STATE | SYNCHRONIZE,
            );
            let err = GetLastError();
            LocalFree(sd);
            if ev.is_null() {
                return Err(ShmError::Event(err));
            }
            Ok(EventHandle(ev))
        }
    }

    /// Open an existing named event created by another process.
    fn open(name: &str) -> Result<Self, ShmError> {
        let wname = to_wide(name);
        unsafe {
            let ev = OpenEventW(EVENT_MODIFY_STATE | SYNCHRONIZE, 0, wname.as_ptr());
            if ev.is_null() {
                return Err(ShmError::Event(GetLastError()));
            }
            Ok(EventHandle(ev))
        }
    }

    #[inline]
    fn signal(&self) {
        unsafe {
            SetEvent(self.0);
        }
    }
}

impl Drop for EventHandle {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                CloseHandle(self.0);
            }
        }
    }
}

// EventHandle/MappedSection own raw Win32 handles; sending across threads is
// safe (handles are process-global), and FlexInput drives a device from one I/O
// thread at a time.
unsafe impl Send for MappedSection {}
unsafe impl Send for EventHandle {}

/// Writer for one controller's input section (`Global\HIDMaestroInput{i}`).
///
/// Single-writer seqlock: the driver and XUSB companion are many readers that
/// retry on a SeqNo mismatch. Hold one of these per active virtual device and
/// call [`write_frame`](Self::write_frame) once per I/O tick.
pub struct InputSection {
    section: MappedSection,
    event: EventHandle,
    seq_no: u32,
}

impl InputSection {
    /// Create the input section + signaling event for `controller_index`.
    /// Requires the caller to hold `SeCreateGlobalPrivilege` (i.e. run elevated)
    /// — WUDFHost cannot create `Global\` objects itself. For the Phase-1 gate,
    /// prefer [`open`](Self::open) against a C#-created section instead.
    pub fn create(controller_index: u32) -> Result<Self, ShmError> {
        let section = MappedSection::create(
            &format!(r"Global\HIDMaestroInput{controller_index}"),
            SHARED_INPUT_SIZE,
        )?;
        let event = EventHandle::create(&format!(r"Global\HIDMaestroInputEvent{controller_index}"))?;
        Ok(InputSection { section, event, seq_no: 0 })
    }

    /// Open an input section + event already created by another process
    /// (HIDMaestro's C# app, or FlexInput's elevated helper in a later phase).
    pub fn open(controller_index: u32) -> Result<Self, ShmError> {
        let section = MappedSection::open(
            &format!(r"Global\HIDMaestroInput{controller_index}"),
            SHARED_INPUT_SIZE,
        )?;
        let event = EventHandle::open(&format!(r"Global\HIDMaestroInputEvent{controller_index}"))?;
        // Resume from whatever sequence the section currently holds so the driver
        // (which tracks last-seen SeqNo) sees our first write as a real change.
        let seq_no = unsafe { section.read_u32(0) };
        Ok(InputSection { section, event, seq_no })
    }

    /// Publish one legacy input frame (Report-ID stripped `report`), optionally
    /// with Xbox `gip` companion bytes. Mirrors `WriteInputFrame` with
    /// `extendedData = null`, so `ExtendedReportSize` is cleared to 0.
    ///
    /// `report` is truncated to `DATA_CAPACITY` (256); `gip`, if present, must be
    /// `GIP_DATA_LENGTH` (14) bytes and is written only for Xbox companions.
    pub fn write_frame(&mut self, report: &[u8], gip: Option<&[u8; 14]>) {
        let data_len = report.len().min(DATA_CAPACITY);
        unsafe {
            // 1. Mark write-in-progress (odd SeqNo) before touching payload.
            let pending = self.seq_no.wrapping_add(1);
            self.section.write_seqno_volatile(pending);
            fence(Ordering::Release);

            // 2. Payload: DataSize + Data[] (+ GipData[] for Xbox); clear the
            //    extended-report size so the driver stays on the legacy path and
            //    never emits stale extended bytes from a prior arming/foreign
            //    writer. This per-frame clear is the correctness rule that the
            //    prior implementation most likely missed.
            self.section.write_u32(4, data_len as u32);
            if data_len > 0 {
                self.section.copy_in(DATA_OFFSET, &report[..data_len]);
            }
            match gip {
                Some(g) => self.section.copy_in(GIP_DATA_OFFSET, g),
                None => {
                    // Section was zeroed at create; but a foreign writer between
                    // runs could have dirtied it — keep the legacy path clean.
                    self.section.zero_range(GIP_DATA_OFFSET, GIP_DATA_LENGTH);
                }
            }
            self.section.write_u32(EXTENDED_SIZE_OFFSET, 0);

            // 3. Mark write-complete (even SeqNo).
            fence(Ordering::Release);
            self.seq_no = pending.wrapping_add(1);
            self.section.write_seqno_volatile(self.seq_no);
        }
        // 4. Wake the driver's per-device worker (auto-reset event).
        self.event.signal();
    }

    /// Diagnostic snapshot of the current input section: `(SeqNo, DataSize,
    /// Data[..DataSize], ExtendedReportSize)`. For probes/tests only — reads the
    /// section another writer may own. Data is capped at `DATA_CAPACITY`.
    pub fn debug_snapshot(&self) -> (u32, u32, Vec<u8>, u32) {
        unsafe {
            let seq = self.section.read_u32(0);
            let mut data_size = self.section.read_u32(4) as usize;
            if data_size > DATA_CAPACITY {
                data_size = DATA_CAPACITY;
            }
            let mut data = vec![0u8; data_size];
            if data_size > 0 {
                self.section.copy_out(DATA_OFFSET, &mut data);
            }
            let ext = self.section.read_u32(EXTENDED_SIZE_OFFSET);
            (seq, data_size as u32, data, ext)
        }
    }

    /// Publish one extended (vendor-blob) input frame — full RID-included report
    /// (e.g. Sony BT 0x31/0x11). Mirrors `WriteInputFrame` with a non-null
    /// `extendedData`: sets `ExtendedReportSize > 0`; the driver emits
    /// `ExtendedReportData` verbatim. `DataSize`/`Data[]` carry the legacy
    /// fallback report (may be empty).
    pub fn write_extended_frame(&mut self, legacy: &[u8], extended: &[u8]) {
        let data_len = legacy.len().min(DATA_CAPACITY);
        let ext_len = extended.len().min(EXTENDED_DATA_CAPACITY);
        unsafe {
            let pending = self.seq_no.wrapping_add(1);
            self.section.write_u32(0, pending);
            fence(Ordering::Release);

            self.section.write_u32(4, data_len as u32);
            if data_len > 0 {
                self.section.copy_in(DATA_OFFSET, &legacy[..data_len]);
            }
            if ext_len > 0 {
                self.section.copy_in(EXTENDED_DATA_OFFSET, &extended[..ext_len]);
            }
            self.section.write_u32(EXTENDED_SIZE_OFFSET, ext_len as u32);

            fence(Ordering::Release);
            self.seq_no = pending.wrapping_add(1);
            self.section.write_seqno_volatile(self.seq_no);
        }
        self.event.signal();
    }
}

/// One decoded output packet from the driver (rumble / haptics / LED / FFB).
#[derive(Debug, Clone)]
pub struct OutputFrame {
    pub source: u8,
    pub report_id: u8,
    pub data: Vec<u8>,
}

/// Reader for one controller's output ring (`Global\HIDMaestroOutput{i}`).
///
/// The driver publishes captured output reports into a 64-slot ring; drain it in
/// a loop each poll iteration (e.g. every ~8 ms) via [`try_read`](Self::try_read)
/// until it returns `None`, so a burst of FFB packets isn't coalesced.
pub struct OutputSection {
    section: MappedSection,
    last_seq: u32,
}

impl OutputSection {
    /// Create the output section for `controller_index` (requires elevation; see
    /// [`InputSection::create`]).
    pub fn create(controller_index: u32) -> Result<Self, ShmError> {
        let section = MappedSection::create(
            &format!(r"Global\HIDMaestroOutput{controller_index}"),
            SHARED_OUTPUT_SIZE,
        )?;
        Ok(OutputSection { section, last_seq: 0 })
    }

    /// Open an output section already created by another process.
    pub fn open(controller_index: u32) -> Result<Self, ShmError> {
        let section = MappedSection::open(
            &format!(r"Global\HIDMaestroOutput{controller_index}"),
            SHARED_OUTPUT_SIZE,
        )?;
        // Start from the current Head so we don't replay pre-existing packets as
        // brand-new on the first poll.
        let last_seq = unsafe { section.read_u32(0) };
        Ok(OutputSection { section, last_seq })
    }

    /// Read the next ring packet if one was published since the last call.
    /// Returns `None` when nothing new. Mirrors `TryReadOutputFrame`.
    pub fn try_read(&mut self) -> Option<OutputFrame> {
        unsafe {
            let head = self.section.read_u32(0);
            if head == self.last_seq {
                return None;
            }

            let mut next_seq = self.last_seq.wrapping_add(1);

            // Fell more than RING_SLOTS behind → oldest packets overwritten;
            // skip ahead to the oldest still-readable slot.
            if head > next_seq.wrapping_add(OUTPUT_RING_SLOTS - 1) {
                next_seq = head.wrapping_sub(OUTPUT_RING_SLOTS).wrapping_add(1);
            }

            let slot_idx = (next_seq.wrapping_sub(1)) % OUTPUT_RING_SLOTS;
            let slot_base = OUTPUT_HEADER_SIZE + slot_idx as usize * OUTPUT_SLOT_SIZE;

            // Per-slot seqlock: SeqNo before, payload, SeqNo after; retry ≤4.
            let mut retries = 4;
            loop {
                let seq_before = self.section.read_seqno_volatile(slot_base + OUTPUT_SLOT_OFFSET_SEQNO);
                if seq_before != next_seq {
                    // Writer bumped Head but hasn't finished this slot's SeqNo
                    // yet — treat as nothing new, retry next poll.
                    return None;
                }
                let source = self.section.read_u8(slot_base + OUTPUT_SLOT_OFFSET_SOURCE);
                let report_id = self.section.read_u8(slot_base + OUTPUT_SLOT_OFFSET_REPORT_ID);
                let mut sz = self.section.read_u16(slot_base + OUTPUT_SLOT_OFFSET_SIZE) as usize;
                if sz > DATA_CAPACITY {
                    sz = DATA_CAPACITY;
                }
                let mut data = vec![0u8; sz];
                if sz > 0 {
                    self.section.copy_out(slot_base + OUTPUT_SLOT_OFFSET_DATA, &mut data);
                }
                fence(Ordering::Acquire);
                let seq_after = self.section.read_seqno_volatile(slot_base + OUTPUT_SLOT_OFFSET_SEQNO);
                if seq_after == seq_before {
                    self.last_seq = next_seq;
                    return Some(OutputFrame { source, report_id, data });
                }
                retries -= 1;
                if retries == 0 {
                    // Unstable read after retries — give up this round.
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_constants_match_driver() {
        // These are load-bearing — a regression here is exactly the kind of
        // off-by-N that shifts every field and corrupts input.
        assert_eq!(DATA_OFFSET, 8);
        assert_eq!(GIP_DATA_OFFSET, 264);
        assert_eq!(EXTENDED_SIZE_OFFSET, 278);
        assert_eq!(EXTENDED_DATA_OFFSET, 282);
        assert_eq!(SHARED_INPUT_SIZE, 362);
        assert_eq!(OUTPUT_SLOT_SIZE, 264);
        assert_eq!(SHARED_OUTPUT_SIZE, 16904);
    }
}
