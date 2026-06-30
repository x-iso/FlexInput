mod app;
mod canvas;
mod device_ops;
mod easy;
mod gamepad_nav;
mod kbm_picker;
mod guide_watcher;
mod panels;
mod panic_hotkey;
mod pin_hotkey;
pub mod process_list;
mod settings;

pub use app::{render_app_icon, FlexInputApp};
pub use canvas::UiPatch;

/// Append a timestamped crash/diagnostic entry to `%APPDATA%\FlexInput\crash.log`
/// — the same directory that holds `settings.json` and the recovery snapshot.
///
/// Used by the last-ditch panic hook in `app/src/main.rs` so any panic (GPU
/// loss, monitor hot-plug, or an unexpected one) leaves a durable breadcrumb the
/// user/support can read, instead of vanishing — the release build is
/// `windows_subsystem = "windows"` with no console attached. Writing to AppData
/// rather than next to the exe matters because the install dir is often
/// read-only (Program Files); AppData is always user-writable.
///
/// Best-effort and panic-safe: it must never itself panic from inside a panic
/// hook, so every step is fallible and silently ignored on error. The log is
/// appended (not truncated) and capped so it can't grow without bound.
pub fn log_crash(kind: &str, detail: &str) {
    let Some(dir) = settings::appdata_dir() else { return; };
    let path = dir.join("crash.log");

    // Cap the log: if it's grown past ~256 KiB, start fresh so a relaunch loop
    // can't fill the disk. (We can't easily rotate from a panic hook.)
    if std::fs::metadata(&path).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }

    use std::io::Write;
    // Wall-clock time without pulling in a date crate: seconds since the Unix
    // epoch is enough to correlate a crash with what the user was doing.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = format!("[unix:{secs}] {kind}\n{detail}\n\n");

    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(entry.as_bytes());
    }
}

/// Spawn a fresh copy of this executable, then exit the current process.
///
/// Used to recover from an unrecoverable GPU device loss: eframe 0.33 owns the
/// `wgpu` `RenderState` privately and exposes no API to rebuild the device in
/// place, so the only way back to a working window is a new process. The user's
/// latest work is already persisted via the always-on crash-recovery snapshot
/// (`settings::save_recovery`), which the fresh instance restores on boot — so
/// from the user's perspective the app blinks and reappears with their patch
/// intact, rather than crashing.
///
/// Called from two places: the normal-path `GPU_LOST` check in
/// `FlexInputApp::update`, and the last-ditch panic hook in `app/src/main.rs`
/// (for any device-loss path the vendored renderer didn't convert to a flag).
/// Both first persist the snapshot; this only handles the spawn-and-exit.
///
/// If spawning the child fails for any reason we still exit cleanly (code 0)
/// rather than leaving a half-dead window on a lost device — the recovery
/// snapshot is on disk, so a manual relaunch loses nothing.
pub fn relaunch_self_and_exit() -> ! {
    // Keep HIDMaestro virtual devices alive across the relaunch. This process is
    // the elevated helper's parent; when we exit, the helper's parent-death
    // teardown would (with persistence off) destroy the devices, and the fresh
    // process would then fail to talk to the dying helper (pipe "os error 233").
    // Flipping the helper to persist=on now makes it KEEP the devices and itself
    // through our death; the relaunched process reclaims them via the cross-run
    // reclaim path. The child is told (via env) that this is a GPU-recovery boot
    // so it doesn't run the startup wipe before reclaiming, and restores the
    // real persistence setting afterward. set_persist pushes the policy to the
    // helper synchronously (the GPU is gone but the helper pipe still works).
    #[cfg(windows)]
    flexinput_hidmaestro::helper::set_persist(true);

    if let Ok(exe) = std::env::current_exe() {
        // Detached child: we don't wait on it. On Windows this inherits no
        // console (the release build is `windows_subsystem = "windows"`), so
        // the new instance comes up as a normal windowed process.
        match std::process::Command::new(&exe)
            .env(GPU_RECOVERY_ENV, "1")
            .spawn()
        {
            Ok(_) => eprintln!("Relaunched FlexInput after GPU device loss: {}", exe.display()),
            Err(e) => eprintln!("Failed to relaunch FlexInput after GPU device loss: {e}"),
        }
    } else {
        eprintln!("Could not resolve current exe path for GPU-loss relaunch.");
    }
    // Exit the dead-GPU process. Use a hard exit: we're past the point where an
    // orderly eframe shutdown is safe (the device is gone), and `on_exit` would
    // otherwise delete the recovery snapshot the child needs.
    std::process::exit(0);
}

/// Env flag set on the relaunched child after a GPU-loss recovery. When present,
/// the child seeds helper persistence ON before creating devices (so the helper's
/// first Hello doesn't wipe the devices kept alive across the relaunch), reclaims
/// them, then applies the user's real persistence setting.
pub const GPU_RECOVERY_ENV: &str = "FLEXINPUT_GPU_RECOVERY";

/// Env flag set on a child relaunched because the GPU was lost *while a game owned
/// it* (FlexInput backgrounded). The child boots straight into the GUI-stall
/// state instead of attempting a full render against the game-held device — which
/// would just lose the device again and (if it came as a panic) relaunch in a
/// loop. The stalled child keeps its input/engine threads running and only
/// rebuilds the UI once FlexInput returns to the foreground. See
/// `FlexInputApp::update`'s GPU-loss block.
pub const GPU_STALL_ENV: &str = "FLEXINPUT_GPU_STALL";

/// Like [`relaunch_self_and_exit`], but marks the child to boot into the GUI-stall
/// state (see [`GPU_STALL_ENV`]). Used by the last-ditch panic hook when the loss
/// arrived as a panic while FlexInput was backgrounded: we can't resume a panicked
/// frame, so we relaunch — but the fresh process must not fight the game for the
/// GPU, so it starts stalled.
pub fn relaunch_self_stalled_and_exit() -> ! {
    #[cfg(windows)]
    flexinput_hidmaestro::helper::set_persist(true);
    if let Ok(exe) = std::env::current_exe() {
        match std::process::Command::new(&exe)
            .env(GPU_RECOVERY_ENV, "1")
            .env(GPU_STALL_ENV, "1")
            .spawn()
        {
            Ok(_) => eprintln!("Relaunched FlexInput (stalled) after backgrounded GPU loss: {}", exe.display()),
            Err(e) => eprintln!("Failed to relaunch FlexInput after backgrounded GPU loss: {e}"),
        }
    }
    std::process::exit(0);
}
