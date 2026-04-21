use flexinput_core::ModuleDescriptor;

use crate::canvas::Canvas;

pub fn show(canvas: &mut Canvas, descriptors: &[ModuleDescriptor], ui: &mut egui::Ui) {
    canvas.show(descriptors, ui);
}
