use eframe::egui::{self, Color32};

/// Render a device-family SVG at a fixed height, cached in egui memory.
/// Mirrors the per-pin chip rasterization in `canvas::viewer::remapper_render_chip`.
pub(crate) fn render_device_icon(ui: &mut egui::Ui, bytes: &'static [u8], height: f32) {
    let size_px = (height * ui.ctx().pixels_per_point()).round() as u32;
    let cache_key = egui::Id::new(("device_icon", bytes.as_ptr() as usize, size_px));
    let tex = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(cache_key))
        .or_else(|| {
            let text = std::str::from_utf8(bytes).ok()?;
            let img = crate::canvas::viewer::rasterize_svg_recolored(
                text, size_px, size_px, "override", Color32::TRANSPARENT,
            )?;
            let handle = ui.ctx().load_texture(
                format!("device_icon_{:p}", bytes.as_ptr()),
                img,
                egui::TextureOptions::LINEAR,
            );
            ui.ctx().data_mut(|d| d.insert_temp(cache_key, handle.clone()));
            Some(handle)
        });
    if let Some(tex) = tex {
        ui.add(
            egui::Image::new(&tex)
                .fit_to_exact_size(egui::vec2(height, height))
                .tint(Color32::WHITE),
        );
    }
}

/// Like `render_device_icon` but the image is clickable and returns a
/// `Response`. Preserves SVG colors at rest; inverts pixel colors on hover.
/// Hover detection is per-instance (uses this frame's pointer position
/// against this widget's rect), so multiple buttons with the same SVG don't
/// share hover state.
pub(crate) fn svg_icon_button(
    ui: &mut egui::Ui,
    bytes: &'static [u8],
    height: f32,
) -> egui::Response {
    let size_px = (height * ui.ctx().pixels_per_point()).round() as u32;

    // Cache both variants once each.
    let tex_normal = load_svg_texture(ui, bytes, size_px, false);
    let tex_hover  = load_svg_texture(ui, bytes, size_px, true);

    // Allocate the rect first so we can probe pointer position against THIS
    // instance's rect, then pick which texture to display this frame.
    let size = egui::vec2(height, height);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let is_hover = ui.rect_contains_pointer(rect);

    let tex = if is_hover { tex_hover } else { tex_normal };
    let resp = match tex {
        Some(tex) => ui.put(
            rect,
            egui::ImageButton::new(
                egui::Image::new(&tex).fit_to_exact_size(size),
            )
            .frame(false),
        ),
        None => ui.interact(rect, ui.next_auto_id(), egui::Sense::click()),
    };
    resp
}

/// Device-card icon that doubles as a rumble "ping" button. Renders the family
/// SVG (color-inverts on hover for affordance), shows a pointer cursor and a
/// tooltip, and returns the click `Response`. Used on physical device cards in
/// both Easy and Advanced mode so the user can identify which pad is which.
pub(crate) fn ping_device_icon(
    ui: &mut egui::Ui,
    bytes: &'static [u8],
    height: f32,
) -> egui::Response {
    let resp = svg_icon_button(ui, bytes, height);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Ping (rumble)")
}

fn load_svg_texture(
    ui: &egui::Ui,
    bytes: &'static [u8],
    size_px: u32,
    invert: bool,
) -> Option<egui::TextureHandle> {
    let cache_key = egui::Id::new(("svg_icon_btn", bytes.as_ptr() as usize, size_px, invert));
    if let Some(h) = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(cache_key)) {
        return Some(h);
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut img = crate::canvas::viewer::rasterize_svg_recolored(
        text, size_px, size_px, "override", Color32::TRANSPARENT,
    )?;
    if invert {
        for px in img.pixels.iter_mut() {
            let a = px.a();
            if a == 0 { continue; }
            let af = a as f32 / 255.0;
            let r = (px.r() as f32 / af).min(255.0);
            let g = (px.g() as f32 / af).min(255.0);
            let b = (px.b() as f32 / af).min(255.0);
            *px = Color32::from_rgba_premultiplied(
                ((255.0 - r) * af) as u8,
                ((255.0 - g) * af) as u8,
                ((255.0 - b) * af) as u8,
                a,
            );
        }
    }
    let handle = ui.ctx().load_texture(
        format!("svg_icon_btn_{:p}_{}", bytes.as_ptr(), invert as u8),
        img,
        egui::TextureOptions::LINEAR,
    );
    ui.ctx().data_mut(|d| d.insert_temp(cache_key, handle.clone()));
    Some(handle)
}
