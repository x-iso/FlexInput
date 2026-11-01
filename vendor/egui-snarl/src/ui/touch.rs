//! FLEXINPUT PATCH: touchscreen support for the graph and the menus around it.
//!
//! A finger needs bigger targets than a mouse, but a mouse user on a touch laptop
//! shouldn't get them. So sizes follow whichever the user touched last, rather than
//! whether the machine has a touchscreen at all.

use egui::{
    Color32, Context, Event, Id, InnerResponse, Margin, Painter, Popup, Pos2, Response, ScrollArea,
    Shape, Stroke, Style, Ui, Vec2, containers::menu, response::Flags, style::ScrollStyle, vec2,
};

/// The radius of the ring that fills up while a finger holds something, big enough to show
/// around the finger, in points on screen.
const HOLD_RING_RADIUS: f32 = 34.0;

/// Did the user last use the touchscreen, rather than a mouse?
///
/// egui-winit sends each touch as a `Touch` event together with an emulated pointer
/// event, so a frame with pointer events and no `Touch` is a real mouse. Windows sends a
/// mouse message for each touch too; the vendored winit drops those.
pub fn is_active(ctx: &Context) -> bool {
    let id = Id::new("flexinput_touch_active");
    let seen = ctx.input(|i| {
        let mut pointer = false;
        for event in &i.events {
            match event {
                Event::Touch { .. } => return Some(true),
                Event::PointerMoved(_) | Event::PointerButton { .. } => pointer = true,
                _ => {}
            }
        }
        pointer.then_some(false)
    });
    match seen {
        Some(active) => {
            ctx.data_mut(|d| d.insert_temp(id, active));
            active
        }
        None => ctx.data(|d| d.get_temp(id)).unwrap_or(false),
    }
}

/// A widget is working with more than one finger, so the canvas under it shouldn't pinch.
/// Good for the frame after this one too, as the canvas reads it before drawing its nodes.
///
/// Per viewport, as pass numbers are: one window's stamp means nothing in another's count,
/// and a sub-patch editor counts far behind the main window.
pub fn block_pinch(ctx: &Context) {
    let (pass, key) = (ctx.cumulative_pass_nr(), pinch_key(ctx));
    ctx.data_mut(|d| d.insert_temp(key, pass));
}

pub(crate) fn pinch_blocked(ctx: &Context) -> bool {
    let (pass, key) = (ctx.cumulative_pass_nr(), pinch_key(ctx));
    ctx.data(|d| d.get_temp::<u64>(key))
        .is_some_and(|blocked| pass.saturating_sub(blocked) <= 1)
}

fn pinch_key(ctx: &Context) -> Id {
    Id::new("flexinput_pinch_blocked").with(ctx.viewport_id())
}

/// The ring that fills up around what a finger is holding. Stays away for the first
/// moments of a touch, so that it doesn't flash on every drag. `scale` is the layer's, to
/// keep its size on screen whatever the zoom.
pub fn paint_hold_ring(painter: &Painter, center: Pos2, scale: f32, progress: f32, color: Color32) {
    const SHOW_AFTER: f32 = 0.15;
    if progress < SHOW_AFTER {
        return;
    }
    let radius = HOLD_RING_RADIUS / scale;
    let filled = (progress - SHOW_AFTER) / (1.0 - SHOW_AFTER);
    let points = (0..=48)
        .map(|n| {
            let angle = std::f32::consts::TAU * filled * n as f32 / 48.0;
            center + radius * Vec2::angled(angle - std::f32::consts::FRAC_PI_2)
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(4.0 / scale, color)));
}

/// The menu style for a finger: egui's, with rows about 10 mm tall at 96 DPI.
pub fn menu_style(style: &mut Style) {
    menu::menu_style(style);
    let spacing = &mut style.spacing;
    spacing.button_padding = vec2(14.0, 9.0);
    spacing.interact_size.y = 40.0;
    spacing.item_spacing = vec2(8.0, 4.0);
    spacing.menu_margin = Margin::same(8);
    spacing.icon_width *= 1.4;
    spacing.icon_width_inner *= 1.4;
    // A bar that says there's more to scroll to. egui's default floats, and stays hidden
    // until a pointer comes near, which a finger on a screen doesn't do.
    spacing.scroll = ScrollStyle {
        bar_width: 8.0,
        ..ScrollStyle::solid()
    };
    for font in style.text_styles.values_mut() {
        font.size *= 1.25;
    }
}

/// The style a menu opened now should use. A menu passes its style on to its submenus.
pub fn menu_style_for(ctx: &Context) -> fn(&mut Style) {
    if is_active(ctx) {
        menu_style
    } else {
        menu::menu_style
    }
}

/// A menu's contents: in a menu for a finger, whose rows take a lot of room, these scroll
/// rather than running off the screen.
pub fn menu_scroll<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    if !is_active(ui.ctx()) {
        return content(ui);
    }
    let max_height = ui.ctx().content_rect().height() - 32.0;
    ScrollArea::vertical().max_height(max_height).show(ui, content).inner
}

/// [`Response::context_menu`], sized for the input that opened it. A finger resting while
/// another moves is anchoring a pinch, not asking for a menu, whatever egui's long touch
/// makes of it.
pub fn context_menu<R>(
    response: &Response,
    content: impl FnOnce(&mut Ui) -> R,
) -> Option<InnerResponse<R>> {
    let style = menu_style_for(&response.ctx);
    if response.long_touched() && response.ctx.input(|i| i.multi_touch().is_some()) {
        let mut response = response.clone();
        response.flags.remove(Flags::LONG_TOUCHED);
        return Popup::context_menu(&response).style(style).show(|ui| menu_scroll(ui, content));
    }
    Popup::context_menu(response).style(style).show(|ui| menu_scroll(ui, content))
}
