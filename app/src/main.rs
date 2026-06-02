#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Installs a panic hook that intercepts the *one* unrecoverable GPU panic we
/// know about and exits cleanly instead of dumping a Rust backtrace.
///
/// When a fullscreen game resets the GPU (driver TDR / exclusive-fullscreen
/// mode switch), our D3D12 device is lost. The very next frame, egui-wgpu's
/// `write_buffer_with` returns `None` and the vendored renderer panics with
/// "Failed to create staging buffer ...". This panic exists verbatim in every
/// egui-wgpu release through 0.34.3 — upgrading does not fix it, and true
/// recovery would require recreating the whole `RenderState`, which egui-wgpu
/// does not expose. We pair this with `PowerPreference::LowPower` (see
/// `native_options` below) so the overlay lives on the integrated GPU and is
/// not caught in the game's device reset in the first place; this hook is the
/// safety net for the residual case (and for genuine VRAM exhaustion).
fn install_gpu_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.to_string();
        if msg.contains("Failed to create staging buffer") {
            let note = "GPU device was lost (another app, likely a fullscreen game, \
                 reset the graphics device). FlexInput cannot recover the render \
                 context and is exiting cleanly. Restart FlexInput after the game \
                 closes.";
            // The release build is `windows_subsystem = "windows"` with no logger
            // configured, so stderr is invisible. Drop a breadcrumb next to the
            // exe so the user/support can see why FlexInput vanished mid-game.
            eprintln!("{note}");
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    let _ = std::fs::write(dir.join("flexinput-gpu-lost.log"), note);
                }
            }
            std::process::exit(0);
        }
        default_hook(info);
    }));
}

fn main() -> eframe::Result<()> {
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
    // Prefer the integrated GPU over the discrete one. eframe defaults to
    // `PowerPreference::HighPerformance`, which puts our UI on the same
    // discrete GPU a fullscreen game is using. When that game triggers a
    // driver reset (TDR) or an exclusive-fullscreen mode switch, our device is
    // lost and the next frame panics inside egui-wgpu (see
    // `install_gpu_panic_hook`). A controller-overlay UI does not need the
    // discrete GPU, and the integrated GPU is not caught in the game's reset —
    // so `LowPower` removes the root cause for the common case. We start from
    // the default config and override only the power preference, preserving
    // `present_mode: AutoVsync` and the surface-error handler.
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.power_preference = eframe::wgpu::PowerPreference::LowPower;
    }

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
