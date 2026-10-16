//! High-resolution periodic waiter.
//!
//! Replaces `timeBeginPeriod(1)` + `thread::sleep` for the tight input loops (the
//! engine tick and the device-I/O poll). `timeBeginPeriod` raises the GLOBAL
//! system timer resolution to 1 ms, which (a) on Windows 11 is honored only while
//! the process is in the foreground — a backgrounded process reverts to the
//! ~15.6 ms default, collapsing the loops to ~64 Hz — and (b) system-wide, adds
//! DPC latency that can stutter other high-rate input devices (e.g. an I2C-HID
//! laptop trackpad).
//!
//! A `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` timer (Win10 1803+) gives ~0.5 ms
//! wait precision on a PER-TIMER basis, without touching the global timer
//! resolution — so no system-wide penalty, and not subject to the foreground-only
//! throttle. Falls back to `thread::sleep` when the timer can't be created (older
//! Windows) or off-Windows.

use std::time::Duration;

#[cfg(windows)]
pub struct HrWaiter {
    // HANDLE (*mut c_void); null when creation failed → fall back to thread::sleep.
    // Created and used on ONE thread (never sent across), so the raw handle is fine.
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl HrWaiter {
    pub fn new() -> Self {
        use windows_sys::Win32::System::Threading::{
            CreateWaitableTimerExW, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS,
        };
        // SAFETY: FFI. Null name/attributes; the HIGH_RESOLUTION flag is ignored
        // (and creation fails → null) on pre-1803 Windows, where we fall back.
        let handle = unsafe {
            CreateWaitableTimerExW(
                std::ptr::null(),
                std::ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        Self { handle }
    }

    /// Block the current thread for approximately `dur` using the high-resolution
    /// timer. No-op for a zero duration.
    pub fn wait(&self, dur: Duration) {
        use windows_sys::Win32::System::Threading::{SetWaitableTimer, WaitForSingleObject, INFINITE};
        if dur.is_zero() {
            return;
        }
        if self.handle.is_null() {
            std::thread::sleep(dur);
            return;
        }
        // Relative due time in 100 ns units; negative = relative to now.
        let hundred_ns = (dur.as_nanos() / 100).max(1).min(i64::MAX as u128) as i64;
        let due: i64 = -hundred_ns;
        // SAFETY: FFI on a valid handle. No completion routine, single-shot period.
        let armed = unsafe {
            SetWaitableTimer(self.handle, &due, 0, None, std::ptr::null(), 0)
        };
        if armed == 0 {
            std::thread::sleep(dur);
            return;
        }
        // SAFETY: FFI; wait until the timer signals.
        unsafe {
            WaitForSingleObject(self.handle, INFINITE);
        }
    }

    /// Block until `event` is signaled or about `dur` has passed, whichever is
    /// first — with this timer's precision, where a plain timed wait would
    /// round up to the system tick (~15.6 ms). True if the event fired.
    ///
    /// The timer may still go off later with nobody waiting and stay signaled,
    /// so a later wait on it can return early: callers loop on their own
    /// condition and deadline rather than trusting a timeout.
    pub fn wait_event(&self, event: windows_sys::Win32::Foundation::HANDLE, dur: Duration) -> bool {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{
            SetWaitableTimer, WaitForMultipleObjects, WaitForSingleObject, INFINITE,
        };
        // SAFETY: FFI on valid handles owned by the caller / us.
        unsafe {
            if dur.is_zero() {
                return WaitForSingleObject(event, 0) == WAIT_OBJECT_0;
            }
            let hundred_ns = (dur.as_nanos() / 100).max(1).min(i64::MAX as u128) as i64;
            let due: i64 = -hundred_ns;
            if self.handle.is_null()
                || SetWaitableTimer(self.handle, &due, 0, None, std::ptr::null(), 0) == 0
            {
                // No high-resolution timer: a millisecond timeout, rounded up.
                let ms = dur.as_millis().max(1).min(u32::MAX as u128 - 1) as u32;
                return WaitForSingleObject(event, ms) == WAIT_OBJECT_0;
            }
            let handles = [event, self.handle];
            WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) == WAIT_OBJECT_0
        }
    }
}

#[cfg(windows)]
impl Drop for HrWaiter {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: FFI; the handle was created by us and isn't used after drop.
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(not(windows))]
pub struct HrWaiter;

#[cfg(not(windows))]
impl HrWaiter {
    pub fn new() -> Self {
        Self
    }
    pub fn wait(&self, dur: Duration) {
        std::thread::sleep(dur);
    }
}

impl Default for HrWaiter {
    fn default() -> Self {
        Self::new()
    }
}
