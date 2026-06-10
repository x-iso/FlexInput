mod app;
mod canvas;
mod easy;
mod gamepad_nav;
mod kbm_picker;
mod guide_watcher;
mod panels;
mod panic_hotkey;
mod pin_hotkey;
mod process_list;
mod settings;

pub use app::{render_app_icon, FlexInputApp};
pub use canvas::UiPatch;

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
    if let Ok(exe) = std::env::current_exe() {
        // Detached child: we don't wait on it. On Windows this inherits no
        // console (the release build is `windows_subsystem = "windows"`), so
        // the new instance comes up as a normal windowed process.
        match std::process::Command::new(&exe).spawn() {
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
