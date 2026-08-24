//! One diagnostic log file, shared by both transports.
//!
//! Both the dongle and the Windows-stack hub write here, so a run that
//! exercises one and then the other reads as a single timeline. That is the
//! point: the two paths differ somewhere, and comparing them across two
//! files with independent clocks would hide exactly the difference being
//! looked for.

use std::time::Instant;

/// Where long-lived logs belong: `%APPDATA%\FlexInput\logs`.
///
/// ⛔ **Not beside the executable.** An installed app sits somewhere the user
/// cannot write, so the log either failed to open or landed in a temp directory
/// nobody was told about; and when it DID succeed it scattered files next to
/// the binary, which is not where anyone looks and not where an uninstall
/// cleans up. AppData is writable, per-user, and the same place the settings
/// and crash log already live.
pub(crate) fn log_dir() -> Option<std::path::PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let dir = std::path::PathBuf::from(appdata).join("FlexInput").join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Largest a log may grow before it is rotated.
///
/// ❗ A cap, not a nicety. These append for the life of a session, and a
/// discovery log on a pad that reconnects in a loop grows without bound —
/// unbounded diagnostics on a user's disk is a bug, not thoroughness. One
/// generation is kept, so the worst case is twice this.
pub(crate) const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// An open log that rotates itself once it reaches [`MAX_LOG_BYTES`].
///
/// ⛔ **Size is tracked as lines are written, not checked when the file is
/// opened.** A session that runs for days never revisits the open path, and
/// that is exactly the session whose log grows without bound — a controller
/// reconnecting in a loop writes discovery lines for as long as the app is up.
/// Checking only at startup caps nothing at all.
pub(crate) struct Sink {
    path: std::path::PathBuf,
    /// Lines queued for the writer thread.
    ///
    /// ⛔ **Bounded, and dropped when full — never blocking.** The sender is a
    /// transport thread servicing a controller at up to 200 Hz.
    tx: std::sync::mpsc::SyncSender<String>,
}

/// How many lines may queue before new ones are dropped.
///
/// Generous, because the writer only has to keep up on average, and small
/// enough that a wedged disk cannot consume real memory.
const QUEUE: usize = 4096;

impl Sink {
    /// Open `name`, honouring `env` (`off` disables), under `dir` with the
    /// temp directory as a fallback.
    fn open(name: &str, env: &str, beside_exe: bool) -> Option<Self> {
        let configured = std::env::var(env).ok();
        if configured.as_deref().is_some_and(|v| v.eq_ignore_ascii_case("off")) {
            return None;
        }
        let candidates: Vec<std::path::PathBuf> = match configured {
            Some(p) => vec![std::path::PathBuf::from(p)],
            None => {
                let mut v = Vec::new();
                if beside_exe {
                    if let Some(p) =
                        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name)))
                    {
                        v.push(p);
                    }
                }
                if let Some(dir) = log_dir() {
                    v.push(dir.join(name));
                }
                // Last resort: an install directory can be read-only, and a
                // diagnostic that silently produces nothing is worse than one
                // in an odd place — so long as the path is printed.
                v.push(std::env::temp_dir().join(name));
                v
            }
        };
        for path in candidates {
            rotate(&path);
            let Ok(file) = std::fs::File::create(&path) else { continue };
            let (tx, rx) = std::sync::mpsc::sync_channel::<String>(QUEUE);
            let thread_path = path.clone();
            // ⛔ **The file is owned by a thread of its own, and this is not
            // tidiness.** Writing happened inline on the transport thread, with
            // a `flush` per line — and the temporary diagnostic is written
            // BESIDE THE EXE, which on a development build sits inside a
            // OneDrive-synced tree. Every line was therefore a synchronous
            // fsync into a directory a sync client can lock for seconds, on the
            // thread servicing the controller. Reported from hardware as input
            // freezing for a few seconds at a time.
            //
            // A diagnostic that stalls the thing it is diagnosing does not just
            // cost performance, it manufactures the symptom being investigated.
            std::thread::Builder::new()
                .name("jc2-log".into())
                .spawn(move || {
                    let mut file = file;
                    let mut written: u64 = 0;
                    while let Ok(line) = rx.recv() {
                        use std::io::Write;
                        if written > MAX_LOG_BYTES {
                            let _ = file.flush();
                            rotate(&thread_path);
                            match std::fs::File::create(&thread_path) {
                                Ok(f) => {
                                    file = f;
                                    written = 0;
                                }
                                Err(_) => break,
                            }
                        }
                        if writeln!(file, "{line}").is_ok() {
                            written += line.len() as u64 + 1;
                        }
                        // ❗ Flushed when the queue goes quiet rather than per
                        // line: the data still reaches disk promptly, without
                        // an fsync for every entry.
                        if rx.try_recv().is_err() {
                            let _ = file.flush();
                        }
                    }
                    use std::io::Write;
                    let _ = file.flush();
                })
                .ok()?;
            return Some(Sink { path, tx });
        }
        None
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Queue a line. Never blocks, and never fails visibly: a dropped
    /// diagnostic line is preferable to a stalled controller.
    fn write(&self, line: &str) {
        let _ = self.tx.try_send(line.to_string());
    }
}

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
    static SINK: std::sync::OnceLock<Option<Sink>> = std::sync::OnceLock::new();
    let sink = SINK.get_or_init(|| {
        let s = Sink::open("jc2-dongle.log", "FLEXINPUT_JC2_LOG", false);
        match &s {
            Some(s) => eprintln!("[jc2-dongle] ⭐ discovery log (previous run kept as .prev): {}", s.path().display()),
            None => {}
        }
        s
    });
    let Some(sink) = sink else { return };
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed().as_millis();
    sink.write(&format!("{t} {args}"));
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
    static SINK: std::sync::OnceLock<Option<Sink>> = std::sync::OnceLock::new();
    let sink = SINK.get_or_init(|| {
        let s = Sink::open("jc2-drift.log", "FLEXINPUT_JC2_DRIFT_LOG", false);
        match &s {
            Some(s) => eprintln!("[jc2] ⭐ drift log: {}", s.path().display()),
            None => {}
        }
        s
    });
    let Some(sink) = sink else { return };
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed().as_secs();
    sink.write(&format!("{t} {args}"));
}

/// ⏳ **TEMPORARY.** The IMU diagnostic log, for one investigation.
///
/// A field report of a genuine Joy-Con 2 whose gyro drifts, on a build whose
/// constants were all measured against a third-party M12-S. Two explanations
/// fit that and they need opposite fixes:
///
/// * a zero-rate OFFSET, which accumulates while the controller sits still and
///   which per-controller calibration already corrects — but the tester ran
///   calibration and it did not help;
/// * a SCALE error, where [`crate::reports::ANGLE_COUNTS_PER_TURN`] is not the
///   same on retail hardware, which accumulates only when the controller MOVES
///   and which no amount of calibration touches, because calibration measures
///   an offset at rest.
///
/// The existing drift log cannot separate them: it requires stillness and
/// abandons the run the moment anything moves, so it is blind to precisely the
/// half now under suspicion. This records the moving half — cumulative
/// integrated rotation per field — so a tester who turns the controller through
/// a known angle and back shows, in one line, whether the integral returns to
/// where it started.
///
/// ❗ Beside the EXE, unlike the others, because it is temporary and meant to
/// be found, read and mailed by someone who is not going to be walked through
/// AppData. Delete this function and its call site when the question is
/// answered. `FLEXINPUT_JC2_IMU_LOG` overrides the path, `off` disables it.
pub(crate) fn imu(args: std::fmt::Arguments) {
    static SINK: std::sync::OnceLock<Option<Sink>> = std::sync::OnceLock::new();
    let sink = SINK.get_or_init(|| {
        let s = Sink::open("jc2-imu-diag.log", "FLEXINPUT_JC2_IMU_LOG", true);
        match &s {
            Some(s) => eprintln!("[jc2] ⏳ TEMPORARY IMU diagnostic log: {}", s.path().display()),
            None => {}
        }
        s
    });
    let Some(sink) = sink else { return };
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed().as_secs();
    sink.write(&format!("{t} {args}"));
}
