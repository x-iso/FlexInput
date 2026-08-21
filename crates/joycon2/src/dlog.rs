//! One diagnostic log file, shared by both transports.
//!
//! Both the dongle and the Windows-stack hub write here, so a run that
//! exercises one and then the other reads as a single timeline. That is the
//! point: the two paths differ somewhere, and comparing them across two
//! files with independent clocks would hide exactly the difference being
//! looked for.

use std::sync::Mutex;
use std::time::Instant;

/// Move an existing log aside before the new run truncates it.
///
/// ⭐ **Because the interesting run is always the one before the restart.**
/// Both logs were created with `File::create`, which truncates — so a session
/// that misbehaved was erased by the very next launch, which is exactly what a
/// user does after something goes wrong. Evidence of a glitch that took an hour
/// of play to provoke was destroyed by the restart that followed it.
///
/// One generation is kept, as `<name>.prev`. Two files is enough to cover
/// "it broke, I restarted, then I came to look", and never grows without bound.
fn rotate(path: &std::path::Path) {
    if path.metadata().map(|m| m.len() == 0).unwrap_or(true) {
        return; // nothing worth keeping
    }
    let mut prev = path.as_os_str().to_owned();
    prev.push(".prev");
    let _ = std::fs::rename(path, std::path::PathBuf::from(prev));
}

/// Append-only discovery log, written to a file.
///
/// ⭐ **Because "it took seven attempts" is not debuggable from the console.**
/// Discovery either sees a controller or it does not, and the console showed
/// neither — a failed pickup produced no output at all, so there was no way to
/// tell apart:
///
/// * the advertisement never arrived (radio, scan window, or the pad simply
///   not advertising yet),
/// * it arrived and was REJECTED by the matcher, which had six silent `return
///   None` paths and reported none of them,
/// * it matched and the connect failed,
/// * it connected and init failed.
///
/// Those need four different fixes, and guessing between them is what the last
/// several rounds have been.
///
/// Written to `jc2-dongle.log` beside the working directory. The previous
/// run is kept as `.prev` — see [`rotate`]. Override the path with
/// `FLEXINPUT_JC2_LOG`, or set it to `off` to disable.
pub(crate) fn dlog(args: std::fmt::Arguments) {
    use std::io::Write;
    static FILE: std::sync::OnceLock<Option<Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    let f = FILE.get_or_init(|| {
        let configured = std::env::var("FLEXINPUT_JC2_LOG").ok();
        if configured.as_deref().is_some_and(|v| v.eq_ignore_ascii_case("off")) {
            return None;
        }
        // ❗ ABSOLUTE, and announced. The first version wrote "jc2-dongle.log"
        // relative to the working directory and printed that bare name, which
        // is useless the moment the app is launched from anywhere but the
        // source tree — the file lands somewhere real and the user is told a
        // name they cannot find. Resolving it and printing the full path is the
        // difference between a log and a scavenger hunt.
        let candidates: Vec<std::path::PathBuf> = match configured {
            Some(p) => vec![std::path::PathBuf::from(p)],
            // Temp dir SECOND, not first: the working directory is where a
            // developer looks, but it is not always writable (an installed
            // app under Program Files is not), and a log that silently fails
            // to open is worse than one in an odd place.
            None => {
                let name = "jc2-dongle.log";
                let mut v = Vec::new();
                if let Ok(cwd) = std::env::current_dir() {
                    v.push(cwd.join(name));
                }
                v.push(std::env::temp_dir().join(name));
                v
            }
        };
        for path in candidates {
            rotate(&path);
            match std::fs::File::create(&path) {
                Ok(f) => {
                    eprintln!(
                        "[jc2-dongle] ⭐ discovery log: {} (previous run kept as .prev)",
                        path.display(),
                    );
                    return Some(Mutex::new(f));
                }
                Err(e) => eprintln!("[jc2-dongle] could not open {}: {e}", path.display()),
            }
        }
        eprintln!("[jc2-dongle] no writable location for the discovery log");
        None
    });
    let Some(f) = f else { return };
    // Milliseconds since the log opened: absolute wall time is noise here, and
    // the GAPS are the whole point -- how long after a wake the advertisement
    // arrives, and how long a connect attempt hangs before failing.
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed().as_millis();
    if let Ok(mut f) = f.lock() {
        let _ = writeln!(f, "[{t:>7} ms] {args}");
        let _ = f.flush();
    }
}

macro_rules! dlog {
    // ❗ `$crate::dlog::dlog`, not a bare `dlog`. A bare name resolves at the
    // CALL site, where — in every module except this one — it finds the macro
    // again rather than the function.
    ($($arg:tt)*) => { $crate::dlog::dlog(format_args!($($arg)*)) };
}


/// The DRIFT log, a separate file from the discovery one.
///
/// Kept apart deliberately. Discovery logging is a firehose — every
/// advertisement from every device in the room — and a drift reading is one
/// line every thirty seconds. Interleaving them buries the thing being measured
/// in traffic that has nothing to do with it, and the two are read at completely
/// different times for completely different reasons.
///
/// Written to `jc2-drift.log` beside the discovery log, same path rules and
/// the same one-generation rotation — see [`rotate`].
/// `FLEXINPUT_JC2_DRIFT_LOG` overrides, `off` disables.
pub(crate) fn drift(args: std::fmt::Arguments) {
    use std::io::Write;
    static FILE: std::sync::OnceLock<Option<Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    let f = FILE.get_or_init(|| {
        let configured = std::env::var("FLEXINPUT_JC2_DRIFT_LOG").ok();
        if configured.as_deref().is_some_and(|v| v.eq_ignore_ascii_case("off")) {
            return None;
        }
        let candidates: Vec<std::path::PathBuf> = match configured {
            Some(p) => vec![std::path::PathBuf::from(p)],
            None => {
                let name = "jc2-drift.log";
                let mut v = Vec::new();
                if let Ok(cwd) = std::env::current_dir() {
                    v.push(cwd.join(name));
                }
                v.push(std::env::temp_dir().join(name));
                v
            }
        };
        for path in candidates {
            rotate(&path);
            if let Ok(f) = std::fs::File::create(&path) {
                eprintln!("[jc2] ⭐ drift log: {}", path.display());
                return Some(Mutex::new(f));
            }
        }
        None
    });
    let Some(f) = f else { return };
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed().as_secs();
    if let Ok(mut f) = f.lock() {
        let _ = writeln!(f, "[{t:>6} s] {args}");
        let _ = f.flush();
    }
}
