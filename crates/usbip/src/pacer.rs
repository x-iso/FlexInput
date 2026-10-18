//! A wait that lands within a fraction of a millisecond and can be cut short.
//!
//! The isochronous engine completes one URB per millisecond of audio, and a
//! plain timed wait on Windows rounds up to the ~15.6 ms system tick. A
//! `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` timer (Win10 1803+) waits to ~0.5 ms
//! on its own, without raising the global timer resolution — the same choice
//! the engine's `HrWaiter` makes. Beside it sits an auto-reset event, so a new
//! submit can wake the pump before the timer fires.

use std::time::Duration;

#[cfg(windows)]
pub(crate) struct Pacer {
    timer: windows_sys::Win32::Foundation::HANDLE,
    event: windows_sys::Win32::Foundation::HANDLE,
}

// SAFETY: both handles are kernel objects, usable from any thread. Only the
// pump thread arms and waits on the timer; any thread may set the event.
#[cfg(windows)]
unsafe impl Send for Pacer {}
#[cfg(windows)]
unsafe impl Sync for Pacer {}

#[cfg(windows)]
impl Pacer {
    pub(crate) fn new() -> Self {
        use windows_sys::Win32::System::Threading::{
            CreateEventW, CreateWaitableTimerExW, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
            TIMER_ALL_ACCESS,
        };
        // SAFETY: FFI with null names and attributes. Pre-1803 Windows refuses
        // the HIGH_RESOLUTION flag, so fall back to an ordinary timer there.
        unsafe {
            let mut timer = CreateWaitableTimerExW(
                std::ptr::null(),
                std::ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            );
            if timer.is_null() {
                timer = CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), 0, TIMER_ALL_ACCESS);
            }
            let event = CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
            Pacer { timer, event }
        }
    }

    /// Cut the current (or next) wait short.
    pub(crate) fn wake(&self) {
        use windows_sys::Win32::System::Threading::SetEvent;
        if !self.event.is_null() {
            // SAFETY: FFI on our own event handle.
            unsafe { SetEvent(self.event) };
        }
    }

    /// Wait about `dur`, or until [`wake`](Self::wake). The timer may fire
    /// later with nobody waiting and stay signaled, so a later wait can return
    /// early: callers re-check their own deadline.
    pub(crate) fn wait(&self, dur: Duration) {
        use windows_sys::Win32::System::Threading::{
            SetWaitableTimer, WaitForMultipleObjects, WaitForSingleObject,
        };
        let ms = dur.as_millis().clamp(1, 60_000) as u32;
        // SAFETY: FFI on our own handles.
        unsafe {
            if self.timer.is_null() {
                if !self.event.is_null() {
                    WaitForSingleObject(self.event, ms);
                } else {
                    std::thread::sleep(dur);
                }
                return;
            }
            let due: i64 = -((dur.as_nanos() / 100).clamp(1, i64::MAX as u128) as i64);
            if SetWaitableTimer(self.timer, &due, 0, None, std::ptr::null(), 0) == 0 {
                std::thread::sleep(dur);
                return;
            }
            if self.event.is_null() {
                WaitForSingleObject(self.timer, ms + 50);
            } else {
                let handles = [self.event, self.timer];
                WaitForMultipleObjects(2, handles.as_ptr(), 0, ms + 50);
            }
        }
    }
}

#[cfg(windows)]
impl Drop for Pacer {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        // SAFETY: FFI; the handles are ours and unused after drop.
        unsafe {
            if !self.timer.is_null() {
                CloseHandle(self.timer);
            }
            if !self.event.is_null() {
                CloseHandle(self.event);
            }
        }
    }
}

#[cfg(not(windows))]
pub(crate) struct Pacer {
    woken: std::sync::Mutex<bool>,
    cv: std::sync::Condvar,
}

#[cfg(not(windows))]
impl Pacer {
    pub(crate) fn new() -> Self {
        Pacer { woken: std::sync::Mutex::new(false), cv: std::sync::Condvar::new() }
    }

    pub(crate) fn wake(&self) {
        *self.woken.lock().unwrap() = true;
        self.cv.notify_one();
    }

    pub(crate) fn wait(&self, dur: Duration) {
        let g = self.woken.lock().unwrap();
        let (mut g, _) = self.cv.wait_timeout_while(g, dur, |w| !*w).unwrap();
        *g = false;
    }
}
