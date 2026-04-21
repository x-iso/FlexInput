use eframe::egui;
use flexinput_core::ModuleDescriptor;
use flexinput_devices::{init_backend, DeviceBackend, PhysicalDevice};
use flexinput_engine::Engine;
use flexinput_modules::all_modules;

use crate::{
    canvas::Canvas,
    panels::{physical_devices, virtual_devices::VirtualDevicePanel},
};

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // Load Segoe UI as a Unicode fallback — covers symbols egui's default font misses.
    // Primary font stays egui's built-in (better Latin hinting); Segoe UI fills the gaps.
    #[cfg(windows)]
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts.font_data.insert("segoe_ui".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        for family in fonts.families.values_mut() {
            family.push("segoe_ui".to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

pub struct FlexInputApp {
    engine: Engine,
    canvas: Canvas,
    descriptors: Vec<ModuleDescriptor>,
    device_backend: Option<Box<dyn DeviceBackend>>,
    devices: Vec<PhysicalDevice>,
    virtual_panel: VirtualDevicePanel,
}

impl FlexInputApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        let descriptors = all_modules().into_iter().map(|r| r.descriptor).collect();
        let device_backend = init_backend();
        Self {
            engine: Engine::new(),
            canvas: Canvas::new(),
            descriptors,
            device_backend,
            devices: vec![],
            virtual_panel: VirtualDevicePanel::new(),
        }
    }
}

impl eframe::App for FlexInputApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(backend) = &mut self.device_backend {
            self.devices = backend.enumerate();
            let _signals = backend.poll();
            // TODO: feed signals into the engine
        }

        self.engine.tick();

        // Submit HID reports for all active virtual devices.
        for dev in &mut self.virtual_panel.active {
            dev.flush();
        }

        let (virtual_panel, canvas) = (&mut self.virtual_panel, &mut self.canvas);
        egui::TopBottomPanel::top("virtual_devices_panel")
            .min_height(48.0)
            .resizable(true)
            .show(ctx, |ui| {
                virtual_panel.show(ui, canvas);
            });

        egui::TopBottomPanel::bottom("physical_devices_panel")
            .min_height(80.0)
            .default_height(220.0)
            .resizable(true)
            .show(ctx, |ui| {
                physical_devices::show(ui, &self.devices, canvas);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            crate::panels::canvas::show(&mut self.canvas, &self.descriptors, ui);
        });
    }
}
