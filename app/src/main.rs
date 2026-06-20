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
        // Device-reset signatures. When another app (usually a fullscreen game)
        // resets the graphics device, wgpu objects created against the old
        // device become invalid; the next frame's egui texture upload then
        // panics from a context our in-renderer GPU_LOST guard didn't cover.
        // Match those signatures here so the *last-ditch* net still relaunches
        // instead of dumping a backtrace.
        //   - "Failed to create staging buffer" / "GPU device lost": older paths.
        //   - egui_texid_Managed(N) ... is invalid: a managed egui texture
        //     whose backing texture was orphaned by the reset (seen as
        //     `Texture::create_view` / bind-group validation errors).
        //   - "Device is lost" / "device was lost": wgpu's own DeviceLost text.
        // We intentionally do NOT treat every "Validation Error" as device
        // loss — only ones carrying a managed-egui-texture-invalid or
        // device-lost signature — so a genuine logic bug can't loop-relaunch.
        let gpu_lost = msg.contains("Failed to create staging buffer")
            || msg.contains("GPU device lost")
            || msg.contains("Device is lost")
            || msg.contains("device was lost")
            || (msg.contains("egui_texid_Managed") && msg.contains("is invalid"));

        // Monitor hot-plug: when a display disconnects/reconnects (e.g. a monitor
        // briefly drops, a KVM switch, or a GPU re-scan), winit's monitor
        // enumeration can `unwrap()` an `Invalid monitor handle` (Win32 err 1461)
        // on a stale `HMONITOR` from inside its own event loop — a context we
        // can't wrap in `catch_unwind`. It's transient: a fresh process
        // re-enumerates monitors cleanly. So relaunch + restore from the recovery
        // snapshot rather than dying. Match the OS error text (stable across winit
        // patch versions) and, defensively, the winit monitor source path.
        let monitor_lost = msg.contains("Invalid monitor handle")
            || (msg.contains("monitor.rs") && msg.contains("1461"));
        if monitor_lost && !gpu_lost {
            let note = "A display was disconnected/reconnected and the windowing \
                 layer hit an invalid monitor handle. FlexInput is relaunching to \
                 recover; your patch is restored from the crash-recovery snapshot.";
            eprintln!("{note}");
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    let _ = std::fs::write(dir.join("flexinput-monitor-lost.log"), note);
                }
            }
            // Unlike GPU loss, no game owns the device here — a plain relaunch is
            // correct (the fresh process enumerates monitors fine and rebuilds the
            // window immediately).
            flexinput_ui::relaunch_self_and_exit();
        }

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
            //
            // If we were NOT the foreground window, a game owns the GPU. We
            // can't resume a panicked frame, so we must relaunch — but the
            // fresh process must not render against the game-held device (it'd
            // lose it again and loop). Boot it straight into the GUI stall; it
            // keeps input/engine alive and rebuilds the UI once FlexInput is
            // foreground again.
            #[cfg(windows)]
            {
                if flexinput_ui::process_list::foreground_exe().is_some() {
                    flexinput_ui::relaunch_self_stalled_and_exit();
                }
            }
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
