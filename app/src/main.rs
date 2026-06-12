#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Installs a panic hook that intercepts an unrecoverable GPU panic and
/// relaunches FlexInput instead of dumping a Rust backtrace.
///
/// When a fullscreen game resets the GPU (driver TDR / exclusive-fullscreen
/// mode switch), our device can be lost. The vendored egui-wgpu (see
/// `vendor/egui-wgpu/src/renderer.rs`) normally catches this case, raises the
/// `GPU_LOST` flag instead of panicking, and `FlexInputApp::update` then saves
/// state and relaunches gracefully. This hook is the *last-ditch net* for any
/// device-loss path that still reaches a panic — older "Failed to create
/// staging buffer" messages, or a panic raised from a context our flag check
/// hasn't run in yet.
///
/// We pair both with `PowerPreference::LowPower` (see `native_options` below)
/// so the overlay lives on the integrated GPU and is not caught in the game's
/// device reset in the first place — that removes the root cause for the common
/// case. The user's latest work is already on disk via the always-on recovery
/// snapshot, so the relaunched instance restores it seamlessly.
fn install_gpu_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.to_string();
        let gpu_lost = msg.contains("Failed to create staging buffer")
            || msg.contains("GPU device lost");
        if gpu_lost {
            // The release build is `windows_subsystem = "windows"` with no
            // logger surfaced to a console, so drop a breadcrumb next to the
            // exe for the user/support to see why FlexInput blinked mid-game.
            let note = "GPU device was lost (another app, likely a fullscreen game, \
                 reset the graphics device). FlexInput is relaunching to recover; \
                 your patch is restored from the crash-recovery snapshot.";
            eprintln!("{note}");
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    let _ = std::fs::write(dir.join("flexinput-gpu-lost.log"), note);
                }
            }
            // Spawn a fresh instance and exit this dead-GPU process. The
            // recovery snapshot the child restores from was written by the
            // app's autosave-on-settle (and forced on the GPU_LOST path); we
            // do NOT delete it here — the child consumes it on boot.
            flexinput_ui::relaunch_self_and_exit();
        }
        default_hook(info);
    }));
}

/// If launched as the elevated HIDMaestro helper (`--hidmaestro-helper`), run
/// the named-pipe server and exit *before* touching the GPU / window. The app
/// re-execs itself with this flag (see `flexinput_hidmaestro::helper`) so a
/// single binary ships instead of a separate helper exe.
///
/// Returns true if this process was the helper (caller should exit).
#[cfg(windows)]
fn run_as_helper_if_requested() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == flexinput_hidmaestro::helper::HELPER_FLAG) {
        return false;
    }
    let mut parent_pid: Option<u32> = None;
    let mut persist = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--parent-pid" => parent_pid = it.next().and_then(|s| s.parse().ok()),
            "--persist" => persist = true,
            _ => {}
        }
    }
    flexinput_hidmaestro::run_helper_server(parent_pid, persist);
    true
}

#[cfg(not(windows))]
fn run_as_helper_if_requested() -> bool {
    false
}

fn main() -> eframe::Result<()> {
    // Helper mode short-circuits everything else (no GPU, no window).
    if run_as_helper_if_requested() {
        return Ok(());
    }

    install_gpu_panic_hook();

    // Window / taskbar icon — decoded from the pre-baked 256px logo PNG
    // (rendered from icon_v2.svg). Decoding is instant; rasterizing the
    // source SVG at 256px takes ~45s and was stalling startup.
    let icon = flexinput_ui::render_app_icon().expect("bundled app icon PNG is valid");

    // Transparent viewport is enabled at startup so the runtime "see-through"
    // toggle (eye icon next to the zoom controls) can show whatever is behind
    // FlexInput. With `with_transparent(true)` the compositor allocates an
    // RGBA surface; whether anything bleeds through is controlled per-frame
    // by the alpha values in panel/window fills (see
    // `settings::apply_theme_and_contrast`).
    // GPU selection: use eframe's default (`PowerPreference::HighPerformance`),
    // i.e. the discrete GPU on a dual-GPU machine.
    //
    // We previously forced `LowPower` to dodge a known crash: when a fullscreen
    // game on the discrete GPU triggers a driver reset (TDR) or an exclusive-
    // fullscreen mode switch, our device is lost and egui-wgpu used to `panic!`
    // the whole process on the next frame. That root-cause avoidance is no
    // longer necessary — the vendored egui-wgpu now turns device loss into a
    // skipped frame + `GPU_LOST` flag, and FlexInput saves its workspace and
    // relaunches cleanly (see `install_gpu_panic_hook`, the `GPU_LOST` check in
    // `FlexInputApp::update`, and `vendor/egui-wgpu/src/renderer.rs`).
    //
    // Running on the discrete GPU deliberately keeps us in the device-reset
    // blast radius, which is the worst case for the recovery path — the best
    // way to keep that path honest. The plain default config preserves
    // `present_mode: AutoVsync` and the surface-error handler.
    let wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("FlexInput")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_transparent(true)
            .with_icon(icon),
        wgpu_options,
        ..Default::default()
    };

    eframe::run_native(
        "FlexInput",
        native_options,
        Box::new(|cc| Ok(Box::new(flexinput_ui::FlexInputApp::new(cc)))),
    )
}
