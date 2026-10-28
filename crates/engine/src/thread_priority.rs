//! Putting the input threads ahead of everything else on the machine.
//!
//! Windows does it with priorities a process may set itself: the device-I/O
//! thread runs TIME_CRITICAL, the engine ABOVE_NORMAL (see their spawn sites).
//! Linux lets an unprivileged process only lower its own priority, so it asks
//! RealtimeKit, the desktop's broker for exactly this (PipeWire gets its audio
//! priority from it), present on every mainstream distribution. rtkit hands
//! the active session's threads a nice level down to its floor (-15 unless
//! configured otherwise), which wins the CPU back from a busy renderer or a
//! browser at once. Not realtime scheduling: rtkit kills a realtime thread that
//! runs past its CPU-time limit without blocking, and the I/O loop's device
//! enumeration can run that long on a slow machine.

/// Raise the calling thread. Best effort: without rtkit (or a system bus) the
/// thread stays at normal priority and says so in the log.
pub fn raise_input_thread(name: &str) {
    #[cfg(target_os = "linux")]
    match linux::raise() {
        Ok(nice) => eprintln!("[priority] {name}: nice {nice} from rtkit"),
        Err(e) => eprintln!("[priority] {name}: stays at normal priority ({e})"),
    }
    #[cfg(not(target_os = "linux"))]
    let _ = name;
}

#[cfg(target_os = "linux")]
mod linux {
    use std::time::Duration;

    const RTKIT: &str = "org.freedesktop.RealtimeKit1";

    /// The nice level the calling thread got.
    pub(super) fn raise() -> Result<i32, Box<dyn std::error::Error>> {
        use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
        let conn = dbus::blocking::Connection::new_system()?;
        let rtkit = conn.with_proxy(RTKIT, "/org/freedesktop/RealtimeKit1", Duration::from_secs(2));
        // Its floor: the most it will grant.
        let nice: i32 = rtkit.get(RTKIT, "MinNiceLevel").unwrap_or(-15);
        // SAFETY: gettid has no preconditions.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u64;
        rtkit.method_call::<(), _, _, _>(RTKIT, "MakeThreadHighPriority", (tid, nice))?;
        Ok(nice)
    }
}
