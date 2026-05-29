#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
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
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("FlexInput")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_transparent(true)
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "FlexInput",
        native_options,
        Box::new(|cc| Ok(Box::new(flexinput_ui::FlexInputApp::new(cc)))),
    )
}
