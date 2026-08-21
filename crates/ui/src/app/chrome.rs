//! Window chrome: fonts, the inner-window shadow, the tab bar, the custom
//! title bar, and the mode pills / eye toggle that live on it.

use super::*;

pub(crate) fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    #[cfg(windows)]
    {
        if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
            fonts.font_data.insert("segoe_ui".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            for family in fonts.families.values_mut() {
                family.push("segoe_ui".to_owned());
            }
        }
        // Segoe UI Symbol provides arrows/symbols (↶ ↷) not covered by Segoe UI.
        if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\seguisym.ttf") {
            fonts.font_data.insert("segoe_sym".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            for family in fonts.families.values_mut() {
                family.push("segoe_sym".to_owned());
            }
        }
    }
    ctx.set_fonts(fonts);
}

/// ctx-data slot holding the rect the inner-shadow gradient should hug.
/// Written during the CentralPanel pass: the content area below the tab
/// strip in Easy mode, or the canvas rect in Advanced mode. Read by
/// `paint_inner_window_shadow` after the frame's panels are laid out.
pub(crate) const INNER_SHADOW_RECT_KEY: &str = "flexinput::inner_shadow_rect";

/// Paint a pronounced inner-edge shadow: a true gradient that darkens
/// the edges of the *content* rect and fades smoothly to transparent
/// toward the center. Only painted while see-through is active — its
/// job is to ground the window dimensions against the desktop bleeding
/// through; in opaque mode the solid surfaces already read as edges.
///
/// The gradient is a real triangle mesh (an outer ring of vertices at
/// peak alpha, an inner ring at zero alpha) so the GPU interpolates the
/// alpha per-pixel — no visible stepping like a stack of strokes.
///
/// `content_rect` comes from `INNER_SHADOW_RECT_KEY`: the area below the
/// tab strip (Easy mode) or the canvas rect (Advanced mode).
pub(crate) fn paint_inner_window_shadow(ctx: &egui::Context) {
    use egui::epaint::{Mesh, Vertex};
    if !ctx.data(|d| d.get_temp::<bool>(
        egui::Id::new(crate::canvas::SEE_THROUGH_DATA_KEY)))
        .unwrap_or(false)
    {
        return;
    }
    let Some(rect) = ctx.data(|d| d.get_temp::<egui::Rect>(
        egui::Id::new(INNER_SHADOW_RECT_KEY)))
    else { return };
    if rect.width() <= 0.0 || rect.height() <= 0.0 { return; }

    // Pronounced: a ~28px band fading from opaque-ish black at the
    // edge to fully transparent inward.
    const BAND: f32 = 28.0;
    const PEAK_ALPHA: u8 = 130;
    let band = BAND.min(rect.width() * 0.5).min(rect.height() * 0.5);
    if band <= 0.0 { return; }

    let outer = rect;
    let inner = rect.shrink(band);
    let edge = egui::Color32::from_black_alpha(PEAK_ALPHA);
    let center = egui::Color32::TRANSPARENT;
    let uv = egui::epaint::WHITE_UV;

    let mut mesh = Mesh::default();
    // 8 vertices: 4 outer corners (edge color) then 4 inner corners
    // (transparent). The GPU interpolates alpha across each band.
    let mut push = |p: egui::Pos2, c: egui::Color32| {
        let idx = mesh.vertices.len() as u32;
        mesh.vertices.push(Vertex { pos: p, uv, color: c });
        idx
    };
    let o_tl = push(outer.left_top(), edge);
    let o_tr = push(outer.right_top(), edge);
    let o_br = push(outer.right_bottom(), edge);
    let o_bl = push(outer.left_bottom(), edge);
    let i_tl = push(inner.left_top(), center);
    let i_tr = push(inner.right_top(), center);
    let i_br = push(inner.right_bottom(), center);
    let i_bl = push(inner.left_bottom(), center);

    // Four trapezoidal bands (top, right, bottom, left), each two tris,
    // with edge verts at peak alpha and inner verts transparent.
    for &[a, b, c, d] in &[
        [o_tl, o_tr, i_tr, i_tl], // top
        [o_tr, o_br, i_br, i_tr], // right
        [o_br, o_bl, i_bl, i_br], // bottom
        [o_bl, o_tl, i_tl, i_bl], // left
    ] {
        mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
    }

    // Background order: this runs AFTER the CentralPanel pass, so within
    // the Background layer the shadow paints over the canvas / sub-patch
    // body content, but Background sits below `Middle` (floating windows)
    // and `Foreground` (menus / popups) — so the shadow stays behind all
    // of those rather than bleeding over them.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("app_inner_shadow"),
    ));
    painter.add(mesh);
}

// ── Tab bar ───────────────────────────────────────────────────────────────────

/// Returns (switch_to_idx, close_tab_idx, new_tab_requested, bypass_toggle_idx).
/// Actions a single tab-bar frame can request. Bundled into a struct
/// (rather than a wide tuple) now that the File menu / Auto-switch
/// toggle live here alongside the tab list.
pub(crate) struct TabBarActions {
    pub(crate) switch_to: Option<usize>,
    pub(crate) close_idx: Option<usize>,
    pub(crate) new_tab: bool,
    pub(crate) bypass_toggle: Option<usize>,
    pub(crate) do_save: bool,
    pub(crate) do_load: bool,
    pub(crate) do_save_workspace: bool,
    pub(crate) do_load_workspace: bool,
    pub(crate) do_bind: bool,
    pub(crate) do_close: bool,
}

pub(crate) fn show_tab_bar(
    ui: &mut egui::Ui,
    tabs: &[PatchTab],
    active_tab: usize,
    effective_bypass: &[bool],
    auto_switch: &mut bool,
) -> TabBarActions {
    let mut switch_to: Option<usize> = None;
    let mut close_idx: Option<usize> = None;
    let mut new_tab = false;
    let mut bypass_toggle: Option<usize> = None;
    let mut do_save = false;
    let mut do_load = false;
    let mut do_save_workspace = false;
    let mut do_load_workspace = false;
    let mut do_bind = false;
    let mut do_close = false;

    let h = ui.available_height();
    let text_color  = ui.visuals().text_color();
    let hover_fill  = ui.visuals().widgets.hovered.bg_fill;
    let sep_color   = ui.visuals().widgets.noninteractive.bg_stroke.color;
    // Darker than the panel background so the selected tab visibly recedes.
    let panel_fill  = ui.visuals().window_fill();
    let darken = |c: egui::Color32, n: i16| {
        let f = |v: u8| (v as i16 - n).clamp(0, 255) as u8;
        egui::Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
    };
    let active_fill = darken(panel_fill, 22);
    let font_id     = egui::FontId::proportional(13.0);

    // The bar splits into two regions on one horizontal row:
    //   1. A PINNED left cluster (File menu + Auto toggle + divider) that
    //      never scrolls, sitting on a solid #1B1B1B "Side Shadow"
    //      backdrop.
    //   2. A horizontal ScrollArea holding only the tabs + "+" button.
    // Tabs scrolling toward the divider fade into the Side Shadow; a
    // right-edge fade appears when more tabs overflow off-screen.
    let panel_outer = ui.max_rect();
    // #1B1B1B from the mockup's Side Shadow gradient.
    let shadow_solid = egui::Color32::from_rgb(27, 27, 27);

    // `tabs_left` is filled in after the pinned cluster lays out; we need
    // it both for the ScrollArea origin and the shadow geometry.
    let mut tabs_left = panel_outer.left();

    // Use horizontal_centered so EVERY item (the short File/Auto widgets
    // AND the full-height tab rects) is vertically centered against the
    // full row height — plain `horizontal()` top-aligns items placed
    // before the tall tabs, which is what made File/Auto ride high.
    ui.horizontal_centered(|ui| {
        // Reserve a paint slot for the Side Shadow backdrop NOW, so it
        // renders UNDER the File/Auto widgets that follow (same layer,
        // earlier z). We fill it once `tabs_left` is known below.
        let shadow_idx = ui.painter().add(egui::Shape::Noop);

        // ── 1. Pinned File menu + Auto-switch cluster ──────────────────
        ui.add_space(8.0);
        ui.menu_button("File", |ui| {
            if ui.button("New").clicked()                       { new_tab = true; ui.close(); }
            if ui.button("Save Patch…").clicked()               { do_save  = true; ui.close(); }
            if ui.button("Load Patch…").clicked()               { do_load  = true; ui.close(); }
            ui.separator();
            if ui.button("Save Workspace…").clicked()           { do_save_workspace = true; ui.close(); }
            if ui.button("Load Workspace…").clicked()           { do_load_workspace = true; ui.close(); }
            ui.separator();
            if ui.button("Bind Tab to Process…").clicked()      { do_bind  = true; ui.close(); }
            ui.separator();
            if ui.button("Close Tab").clicked()                 { do_close = true; ui.close(); }
        });

        ui.add_space(6.0);

        // Auto-switch toggle button. Rendered as a single selectable
        // widget: "Auto" text + a filled circle (same style as the tab
        // activity/bypass dot) that follows the current text color. Using
        // a constant-size painted dot — rather than swapping ●/○ glyphs —
        // keeps the button width fixed so toggling never shifts the row.
        let auto_hover = if *auto_switch {
            "Auto-switch ON — tabs switch when a bound process gains focus"
        } else {
            "Auto-switch OFF — tab switching is manual"
        };
        {
            let font_id = egui::TextStyle::Button.resolve(ui.style());
            let galley = ui.painter().layout_no_wrap(
                "Auto".to_owned(), font_id, egui::Color32::PLACEHOLDER);
            let pad = ui.spacing().button_padding;
            let dot_d = 8.0_f32;          // dot diameter slot
            let gap = 5.0_f32;            // text → dot gap
            let content_w = galley.size().x + gap + dot_d;
            // Height matches a normal button (text + vertical padding) so
            // the Auto pill is the same height as the File button; the
            // surrounding horizontal_centered layout vertically centers it
            // in the tab row. (Using the full row height here made the
            // selected-background pill taller than File.)
            let size = egui::vec2(
                content_w + pad.x * 2.0,
                galley.size().y + pad.y * 2.0,
            );
            let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
            let vis = ui.style().interact_selectable(&resp, *auto_switch);
            // Selectable background (matches egui's SelectableLabel).
            if *auto_switch || resp.hovered() {
                ui.painter().rect(
                    rect, vis.corner_radius, vis.weak_bg_fill,
                    egui::Stroke::NONE, egui::StrokeKind::Inside);
            }
            let text_color = vis.text_color();
            let text_pos = egui::pos2(rect.left() + pad.x, rect.center().y - galley.size().y / 2.0);
            ui.painter().galley(text_pos, galley.clone(), text_color);
            let dot_cx = rect.left() + pad.x + galley.size().x + gap + dot_d / 2.0;
            ui.painter().circle(
                egui::pos2(dot_cx, rect.center().y),
                4.0, text_color, egui::Stroke::new(1.2, text_color));
            if resp.on_hover_text(auto_hover).clicked() {
                *auto_switch = !*auto_switch;
            }
        }

        // Divider between the pinned cluster and the scrolling tabs.
        ui.add_space(8.0);
        let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(1.0, h), egui::Sense::hover());
        let inset = 7.0_f32;
        let x = sep_rect.center().x;
        ui.painter().line_segment(
            [egui::pos2(x, sep_rect.top() + inset),
             egui::pos2(x, sep_rect.bottom() - inset)],
            egui::Stroke::new(1.0, sep_color),
        );
        ui.add_space(2.0);

        // Left edge of the scrolling tab region — the Side Shadow fade is
        // anchored here so tabs dissolve as they slide under the pinned
        // cluster.
        tabs_left = ui.cursor().left();

        // Fill the reserved slot: solid #1B1B1B backdrop covering the
        // pinned cluster (panel left → tabs_left). Because the slot was
        // reserved before File/Auto, this paints UNDER them. Stop 1 px
        // short of the bottom so the tab-bar / content border line stays
        // visible (otherwise the dark block overlaps it).
        ui.painter().set(
            shadow_idx,
            egui::Shape::rect_filled(
                egui::Rect::from_min_max(
                    panel_outer.left_top(),
                    egui::pos2(tabs_left, panel_outer.bottom() - 1.0),
                ),
                egui::CornerRadius::ZERO,
                shadow_solid,
            ),
        );

        // ── 2. Scrolling tab strip (tabs + "+") ────────────────────────
        let scroll_out = egui::ScrollArea::horizontal()
            .id_salt("tab_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                // Initial left padding so the first tab sits clear of the
                // Side Shadow fade when unscrolled. This padding scrolls
                // away with the content, letting tabs slide under the
                // fade once the user scrolls.
                ui.add_space(50.0);

                for (i, tab) in tabs.iter().enumerate() {
                    let is_active = i == active_tab;
                    let is_bypassed = effective_bypass.get(i).copied().unwrap_or(false);

                    let galley = ui.painter().layout_no_wrap(tab.title.clone(), font_id.clone(), text_color);
                    let label_w = galley.size().x;
                    // layout: left(8) + label + buffer(4) + bypass(14) + gap(6) + close(14) + right(8)
                    let tab_w = (label_w + 54.0).max(90.0);

                    let (tab_rect, tab_resp) = ui.allocate_exact_size(
                        egui::vec2(tab_w, h),
                        egui::Sense::click(),
                    );

                    // Background. Active tab is darker than the tab-bar
                    // panel and has rounded top corners so it visually
                    // sits forward from the bar like a file-folder tab.
                    if is_active {
                        let radius = egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
                        ui.painter().rect_filled(tab_rect, radius, active_fill);
                    } else if tab_resp.hovered() {
                        let radius = egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
                        ui.painter().rect_filled(tab_rect, radius, hover_fill);
                    }
                    let _ = sep_color; // kept for the close-X hover bg
                    let _ = panel_fill;

                    // Label (left-padded, vertically centered)
                    let label_x = tab_rect.left() + 8.0;
                    let label_y = tab_rect.center().y - galley.size().y / 2.0;
                    ui.painter().galley(egui::pos2(label_x, label_y), galley, text_color);

                    // Close X button
                    let x_size = 14.0_f32;
                    let x_center = egui::pos2(tab_rect.right() - 8.0 - x_size / 2.0, tab_rect.center().y);
                    let x_rect = egui::Rect::from_center_size(x_center, egui::vec2(x_size, x_size));
                    let x_resp = ui.interact(x_rect, ui.id().with(("tab_x", i)), egui::Sense::click());
                    if x_resp.hovered() {
                        ui.painter().circle_filled(x_rect.center(), x_size / 2.0 + 1.0, sep_color);
                    }
                    let c = x_rect.center();
                    let d = 3.2_f32;
                    let xs = egui::Stroke::new(1.2, text_color);
                    ui.painter().line_segment([egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)], xs);
                    ui.painter().line_segment([egui::pos2(c.x + d, c.y - d), egui::pos2(c.x - d, c.y + d)], xs);

                    // Bypass toggle button (circle, left of X)
                    let bp_cx = x_center.x - x_size / 2.0 - 6.0 - 7.0; // right - 35
                    let bp_center = egui::pos2(bp_cx, tab_rect.center().y);
                    let bp_hit = egui::Rect::from_center_size(bp_center, egui::vec2(14.0, 14.0));
                    let bp_resp = ui.interact(bp_hit, ui.id().with(("tab_bp", i)), egui::Sense::click());
                    // Active tab: green (on) or amber (bypassed).
                    // Inactive tabs: amber if bypassed, invisible otherwise — showing green
                    // would wrongly imply background tabs are actively routing.
                    let dot_color = if is_bypassed {
                        egui::Color32::from_rgb(220, 140, 40) // amber = bypassed
                    } else if is_active {
                        egui::Color32::from_rgb(60, 180, 60)  // green = active (only on active tab)
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let (bp_fill, bp_stroke_color) = (dot_color, dot_color);
                    ui.painter().circle(bp_center, 4.0, bp_fill, egui::Stroke::new(1.2, bp_stroke_color));
                    if bp_resp.clicked() {
                        bypass_toggle = Some(i);
                    }

                    if x_resp.clicked() {
                        close_idx = Some(i);
                    } else if tab_resp.clicked() {
                        switch_to = Some(i);
                    }

                    // Vertical separator between non-active adjacent tabs
                    if i + 1 < tabs.len() && !is_active && (i + 1) != active_tab {
                        let sx = tab_rect.right();
                        ui.painter().line_segment(
                            [egui::pos2(sx, tab_rect.top() + 5.0), egui::pos2(sx, tab_rect.bottom() - 5.0)],
                            egui::Stroke::new(1.0, sep_color),
                        );
                    }
                }

                // "+" new tab button
                let (plus_rect, plus_resp) = ui.allocate_exact_size(egui::vec2(32.0, h), egui::Sense::click());
                if plus_resp.hovered() {
                    ui.painter().rect_filled(plus_rect, egui::CornerRadius::ZERO, hover_fill);
                }
                let c = plus_rect.center();
                let ps = egui::Stroke::new(1.5, text_color);
                ui.painter().line_segment([egui::pos2(c.x - 5.0, c.y), egui::pos2(c.x + 5.0, c.y)], ps);
                ui.painter().line_segment([egui::pos2(c.x, c.y - 5.0), egui::pos2(c.x, c.y + 5.0)], ps);
                if plus_resp.clicked() {
                    new_tab = true;
                }
                });
            });

        // ── Side Shadow fades ──────────────────────────────────────────
        // Left: #1B1B1B → transparent fade starting at the cluster
        // boundary (the soft edge of the solid backdrop), so tabs
        // dissolve as they scroll under File/Auto. Right: same fade,
        // only when more tabs overflow off-screen.
        paint_tab_scroll_shadows(ui.ctx(), &scroll_out, panel_outer, tabs_left, shadow_solid);
    });

    TabBarActions {
        switch_to, close_idx, new_tab, bypass_toggle,
        do_save, do_load, do_save_workspace, do_load_workspace,
        do_bind, do_close,
    }
}

/// Paint the tab-strip Side Shadow fades, matching the mockup's
/// `linear-gradient(90deg, #1B1B1B 70%, transparent 100%)`.
///
/// LEFT: a `#1B1B1B → transparent` fade beginning at `tabs_left` — the
/// soft right edge of the solid backdrop behind File/Auto — so tabs
/// dissolve into it as they scroll under the pinned cluster. Always
/// drawn. RIGHT: the mirror fade, only when more tabs overflow.
pub(crate) fn paint_tab_scroll_shadows<R>(
    ctx: &egui::Context,
    out: &egui::scroll_area::ScrollAreaOutput<R>,
    panel_outer: egui::Rect,
    tabs_left: f32,
    shadow_solid: egui::Color32,
) {
    const FADE_W: f32 = 42.0; // ~30% of the mockup's 141px Side Shadow
    let inner = out.inner_rect;
    let offset_x = out.state.offset.x;
    let max_scroll = (out.content_size.x - inner.width()).max(0.0);
    let can_scroll_right = offset_x < max_scroll - 0.5;

    // Render the fade on Background so floating windows (which default to
    // Order::Middle) sit on top of it instead of being overlaid by it.
    let layer = egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new(("tab_edge_fade", out.id)),
    );
    let mut painter = ctx.layer_painter(layer);
    painter.set_clip_rect(panel_outer);

    let clear = egui::Color32::TRANSPARENT;
    let top = panel_outer.top();
    // Stop 1 px above the bottom so the tab-bar / content border line
    // stays visible under the fade.
    let bot = panel_outer.bottom() - 1.0;
    // Opaque lead-in that overlaps the base-layer backdrop (which ends at
    // tabs_left), so a tab scrolled into this zone is fully hidden and
    // there's no seam where the backdrop and fade meet.
    const OVERLAP: f32 = 10.0;

    // Opaque #1B1B1B lead-in covering [tabs_left - OVERLAP, tabs_left].
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(tabs_left - OVERLAP, top),
            egui::pos2(tabs_left, bot)),
        egui::CornerRadius::ZERO,
        shadow_solid,
    );
    // Fade #1B1B1B → transparent over [tabs_left, tabs_left + FADE_W].
    paint_tab_gradient_quad(&painter,
        egui::pos2(tabs_left, top), egui::pos2(tabs_left + FADE_W, bot),
        shadow_solid, clear, clear, shadow_solid);

    // Right fade — transparent → solid #1B1B1B at the right edge, only
    // when tabs overflow off-screen there.
    if can_scroll_right {
        let x0 = panel_outer.right() - FADE_W;
        let x1 = panel_outer.right();
        paint_tab_gradient_quad(&painter,
            egui::pos2(x0, top), egui::pos2(x1, bot),
            clear, shadow_solid, shadow_solid, clear);
    }
}

/// Two-triangle horizontal gradient quad. Corner colors map tl, tr, br,
/// bl. (Local copy mirroring the physical-devices panel helper to keep
/// the tab-bar self-contained.)
pub(crate) fn paint_tab_gradient_quad(
    painter: &egui::Painter,
    tl: egui::Pos2,
    br: egui::Pos2,
    c_tl: egui::Color32, c_tr: egui::Color32, c_br: egui::Color32, c_bl: egui::Color32,
) {
    use egui::epaint::{Mesh, Vertex};
    let mut mesh = Mesh::default();
    let uv = egui::epaint::WHITE_UV;
    let tr = egui::pos2(br.x, tl.y);
    let bl = egui::pos2(tl.x, br.y);
    let i = mesh.vertices.len() as u32;
    mesh.vertices.push(Vertex { pos: tl, uv, color: c_tl });
    mesh.vertices.push(Vertex { pos: tr, uv, color: c_tr });
    mesh.vertices.push(Vertex { pos: br, uv, color: c_br });
    mesh.vertices.push(Vertex { pos: bl, uv, color: c_bl });
    mesh.indices.extend_from_slice(&[i, i+1, i+2, i, i+2, i+3]);
    painter.add(mesh);
}

// (Removed: rounded-corner HRGN logic. SetWindowRgn interacted badly
// with WS_EX_LAYERED + pseudo-maximize, producing NC chrome strobing.
// Window stays rectangular; the painted 1 px border delineates the
// edge.)

// ── Custom title bar ──────────────────────────────────────────────────────────

pub(crate) fn handle_window_resize(ctx: &egui::Context) {
    // Skip edge-resize hit-testing when OS-maximized.
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    if maximized { return; }

    let screen = ctx.viewport_rect();
    let (pointer_pos, primary_pressed) = ctx.input(|i| (i.pointer.hover_pos(), i.pointer.primary_pressed()));
    let Some(pos) = pointer_pos else { return };

    const BORDER: f32 = 6.0;
    let on_l = pos.x < screen.left()   + BORDER;
    let on_r = pos.x > screen.right()  - BORDER;
    let on_t = pos.y < screen.top()    + BORDER;
    let on_b = pos.y > screen.bottom() - BORDER;

    let dir = match (on_l, on_r, on_t, on_b) {
        (true,  false, true,  false) => Some(egui::ResizeDirection::NorthWest),
        (false, true,  true,  false) => Some(egui::ResizeDirection::NorthEast),
        (true,  false, false, true ) => Some(egui::ResizeDirection::SouthWest),
        (false, true,  false, true ) => Some(egui::ResizeDirection::SouthEast),
        (true,  false, false, false) => Some(egui::ResizeDirection::West),
        (false, true,  false, false) => Some(egui::ResizeDirection::East),
        (false, false, true,  false) => Some(egui::ResizeDirection::North),
        (false, false, false, true ) => Some(egui::ResizeDirection::South),
        _ => None,
    };

    if let Some(dir) = dir {
        let cursor = match dir {
            egui::ResizeDirection::North     => egui::CursorIcon::ResizeNorth,
            egui::ResizeDirection::South     => egui::CursorIcon::ResizeSouth,
            egui::ResizeDirection::East      => egui::CursorIcon::ResizeEast,
            egui::ResizeDirection::West      => egui::CursorIcon::ResizeWest,
            egui::ResizeDirection::NorthEast => egui::CursorIcon::ResizeNorthEast,
            egui::ResizeDirection::NorthWest => egui::CursorIcon::ResizeNorthWest,
            egui::ResizeDirection::SouthEast => egui::CursorIcon::ResizeSouthEast,
            egui::ResizeDirection::SouthWest => egui::CursorIcon::ResizeSouthWest,
        };
        ctx.set_cursor_icon(cursor);
        if primary_pressed {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }
}

pub(crate) fn draw_rect_stroke(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let tl = rect.left_top();
    let tr = rect.right_top();
    let br = rect.right_bottom();
    let bl = rect.left_bottom();
    painter.line_segment([tl, tr], stroke);
    painter.line_segment([tr, br], stroke);
    painter.line_segment([br, bl], stroke);
    painter.line_segment([bl, tl], stroke);
}

pub(crate) fn show_title_bar(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    do_save: &mut bool,
    do_load: &mut bool,
    do_save_workspace: &mut bool,
    do_load_workspace: &mut bool,
    do_new: &mut bool,
    do_close: &mut bool,
    do_bind: &mut bool,
    do_hidhide: &mut bool,
    auto_switch: &mut bool,
    do_undo: &mut bool,
    do_redo: &mut bool,
    can_undo: bool,
    can_redo: bool,
    logo: &Option<egui::TextureHandle>,
    panic_shortcut: &mut PanicShortcut,
    panic_active: &mut bool,
    panic_learning: &mut bool,
    panic_shortcut_shared: &Arc<RwLock<PanicShortcut>>,
    toggle_settings: &mut bool,
    toggle_bluetooth: &mut bool,
    bluetooth_present: bool,
    pin_active: bool,
    do_pin_toggle: &mut bool,
    ui_mode: settings::UiMode,
    do_set_mode: &mut Option<settings::UiMode>,
) {
    let bar = ui.max_rect();
    let h = bar.height();
    let btn_w = 46.0_f32;
    let ctrl_w = btn_w * 3.0;
    // Wide enough for the Wide mode pill (~204 px) + dividers +
    // undo/redo + pin without forcing the Short variant; capped so the
    // cluster never crowds the centered FlexInput title on narrow
    // windows (the pill auto-falls to its Short variant when squeezed).
    let left_w = 380.0_f32.min(bar.width() * 0.42);

    // Full-bar drag sensing (placed first so interactive widgets above take priority).
    let drag = ui.interact(bar, ui.id().with("tb_drag"), egui::Sense::click_and_drag());

    // The File menu / Auto-switch toggle moved down into the tab bar
    // (see `show_tab_bar`); these params are still threaded through for
    // potential future use but are no longer surfaced here.
    let _ = (do_save, do_load, do_save_workspace, do_load_workspace,
             do_new, do_close, do_bind, do_hidhide, auto_switch);

    // ── Left cluster: Mode pill → undo/redo → pin ──────────────────────────
    // The Easy/Advanced mode pill now anchors at the FAR LEFT of the
    // title bar (where File/Auto used to live), followed by the
    // undo/redo buttons and the pin toggle. Matches the new mockup.
    let left_rect = egui::Rect::from_min_size(bar.min, egui::vec2(left_w, h));
    ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add_space(8.0);

            // ── Mode pill (Easy / Advanced) ────────────────────────────
            // Adaptive: pick the widest variant that fits the slack in
            // the left cluster. The pill is SVG-rendered with manual
            // rect math, so allocate a slot in the left-to-right flow
            // and hand that rect to `render_mode_pill`.
            let pill_h = (h - 4.0).max(20.0);
            let wide_w  = pill_size_px(MODE_WHOLE_PILL_SVG, pill_h).0;
            let short_w = pill_size_px(MODE_SHORT_PILL_SVG, pill_h).0;
            // Reserve room for undo/redo + pin + dividers after the pill
            // (~120 px); if the window is narrow, fall back to the short
            // pill so those controls never get clipped.
            let avail = ui.available_width();
            let (pill_w, variant) = if avail - wide_w >= 120.0 {
                (wide_w, ModePillVariant::Wide)
            } else {
                (short_w, ModePillVariant::Short)
            };
            let (pill_rect, _) = ui.allocate_exact_size(
                egui::vec2(pill_w, pill_h), egui::Sense::hover());
            render_mode_pill(ui, pill_rect, variant, ui_mode, do_set_mode);

            // ── Divider before undo/redo ───────────────────────────────
            ui.add_space(6.0);
            let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(1.0, h), egui::Sense::hover());
            let inset = 8.0_f32;
            let x = sep_rect.center().x;
            ui.painter().line_segment(
                [egui::pos2(x, sep_rect.top() + inset),
                 egui::pos2(x, sep_rect.bottom() - inset)],
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
            ui.add_space(4.0);

            // Undo / Redo buttons
            if ui.add_enabled(can_undo, egui::Button::new("↶").small())
                .on_hover_text("Undo (Ctrl+Z)")
                .clicked()
            {
                *do_undo = true;
            }
            if ui.add_enabled(can_redo, egui::Button::new("↷").small())
                .on_hover_text("Redo (Ctrl+Shift+Z)")
                .clicked()
            {
                *do_redo = true;
            }

            // ── Divider before pin ─────────────────────────────────────
            ui.add_space(6.0);
            let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(1.0, h), egui::Sense::hover());
            let inset = 8.0_f32;
            let x = sep_rect.center().x;
            ui.painter().line_segment(
                [egui::pos2(x, sep_rect.top() + inset),
                 egui::pos2(x, sep_rect.bottom() - inset)],
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
            ui.add_space(4.0);

            // ── Pin (always-on-top) ───────────────────────────────────
            // SelectableLabel so the active state gets the standard egui
            // highlight, matching the see-through eye toggle's look.
            let pin_label = egui::RichText::new("📌").size(13.0);
            let pin_resp = ui.add(egui::Button::selectable(pin_active, pin_label));
            let hover = if pin_active {
                "Pinned: window stays on top of all others.\nClick to unpin."
            } else {
                "Pin window to stay on top of all others.\nConfigure global hotkey / Guide-button trigger in Settings."
            };
            if pin_resp.on_hover_text(hover).clicked() {
                *do_pin_toggle = true;
            }

            // ── See-through eye toggle ──────────────────────────────────
            // Shares the same ctx-data slots the (legacy) zoom-overlay
            // eye used, so see-through state + opacity stay in sync no
            // matter which mode the user is in. Click toggles; hover
            // pops out a vertical opacity slider.
            ui.add_space(4.0);
            render_eye_toggle(ui, h);

            // ── Info overlay toggle (spike) ─────────────────────────────
            ui.add_space(4.0);
            crate::overlay::render_overlay_toggle(ui, h);
        });
    });

    // ── Right: window control buttons (painter-drawn icons) ───────────────
    let ctrl_rect = egui::Rect::from_min_size(
        egui::pos2(bar.right() - ctrl_w, bar.top()),
        egui::vec2(ctrl_w, h),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(ctrl_rect), |ui| {
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let icon_color = ui.visuals().text_color();
            let hover_fill = ui.visuals().widgets.hovered.bg_fill;

            // ── Close ──────────────────────────────────────────────────────
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(btn_w, h), egui::Sense::click());
            let close_color = if resp.hovered() {
                ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, egui::Color32::from_rgb(196, 43, 28));
                egui::Color32::WHITE
            } else {
                icon_color
            };
            let c = rect.center();
            let d = 5.0_f32;
            let s = egui::Stroke::new(1.5, close_color);
            ui.painter().line_segment([egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)], s);
            ui.painter().line_segment([egui::pos2(c.x + d, c.y - d), egui::pos2(c.x - d, c.y + d)], s);
            if resp.clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }

            // ── Maximize / Restore ─────────────────────────────────────────
            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(btn_w, h), egui::Sense::click());
            if resp.hovered() { ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, hover_fill); }
            let c = rect.center();
            let s = egui::Stroke::new(1.5, icon_color);
            if maximized {
                let back  = egui::Rect::from_min_size(egui::pos2(c.x - 1.5, c.y - 5.5), egui::vec2(9.0, 8.0));
                let front = egui::Rect::from_min_size(egui::pos2(c.x - 5.0, c.y - 2.0), egui::vec2(9.0, 8.0));
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(front.min, egui::vec2(3.0, 2.0)),
                    egui::CornerRadius::ZERO,
                    ui.visuals().panel_fill,
                );
                draw_rect_stroke(ui.painter(), back, s);
                draw_rect_stroke(ui.painter(), front, s);
            } else {
                draw_rect_stroke(ui.painter(), egui::Rect::from_center_size(c, egui::vec2(11.0, 9.0)), s);
            }
            if resp.clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }

            // ── Minimize ───────────────────────────────────────────────────
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(btn_w, h), egui::Sense::click());
            if resp.hovered() { ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, hover_fill); }
            let c = rect.center();
            ui.painter().line_segment(
                [egui::pos2(c.x - 5.5, c.y + 2.0), egui::pos2(c.x + 5.5, c.y + 2.0)],
                egui::Stroke::new(1.5, icon_color),
            );
            if resp.clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)); }
        });
    });

    // ── Panic-mode strip (anchored to the left of the window controls) ─────
    {
        // Reserve a generous slice immediately left of the window-controls.
        // The strip lays out right-to-left so the rightmost edge is always
        // pinned to ctrl_rect.left() regardless of shortcut-label length.
        const PANIC_STRIP_W: f32 = 260.0;
        let panic_rect = egui::Rect::from_min_size(
            egui::pos2(ctrl_rect.left() - PANIC_STRIP_W, bar.top()),
            egui::vec2(PANIC_STRIP_W, h),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(panic_rect), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);

                // Shortcut button. While learning, label is "Press chord…".
                let btn_text = if *panic_learning {
                    "Press chord…".to_string()
                } else {
                    panic_shortcut.label()
                };
                let mut btn = egui::Button::new(egui::RichText::new(btn_text).size(12.0));
                if *panic_active {
                    btn = btn.fill(egui::Color32::from_rgb(196, 43, 28))
                             .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(220, 80, 60)));
                } else if *panic_learning {
                    btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                             .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 160, 80)));
                }
                let resp = ui.add(btn).on_hover_text(
                    if *panic_active {
                        "Panic mode ENGAGED — virtual output is suppressed.\nPress the shortcut again to release."
                    } else if *panic_learning {
                        "Press the new shortcut (modifier + key).\nClick again to cancel."
                    } else {
                        "Click to re-bind the shortcut.\nPress the shortcut anywhere on the system to toggle Panic mode."
                    }
                );

                // Click toggles Learn mode (start re-binding, or cancel). To
                // engage / disengage Panic mode, the user presses the shortcut
                // — the title-bar button itself is purely a re-bind control.
                if resp.clicked() {
                    *panic_learning = !*panic_learning;
                }

                // While in learn mode, watch every input event for the next chord.
                if *panic_learning {
                    let pressed: Option<egui::Key> = ctx.input(|i| {
                        i.events.iter().find_map(|e| match e {
                            egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                            _ => None,
                        })
                    });
                    if let Some(key) = pressed {
                        // Skip pure-modifier-only events (egui doesn't emit Shift/Ctrl/Alt/Win
                        // as Key here; modifiers come through i.modifiers on the next key).
                        let m = ctx.input(|i| i.modifiers);
                        let key_name = format!("{:?}", key);
                        *panic_shortcut = PanicShortcut {
                            ctrl:  m.ctrl,
                            shift: m.shift,
                            alt:   m.alt,
                            win:   m.command && !m.ctrl,
                            key:   Some(key_name),
                        };
                        save_panic_shortcut(panic_shortcut);
                        if let Ok(mut s) = panic_shortcut_shared.write() {
                            *s = panic_shortcut.clone();
                        }
                        *panic_learning = false;
                    }
                }

                ui.add_space(6.0);
                ui.label(egui::RichText::new("Panic mode:").size(12.0).weak());
            });
        });
    }

    // ── Center: FlexInput title (matches Figma `title` group) ─────────────
    // Layout (Figma group 122×38, scaled to the title-bar height):
    //   • Dark rounded "TitleBG" pill (#1B1B1B) behind logo + text;
    //     gains a white outline on hover.
    //   • Square logo tile (rasterized from icon_v2.svg — the dark
    //     rounded square with the Fi glyph) overhanging the pill's left
    //     edge, sized to the full bar height so it pokes slightly above
    //     and below the pill.
    //   • "FlexInput" text (16 px in Figma) just right of the tile.
    // The whole group is clickable → opens Settings.
    let mid = bar.center();
    let base_color = egui::Color32::WHITE; // Figma title text is pure white

    // Figma proportions: logo 38, pill 32 tall, text 16 px.
    // Scale to the bar: tile = bar height, pill a touch shorter — then a
    // uniform 0.9 nudge so the whole group reads a touch smaller than the
    // exact mockup measurements (matches the intended visual weight).
    const TITLE_SCALE: f32 = 0.9;
    let tile = ((h - 2.0) * TITLE_SCALE).max(24.0 * TITLE_SCALE);  // logo tile side (overhangs pill)
    let pill_h = ((h - 6.0) * TITLE_SCALE).max(20.0 * TITLE_SCALE); // dark pill height
    let text_px = 16.0_f32 * TITLE_SCALE;
    let logo_text_gap = 8.0_f32 * TITLE_SCALE;     // gap between tile and text
    let text_pad_right = 12.0_f32 * TITLE_SCALE;   // pill padding after the text

    let font_id = egui::FontId::proportional(text_px);
    // Trailing gear marks the title as the Settings button (it opens Settings on
    // click). The glyph ships in egui's bundled emoji font.
    let galley = ui.painter().layout_no_wrap("FlexInput ⚙".to_string(), font_id, base_color);
    let text_size = galley.size();

    // The pill spans from a few px inside the tile's left to the right of
    // the text; the tile overhangs the pill's left like in the mock.
    let tile_overhang = 4.0_f32 * TITLE_SCALE; // how far the tile pokes past the pill left
    let group_w = tile + logo_text_gap + text_size.x + text_pad_right;
    let group_left = mid.x - group_w / 2.0;

    let tile_rect = egui::Rect::from_min_size(
        egui::pos2(group_left, mid.y - tile / 2.0),
        egui::vec2(tile, tile),
    );
    let pill_left = tile_rect.left() + tile_overhang;
    let pill_rect = egui::Rect::from_min_max(
        egui::pos2(pill_left, mid.y - pill_h / 2.0),
        egui::pos2(group_left + group_w, mid.y + pill_h / 2.0),
    );
    let text_left = tile_rect.right() + logo_text_gap;

    // Hit rect covers the whole group (tile + pill).
    let hit_rect = tile_rect.union(pill_rect);
    let logo_resp = ui.interact(hit_rect, ui.id().with("logo_settings"), egui::Sense::click());
    if logo_resp.clicked() {
        *toggle_settings = true;
    }
    if logo_resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Dark pill background (#1B1B1B), brightening slightly on hover with
    // an outline around the whole pill as the hover affordance.
    let pill_fill = if logo_resp.hovered() {
        egui::Color32::from_rgb(38, 38, 38)
    } else {
        egui::Color32::from_rgb(27, 27, 27) // #1B1B1B
    };
    let pill_stroke = if logo_resp.hovered() {
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(110))
    } else {
        egui::Stroke::NONE
    };
    ui.painter().rect(
        pill_rect,
        egui::CornerRadius::same(6),
        pill_fill,
        pill_stroke,
        egui::StrokeKind::Inside,
    );

    // Logo tile + text.
    let painter = ui.painter();
    if let Some(tex) = logo {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        // Snap to physical pixels so the bitmap downscale samples on-pixel
        // (otherwise LINEAR sampling smears the icon's alpha edges).
        let ppp = ctx.pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let logo_rect = egui::Rect::from_min_max(
            egui::pos2(snap(tile_rect.left()), snap(tile_rect.top())),
            egui::pos2(snap(tile_rect.right()), snap(tile_rect.bottom())),
        );
        painter.image(tex.id(), logo_rect, uv, egui::Color32::WHITE);
    }
    painter.galley(egui::pos2(text_left, mid.y - text_size.y / 2.0), galley, base_color);

    // ── Bluetooth dongle button ──────────────────────────────────────────
    //
    // ⭐ Shown only when a Bluetooth adapter is actually visible to our USB
    // stack. It is a control for hardware that most machines do not have and
    // most users will never plug in, so a permanent button would be a
    // permanent question ("what is this for?") for everyone it does not serve.
    // Present hardware, present button.
    if bluetooth_present {
        let side = pill_rect.height() * 0.82;
        let bt_rect = egui::Rect::from_center_size(
            egui::pos2(pill_rect.right() + 6.0 + side / 2.0, mid.y),
            egui::vec2(side, side),
        );
        let bt = ui.interact(bt_rect, ui.id().with("bt_dongle"), egui::Sense::click());
        if bt.clicked() {
            *toggle_bluetooth = true;
        }
        if bt.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let p = ui.painter();
        p.rect(
            bt_rect,
            egui::CornerRadius::same(6),
            if bt.hovered() {
                egui::Color32::from_rgb(38, 38, 38)
            } else {
                egui::Color32::from_rgb(27, 27, 27)
            },
            if bt.hovered() {
                egui::Stroke::new(1.0, egui::Color32::from_white_alpha(110))
            } else {
                egui::Stroke::NONE
            },
            egui::StrokeKind::Inside,
        );
        // The Bluetooth rune, drawn rather than glyphed: the character is not
        // in most bundled fonts and renders as a replacement box.
        let c = bt_rect.center();
        let hh = side * 0.30; // half-height of the rune
        let w = side * 0.17;
        let col = if bt.hovered() {
            egui::Color32::from_gray(235)
        } else {
            egui::Color32::from_gray(170)
        };
        let stroke = egui::Stroke::new((side * 0.075).max(1.0), col);
        let (top, bot) = (egui::pos2(c.x, c.y - hh), egui::pos2(c.x, c.y + hh));
        let (ur, lr) = (egui::pos2(c.x + w, c.y - hh / 2.0), egui::pos2(c.x + w, c.y + hh / 2.0));
        let (ul, ll) = (egui::pos2(c.x - w, c.y - hh / 2.0), egui::pos2(c.x - w, c.y + hh / 2.0));
        for seg in [
            [top, bot],   // the spine
            [top, ur],    // upper right diagonal
            [ur, ll],     // down through the centre to lower left
            [bot, lr],    // lower right diagonal
            [lr, ul],     // up through the centre to upper left
        ] {
            p.line_segment(seg, stroke);
        }
        bt.on_hover_text("Bluetooth dongle — paired controllers, keys and transport");
    }

    // Fire StartDrag on mouse-press (not drag_started) to avoid the
    // egui ~6 px threshold lag before the OS drag-move loop takes
    // over. Win32 itself decides click vs drag based on actual cursor
    // travel, so this is safe — single clicks still register, and
    // double-clicks still fire on the second press.
    if drag.is_pointer_button_down_on()
        && ctx.input(|i| i.pointer.primary_pressed())
    {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
}

// ── Mode pill (Easy / Advanced) ──────────────────────────────────────────────
//
// SVG-driven segmented control. Two presentation variants:
//
//   `Wide`  — `mode_whole_pill.svg` (outer pill with "Mode:" label
//             baked in) + `easy_mode.svg` or `advanced_mode.svg`
//             overlaid right-anchored on top of the pill. Both chip
//             SVGs are a single graphic containing BOTH halves with
//             the slanted divider already drawn; the active half is
//             tinted, the inactive half is the muted variant. We
//             just split the rendered image rectangle into two click
//             zones (Easy on left, Advanced on right).
//
//   `Short` — `mode_short_pill.svg` (outer pill, no "Mode:" label) +
//             same chip SVG overlay. Used when there isn't enough
//             room between the File-menu cluster and the centered
//             FlexInput title to fit the wide variant.

/// App logo (the dark rounded "Fi" tile), pre-baked to a 256px PNG from
/// `icon_v2.svg`. Used for both the title-bar logo and the OS
/// window/taskbar icon. Baked rather than rasterized at runtime because
/// the SVG's blur/color-matrix filters take ~45s for resvg to render at
/// 256px — that delay was stalling app startup. Re-bake whenever the
/// source SVG changes (render_app_icon → save_png at 256).
pub(crate) const APP_ICON_PNG: &[u8] = include_bytes!(
    "../../../../app/assets/icon_v2_256.png");

pub(crate) const MODE_WHOLE_PILL_SVG: &[u8] = include_bytes!(
    "../../../../app/assets/mode_whole_pill.svg");
pub(crate) const MODE_SHORT_PILL_SVG: &[u8] = include_bytes!(
    "../../../../app/assets/mode_short_pill.svg");
pub(crate) const MODE_EASY_SVG: &[u8] = include_bytes!(
    "../../../../app/assets/easy_mode.svg");
pub(crate) const MODE_ADV_SVG:  &[u8] = include_bytes!(
    "../../../../app/assets/advanced_mode.svg");

/// Decode the pre-baked app-logo PNG ([`APP_ICON_PNG`], 256px) into an
/// [`egui::IconData`] for the OS window / taskbar icon. Decoding a PNG is
/// instant, unlike rasterizing the filter-heavy source SVG. Returns
/// `None` if the bundled PNG fails to decode.
pub fn render_app_icon() -> Option<egui::IconData> {
    let icon = eframe::icon_data::from_png_bytes(APP_ICON_PNG).ok()?;
    Some(egui::IconData { rgba: icon.rgba, width: icon.width, height: icon.height })
}


#[derive(Clone, Copy, Debug)]
pub(crate) enum ModePillVariant { Wide, Short }

/// See-through eye toggle for the title bar. Click flips see-through on/off;
/// hover pops out a vertical opacity slider below the button. Reads & writes
/// the same ctx-data slots (`SEE_THROUGH_DATA_KEY` / `SEE_THROUGH_ALPHA_KEY`)
/// that `FlexInputApp::update` mirrors into `settings`, so it works from
/// either Easy or Advanced mode without threading state through.
pub(crate) fn render_eye_toggle(ui: &mut egui::Ui, bar_h: f32) {
    let see_through_id = egui::Id::new(crate::canvas::SEE_THROUGH_DATA_KEY);
    let see_through_on: bool = ui.ctx().data(|d| d.get_temp::<bool>(see_through_id))
        .unwrap_or(false);

    let eye_label = egui::RichText::new("👁").size(14.0);
    let eye_btn = egui::Button::selectable(see_through_on, eye_label);
    let eye_resp = ui.add_sized(egui::vec2(26.0, (bar_h - 6.0).max(18.0)), eye_btn);
    let hover = if see_through_on {
        "See-through: ON — click to make app fully opaque.\nHover to adjust opacity."
    } else {
        "See-through: OFF — click to make app translucent.\nHover to adjust opacity."
    };
    let eye_resp = eye_resp.on_hover_text(hover);
    if eye_resp.clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(see_through_id, !see_through_on));
    }

    // Opacity popover BELOW the button (title bar is at the top of the
    // window, so the slider drops down). Same grace-timer pattern as the
    // legacy zoom-overlay version so the cursor can travel from the eye
    // to the slider without the popup closing mid-traversal.
    let popup_id = ui.id().with("titlebar_see_through_popup");
    let last_hover_id = popup_id.with("last_hover");
    const POPUP_GRACE: std::time::Duration = std::time::Duration::from_millis(2500);
    let now = std::time::Instant::now();
    let last_hover: Option<std::time::Instant> =
        ui.ctx().data(|d| d.get_temp::<std::time::Instant>(last_hover_id));
    if eye_resp.hovered() {
        ui.ctx().data_mut(|d| d.insert_temp(last_hover_id, now));
    }
    let popup_visible = eye_resp.hovered()
        || last_hover.map(|t| now.duration_since(t) < POPUP_GRACE).unwrap_or(false);
    if popup_visible {
        let alpha_id = egui::Id::new(crate::canvas::SEE_THROUGH_ALPHA_KEY);
        let mut alpha: f32 = ui.ctx().data(|d| d.get_temp::<f32>(alpha_id))
            .unwrap_or(0.55);
        let popup_area = egui::Area::new(popup_id)
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(
                eye_resp.rect.center().x - 28.0,
                eye_resp.rect.bottom() + 4.0,
            ))
            .interactable(true);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        let popup_resp = popup_area.show(ui.ctx(), |ui| {
            let bg = ui.visuals().window_fill();
            egui::Frame::default()
                .fill(egui::Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 240))
                .stroke(egui::Stroke::new(1.0,
                    ui.visuals().widgets.noninteractive.bg_stroke.color))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new(format!("{:.0}%", alpha * 100.0)).small());
                        let resp = ui.add_sized(
                            egui::vec2(40.0, 96.0),
                            egui::Slider::new(&mut alpha, 0.0_f32..=1.0)
                                .vertical().show_value(false),
                        );
                        if resp.changed() {
                            ui.ctx().data_mut(|d|
                                d.insert_temp(alpha_id, alpha.clamp(0.0, 1.0)));
                        }
                    });
                }).response
        }).response;
        if eye_resp.hovered() || popup_resp.hovered() {
            ui.ctx().data_mut(|d| d.insert_temp(last_hover_id, now));
        }
    }
}

/// Compute the on-screen width that a pill SVG occupies when rendered
/// at `render_h` logical pixels, preserving the SVG's intrinsic aspect.
pub(crate) fn pill_size_px(svg_bytes: &[u8], render_h: f32) -> (f32, f32) {
    // Cheap aspect probe: parse `viewBox` or width/height from the
    // SVG header without going through usvg. Falls back to 1:1.
    let text = std::str::from_utf8(svg_bytes).unwrap_or("");
    let aspect = parse_svg_aspect(text).unwrap_or(1.0);
    (render_h * aspect, render_h)
}

pub(crate) fn parse_svg_aspect(text: &str) -> Option<f32> {
    // Look for the `viewBox="0 0 W H"` attribute first; fall back to
    // separate width / height attributes if the viewBox is missing.
    if let Some(vb_start) = text.find("viewBox=\"") {
        let s = &text[vb_start + 9..];
        if let Some(end) = s.find('"') {
            let parts: Vec<&str> = s[..end].split_whitespace().collect();
            if parts.len() == 4 {
                let w: f32 = parts[2].parse().ok()?;
                let h: f32 = parts[3].parse().ok()?;
                if h > 0.0 { return Some(w / h); }
            }
        }
    }
    let w = parse_svg_attr(text, "width")?;
    let h = parse_svg_attr(text, "height")?;
    if h > 0.0 { Some(w / h) } else { None }
}

pub(crate) fn parse_svg_attr(text: &str, name: &str) -> Option<f32> {
    let key = format!("{}=\"", name);
    let i = text.find(&key)?;
    let s = &text[i + key.len()..];
    let end = s.find('"')?;
    s[..end].parse().ok()
}

/// Rasterize an SVG to a cached non-square texture at exactly
/// (w_px, h_px) DEVICE pixels. Reuses the recolored rasterizer; since
/// w_px : h_px already matches the SVG aspect, no letterboxing occurs.
pub(crate) fn mode_pill_texture(
    ui: &egui::Ui,
    bytes: &'static [u8],
    w_px: u32,
    h_px: u32,
) -> Option<egui::TextureHandle> {
    let cache_key = egui::Id::new(("mode_pill_tex", bytes.as_ptr() as usize, w_px, h_px));
    if let Some(h) = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(cache_key)) {
        return Some(h);
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let img = crate::canvas::viewer::rasterize_svg_recolored(
        text, w_px, h_px, "override", egui::Color32::TRANSPARENT)?;
    let handle = ui.ctx().load_texture(
        format!("mode_pill_{:p}_{}x{}", bytes.as_ptr(), w_px, h_px),
        img,
        egui::TextureOptions::LINEAR,
    );
    ui.ctx().data_mut(|d| d.insert_temp(cache_key, handle.clone()));
    Some(handle)
}

pub(crate) fn paint_pill_svg(ui: &mut egui::Ui, bytes: &'static [u8], rect: egui::Rect) {
    let ppp = ui.ctx().pixels_per_point();
    // Snap the destination rect to physical pixel boundaries so the
    // texture lands at integer texel positions — otherwise LINEAR
    // sampling blends adjacent pixels and softens every edge,
    // making the rasterized SVG text look blurry.
    let snap = |v: f32| (v * ppp).round() / ppp;
    let rect = egui::Rect::from_min_max(
        egui::pos2(snap(rect.left()),  snap(rect.top())),
        egui::pos2(snap(rect.right()), snap(rect.bottom())),
    );
    let w_px = ((rect.width())  * ppp).round() as u32;
    let h_px = ((rect.height()) * ppp).round() as u32;
    if let Some(tex) = mode_pill_texture(ui, bytes, w_px, h_px) {
        let uv = egui::Rect::from_min_max(
            egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(tex.id(), rect, uv, egui::Color32::WHITE);
    }
}

pub(crate) fn render_mode_pill(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    variant: ModePillVariant,
    mode: settings::UiMode,
    do_set_mode: &mut Option<settings::UiMode>,
) {
    // 1. Outer pill background (with or without "Mode:" label).
    let bg_svg = match variant {
        ModePillVariant::Wide  => MODE_WHOLE_PILL_SVG,
        ModePillVariant::Short => MODE_SHORT_PILL_SVG,
    };
    paint_pill_svg(ui, bg_svg, rect);

    // 2. Chip overlay (Easy/Adv combined) right-anchored on the pill.
    // The chip SVG already contains both halves AND the slanted
    // divider; only the colors differ between easy_mode.svg and
    // advanced_mode.svg.
    //
    // Height note: the pill SVGs are 30 px tall (28 inner + 1 px
    // stroke on each side); the chip SVGs are 28 px tall with no
    // stroke. To keep the chip INSIDE the pill's outline, render it
    // at a shrunk height matching the pill's inner content area.
    let chip_svg = match mode {
        settings::UiMode::Easy     => MODE_EASY_SVG,
        settings::UiMode::Advanced => MODE_ADV_SVG,
    };
    let pill_inset = 1.0_f32; // matches the 1 px stroke on the pill SVGs
    let chip_h = (rect.height() - 2.0 * pill_inset).max(1.0);
    let (chip_w, _) = pill_size_px(chip_svg, chip_h);
    let chip_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - chip_w - pill_inset,
                   rect.top() + pill_inset),
        egui::vec2(chip_w, chip_h),
    );
    paint_pill_svg(ui, chip_svg, chip_rect);

    // 3. Click zones — split the chip rect down the middle. The SVGs
    // were authored so the two halves are roughly equal-width either
    // side of the slash, so a 50/50 split on the rect is close enough
    // for hit-testing without parsing the slash geometry.
    let mid_x = chip_rect.center().x;
    let easy_zone = egui::Rect::from_min_max(
        chip_rect.left_top(),
        egui::pos2(mid_x, chip_rect.bottom()),
    );
    let adv_zone = egui::Rect::from_min_max(
        egui::pos2(mid_x, chip_rect.top()),
        chip_rect.right_bottom(),
    );
    let easy_resp = ui.interact(easy_zone,
        ui.id().with("mode_pill_easy"), egui::Sense::click());
    let adv_resp = ui.interact(adv_zone,
        ui.id().with("mode_pill_adv"), egui::Sense::click());
    if easy_resp.clicked() { *do_set_mode = Some(settings::UiMode::Easy); }
    if adv_resp.clicked()  { *do_set_mode = Some(settings::UiMode::Advanced); }
    if easy_resp.hovered() || adv_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}
