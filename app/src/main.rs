#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

const ICON: &[u8] = include_bytes!("../assets/icon.png");

fn main() -> eframe::Result<()> {
    let icon = eframe::icon_data::from_png_bytes(ICON).expect("bundled icon is valid PNG");

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("FlexInput")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "FlexInput",
        native_options,
        Box::new(|cc| Ok(Box::new(flexinput_ui::FlexInputApp::new(cc, ICON)))),
    )
}
