//! Easy-mode central panel — preset dropdown + favorites + sub-patch UI.
//!
//! Top strip:
//!   * Preset dropdown grouped Favorites → Factory → User. Each entry
//!     has a star button (toggle favorite) and, inside the Favorites
//!     section, drag-handles that let the user reorder.
//!   * Active-preset display with re-pick / dirty indicator.
//! Body:
//!   * If the canvas isn't preset-shaped → "incompatible" message.
//!   * Otherwise renders the sub-patch's inner snarl via
//!     `Canvas::show`, the same path the Sub-Patch editor window
//!     uses. Changes are written back into the outer sub-patch
//!     node's `subpatch` field each frame.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use eframe::egui;
use egui_snarl::NodeId;
use flexinput_core::{ModuleDescriptor, Signal};

use crate::app::request_repaint_throttled;
use flexinput_devices::PhysicalDevice;

use crate::app::{EasyState, PanicShortcut};
use crate::canvas::node::UiSubPatch;
use crate::canvas::{Canvas, DeviceParamDefaults, NodeData};
use super::presets::{load_index, PresetInfo, PresetSource};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Compat {
    Ok,
    Incompatible,
}

pub fn show(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    easy_state: &mut EasyState,
    tab_overlay: &mut crate::canvas::OverlayLayout,
    tab_config: &mut crate::canvas::OverlayLayout,
    user_presets_folder: Option<&Path>,
    defaults: DeviceParamDefaults,
    descriptors: &[ModuleDescriptor],
    physical_devices: &[PhysicalDevice],
    live_device_ids: &HashSet<String>,
    live_signals: &HashMap<(String, String), Signal>,
    panic_shortcut: &PanicShortcut,
    device_rates: &HashMap<String, u32>,
    favorites: &mut Vec<PathBuf>,
    gamepad_nav: &mut crate::gamepad_nav::GamepadNav,
) {
    let presets = load_index(user_presets_folder);
    // Lazy restore of the loaded-preset link after app reopen:
    //   1. If easy_state.loaded_preset is present but its hash is 0
    //      (sentinel meaning "just deserialized from disk"), compute
    //      the live sub-patch hash and store it. The link sticks
    //      iff the saved path still resolves AND its file's hash
    //      matches the live sub-patch hash.
    //   2. If the saved path is gone (file moved / deleted) we fall
    //      back to scanning the preset index for a content-hash
    //      match against the live sub-patch.
    restore_preset_link(canvas, easy_state, &presets);
    let compat = check_compat(canvas);

    // Resolve gamepad preset-nav state (open flag + highlight) BEFORE the
    // dropdown renders, so the body can glow the highlighted row this frame.
    show_gamepad_preset_nav(ui, canvas, easy_state, &presets, favorites, gamepad_nav);
    show_preset_picker(ui, canvas, easy_state, tab_overlay, tab_config, &presets, favorites);
    show_pending_modal(ui, canvas, easy_state, &presets);

    // A preset / sub-patch load just replaced a sub-patch node — swap that
    // node's overlay/config items for the freshly-loaded ones (the preset ships
    // its own). Replace (not merge): drop the previous preset's pins and
    // decorations for the node, then materialize the new set.
    if let Some(sp) = easy_state.overlay_reload_node.take() {
        crate::canvas::node::remove_subpatch_overlays(
            &canvas.snarl, sp, tab_overlay, tab_config);
        crate::canvas::node::materialize_subpatch_overlays(
            &canvas.snarl, tab_overlay, tab_config);
    }

    // Layout editing has no panel of its own here: its tools take the preset bar's row
    // (`show_preset_picker`) and the selection's options are the left panel's Inspector
    // (`show_layout_inspector`), so turning it on moves nothing below.

    ui.add_space(6.0);
    ui.separator();

    match compat {
        Compat::Incompatible => {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading("Current patch is incompatible with Easy mode");
                ui.label("Switch to Advanced mode and remove any modules apart from a Sub-patch.");
            });
        }
        Compat::Ok => {
            if find_subpatch(canvas).is_none() {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.label(egui::RichText::new("Pick a preset above to begin.").weak());
                });
            } else {
                let cursor = if gamepad_nav.cursor_visible {
                    Some(gamepad_nav.cursor_pos)
                } else { None };
                render_subpatch_body(
                    ui, canvas, defaults,
                    descriptors, physical_devices, live_device_ids,
                    live_signals, panic_shortcut, device_rates, cursor,
                );
            }
        }
    }
}

// ── Preset dropdown ─────────────────────────────────────────────────

/// Globally-stable ctx-data id for the dropdown open flag. Stable so
/// nested UIs (the dropdown body Area) can write back to it.
fn preset_dropdown_open_id() -> egui::Id {
    egui::Id::new("flexinput_easy_preset_dropdown_open")
}

/// Per-subpatch ctx-data ids for the rename in-place editor state.
fn preset_rename_editing_id(outer_id: Option<NodeId>) -> egui::Id {
    egui::Id::new(("easy_preset_name_editing",
        outer_id.map(|n| n.0).unwrap_or(0)))
}
fn preset_rename_buffer_id(outer_id: Option<NodeId>) -> egui::Id {
    egui::Id::new(("easy_preset_name_buffer",
        outer_id.map(|n| n.0).unwrap_or(0)))
}

fn show_preset_picker(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    easy_state: &mut EasyState,
    tab_overlay: &crate::canvas::OverlayLayout,
    tab_config: &crate::canvas::OverlayLayout,
    presets: &[PresetInfo],
    favorites: &mut Vec<PathBuf>,
) {
    ui.add_space(6.0);

    let active_path = easy_state.loaded_preset.as_ref().map(|(p, _)| p.clone());
    let active_is_fav = active_path.as_ref()
        .map(|p| favorites.iter().any(|f| f == p))
        .unwrap_or(false);
    // Resolve the displayed preset name:
    //   1. live sub-patch's `display_name` (user title) if non-empty
    //   2. otherwise the loaded preset file's stem
    //   3. otherwise "(unsaved)"
    let outer_id = find_subpatch(canvas);
    let live_title: String = outer_id
        .and_then(|id| canvas.snarl.get_node(id))
        .and_then(|n| n.subpatch.as_ref())
        .map(|sp| sp.display_name.clone())
        .unwrap_or_default();
    let name_display = if !live_title.is_empty() {
        live_title.clone()
    } else if let Some(p) = active_path.as_ref() {
        p.file_stem().map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "(none)".into())
    } else {
        "(unsaved)".into()
    };
    let dirty = is_subpatch_dirty_cached(canvas, easy_state, presets);

    // Layout: a single pill bar that extends downward when opened to
    // become one continuous body (header pill + section list below,
    // same width, same rounded outline). Save/Load icons sit OUTSIDE
    // the pill on the right.
    //
    //   Preset:   [ ▼  <name>[*]  ☆ ]   💾  📂   (collapsed)
    //   Preset:   ┌ ▼  <name>[*]  ☆ ┐   💾  📂
    //             │   Favorites      │
    //             │   …rows…         │
    //             └──────────────────┘
    let open_id = preset_dropdown_open_id();
    let is_open: bool = ui.ctx().data(|d| d.get_temp(open_id).unwrap_or(false));

    let bar_rect_out: Option<egui::Rect>;

    // Outer horizontal: pinned to a SINGLE-ROW height so the layout
    // above the sub-patch body doesn't expand to fill all remaining
    // vertical space. Cross-align Center so "Preset:" sits at the
    // bar's midline rather than top-aligned. Without the row-height
    // allocate, `with_layout(left_to_right(Center))` consumes the
    // whole panel and centers everything vertically.
    const BAR_ROW_H: f32 = 34.0;
    let row_rect = ui.available_rect_before_wrap();
    let row_rect = egui::Rect::from_min_size(
        row_rect.min, egui::vec2(row_rect.width(), BAR_ROW_H));
    // Advance the parent cursor past the row we just claimed.
    ui.allocate_rect(row_rect, egui::Sense::hover());

    // The two mode buttons sit at the row's far right whatever is in front of them: 🎨
    // turns layout editing on and off, 🔧 opens the sub-patch editor, or brings it up if it
    // is open already.
    let layout_unlocked = outer_id
        .and_then(|id| canvas.snarl.get_node(id))
        .map(|n| n.extra.layout_unlocked)
        .unwrap_or(false);
    const MODE_BTN: f32 = 28.0;
    let modes_w = 2.0 * MODE_BTN + 4.0 + 8.0;
    let modes_rect = egui::Rect::from_min_max(
        egui::pos2(row_rect.right() - modes_w, row_rect.top()),
        row_rect.max,
    );
    {
        let mut modes_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(modes_rect)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let ui = &mut modes_ui;
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add_space(8.0);
        if ui.add_sized([MODE_BTN, MODE_BTN],
            egui::Button::new(egui::RichText::new("🔧").size(crate::settings::ui_size(14.0))))
            .on_hover_text("Open the sub-patch editor (add/wire modules), or bring it up if it's open")
            .clicked()
        {
            if let Some(oid) = outer_id {
                // Same mechanism the Advanced canvas uses: stash the request;
                // `show_subpatch_editors` consumes it next frame, and spawns the
                // editor window or brings the open one up.
                canvas.pending_edit_subpatch = Some(oid);
            }
        }
        let palette_btn = egui::Button::selectable(
            layout_unlocked, egui::RichText::new("🎨").size(crate::settings::ui_size(14.0)));
        if ui.add_sized([MODE_BTN, MODE_BTN], palette_btn)
            .on_hover_text(if layout_unlocked {
                "Layout edit: ON — drag, resize, and select elements.\nClick to lock."
            } else {
                "Layout edit: OFF — click to arrange elements on the sub-patch body."
            })
            .clicked()
        {
            if let Some(oid) = outer_id {
                if let Some(n) = canvas.snarl.get_node_mut(oid) {
                    n.extra.layout_unlocked = !layout_unlocked;
                }
                // The preset list belongs to the bar the editor's tools replace.
                ui.ctx().data_mut(|d| d.insert_temp(open_id, false));
                // (The Easy I/O nodes reflow to the sub-patch's new size on the
                // next frame via the per-frame reposition in app.rs.)
            }
        }
    }

    let row_rect = egui::Rect::from_min_max(row_rect.min, egui::pos2(modes_rect.left(), row_rect.bottom()));
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    // Nothing in the row reaches the mode buttons, however narrow the window or large the UI
    // scale: what doesn't fit gives way first (`fits`), and the rest is cut at their edge.
    row_ui.set_clip_rect(row_ui.clip_rect().intersect(row_rect));
    let strong_w = |ui: &egui::Ui, text: &str| {
        let font = egui::TextStyle::Body.resolve(ui.style());
        ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::PLACEHOLDER).size().x
    };
    // Clamp so the row's content never slides under the Easy-mode left panel's tab (its
    // right edge is published by app.rs each frame). When the left panel is expanded the
    // tab sits at the panel seam, left of this row, so the clamp is a no-op; when collapsed
    // the tab pins to the window top-left over this row and the clamp pushes it right.
    let devices_label_right: f32 = ui.ctx()
        .data(|d| d.get_temp(egui::Id::new("easy_devices_label_right_x")).unwrap_or(0.0));
    let min_pad = (devices_label_right - row_rect.min.x + 8.0).max(0.0);
    if layout_unlocked {
        // Layout editing: the editor's own tools in the preset bar's place - snap and its
        // grid, and the things to add - with the selection's options in the Inspector.
        let ui = &mut row_ui;
        bar_rect_out = None;
        ui.add_space(min_pad);
        // The title only where the tools still fit beside it (six squares, the grid's pill, the
        // tab picker and the rules between them).
        const TOOLS_W: f32 = 6.0 * 28.0 + 104.0 + 150.0 + 36.0;
        let title_w = strong_w(ui, "Layout editor");
        if ui.available_width() >= title_w + 8.0 + TOOLS_W {
            ui.label(egui::RichText::new("Layout editor").strong());
            ui.add_space(8.0);
        }
        if let Some(sp) = outer_id
            .and_then(|id| canvas.snarl.get_node_mut(id))
            .and_then(|n| n.subpatch.as_mut())
        {
            crate::canvas::viewer::layout_toolbar_controls_core(
                ui, &mut crate::canvas::viewer::LayoutStateMut::of_subpatch(sp));
        }
    } else {
        let ui = &mut row_ui;
        // The Save and Load buttons and the space before them.
        let trailing_w = 2.0 * MODE_BTN + 4.0 + 10.0;
        // What the row has past the left panel's tab. The bar takes what's left of it after
        // its label and the buttons - up to 560, down to 220 while that leaves room, then
        // narrower still; and when even a 160-point bar wouldn't fit beside the label, the
        // label gives way. They collided once the UI was scaled up: the bar never went under
        // 220 and the label was taken to be 56 points whatever its size.
        let room = (ui.available_width() - min_pad).max(0.0);
        let label_w = strong_w(ui, "Preset:") + ui.spacing().item_spacing.x;
        let show_label = room >= label_w + 160.0 + trailing_w + 16.0;
        let label_w = if show_label { label_w } else { 0.0 };
        let fit = room - label_w - trailing_w - 16.0;
        let bar_w = if fit >= 220.0 { fit.min(560.0) } else { fit.max(60.0) };
        // The cluster in the middle of what's left, never under the tab.
        let pad = ((room - label_w - bar_w - trailing_w - 16.0) * 0.5).max(0.0) + min_pad;
        ui.add_space(pad);

        if show_label {
            ui.label(egui::RichText::new("Preset:").strong());
        }

        // Edit-in-place state for the bar's name slot. When `editing`
        // is true, `preset_dropdown_bar` renders a TextEdit inside
        // name_rect INSTEAD OF painting the static name text — the
        // bar becomes the input field itself, no overlay misalign.
        let editing_key = preset_rename_editing_id(outer_id);
        let buffer_key  = preset_rename_buffer_id(outer_id);
        let editing: bool = ui.ctx().data(|d| d.get_temp(editing_key).unwrap_or(false));

        let bar_resp = preset_dropdown_bar(
            ui, bar_w, &name_display, dirty,
            active_is_fav, active_path.is_some(),
            is_open,
            editing,
        );
        bar_rect_out = Some(bar_resp.bar_rect);
        // Only react to bar/popup clicks when NOT editing — clicks
        // inside the TextEdit shouldn't toggle the dropdown.
        if !editing && bar_resp.bar_clicked {
            let new_state = !is_open;
            ui.ctx().data_mut(|d| d.insert_temp(open_id, new_state));
        }
        if bar_resp.star_clicked {
            if let Some(p) = &active_path {
                if active_is_fav { favorites.retain(|f| f != p); }
                else { favorites.push(p.clone()); }
            }
        }
        if !editing && (bar_resp.name_double_clicked || bar_resp.name_alt_clicked) {
            ui.ctx().data_mut(|d| {
                d.insert_temp(editing_key, true);
                d.insert_temp(buffer_key, name_display.clone());
            });
        }

        // Commit / cancel edits if the TextEdit reported a finish event.
        if let Some(commit) = bar_resp.rename_commit {
            let trimmed = commit.trim().to_string();
            if let Some(oid) = outer_id {
                if let Some(n) = canvas.snarl.get_node_mut(oid) {
                    // Update BOTH the outer node's display_name (shown
                    // in the Advanced-mode canvas header) AND the
                    // inner UiSubPatch.display_name (used as the
                    // default filename when saving a .fxsp).
                    n.display_name = trimmed.clone();
                    if let Some(sp) = n.subpatch.as_mut() {
                        sp.display_name = trimmed.clone();
                    }
                }
            }
            ui.ctx().data_mut(|d| {
                d.insert_temp(editing_key, false);
                d.remove::<String>(buffer_key);
            });
        } else if bar_resp.rename_cancel {
            ui.ctx().data_mut(|d| {
                d.insert_temp(editing_key, false);
                d.remove::<String>(buffer_key);
            });
        }

        // ── Save + Load: drawings on square buttons, as every Save and Load is ──
        ui.add_space(10.0);
        let icon = |ui: &mut egui::Ui, glyph: crate::canvas::viewer::glyphs::Glyph, hover: &str| -> bool {
            let resp = ui.add_sized([MODE_BTN, MODE_BTN], egui::Button::new("")).on_hover_text(hover);
            let colour = ui.style().interact(&resp).text_color();
            crate::canvas::viewer::glyphs::paint(ui.painter(), resp.rect.shrink(3.0), glyph, colour);
            resp.clicked()
        };
        if icon(ui, crate::canvas::viewer::glyphs::Glyph::Save, "Save sub-patch to .fxsp") {
            if let Some(oid) = outer_id {
                // Bake THIS tab's overlay/config items for this sub-patch (pins
                // and their decorations) into the saved copy — the tab overlays
                // are the live source of truth — so the preset ships its
                // overlays. The live tab overlays are untouched.
                if let Some(sp) = crate::canvas::node::subpatch_with_overlays(
                    &canvas.snarl, oid.0, tab_overlay, tab_config)
                {
                    if let Some(path) = crate::app::save_subpatch_file(&sp) {
                        super::presets::invalidate_index();
                        let hash = hash_subpatch(&sp);
                        easy_state.loaded_preset = Some((path, hash));
                    }
                }
            }
        }
        if icon(ui, crate::canvas::viewer::glyphs::Glyph::Load, "Load sub-patch from .fxsp") {
            if let Some(loaded) = crate::app::load_subpatch_file() {
                apply_subpatch_inline(canvas, easy_state, &loaded, None);
            }
        }
    }

    // ── Inline expansion ──────────────────────────────────────────
    // When opened, draw the section body directly below the pill bar
    // at the same horizontal extent, sharing a single visual
    // outline. We render the body via an Area pinned to just below
    // the bar so it overlaps anything underneath (separator, sub-
    // patch body) without re-flowing the layout above.
    if is_open {
        let Some(bar_rect) = bar_rect_out else { return; };
        let body_id = ui.id().with("easy_preset_dropdown_body");
        let body_top = bar_rect.bottom() - 1.0; // overlap stroke
        // Clean approach: reserve a shape index BEFORE rendering
        // content, lay the content out, then back-fill the reserved
        // shape with bg + stroke sized to (bar_w × measured_height).
        // Bg ends up BEHIND content because its shape index sits
        // earlier in the layer's draw list. No Frame, no i8 margin
        // rounding, no sublayer ordering trap — outer width is
        // exactly bar_w by construction.
        let h_pad = 8.0_f32;
        let v_pad = 6.0_f32;
        let content_w = (bar_rect.width() - 2.0 * h_pad).max(40.0);
        let area_resp = egui::Area::new(body_id)
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(bar_rect.left(), body_top))
            .show(ui.ctx(), |ui| {
                // Reserve the bg shape index FIRST so it draws below
                // everything painted later in this Area.
                let bg_idx = ui.painter().add(egui::Shape::Noop);

                let start = ui.cursor().min;
                let outer_left = start.x;
                let outer_top  = start.y;
                let outer_right = outer_left + bar_rect.width();

                // Inner content area (padded).
                let inner_rect = egui::Rect::from_min_max(
                    egui::pos2(outer_left + h_pad, outer_top + v_pad),
                    egui::pos2(outer_right - h_pad, f32::INFINITY),
                );
                let mut inner_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(inner_rect)
                        .layout(egui::Layout::top_down(egui::Align::LEFT)),
                );
                inner_ui.set_min_width(content_w);
                inner_ui.set_max_width(content_w);
                preset_dropdown_body(
                    &mut inner_ui, canvas, easy_state, presets, favorites);
                let inner_h = inner_ui.min_rect().height();

                // Final outer rect — width = bar_w exactly.
                let outer_h = inner_h + 2.0 * v_pad;
                let outer_rect = egui::Rect::from_min_size(
                    egui::pos2(outer_left, outer_top),
                    egui::vec2(bar_rect.width(), outer_h),
                );

                // Reserve the outer rect in the parent layout so the
                // Area's response.rect spans the painted region (used
                // for click-outside detection).
                let _ = ui.allocate_rect(outer_rect, egui::Sense::hover());

                // Back-fill the reserved shape index with bg + stroke.
                // RectShape::new takes (rect, corner_radius, fill, stroke, stroke_kind).
                ui.painter().set(bg_idx, egui::epaint::RectShape::new(
                    outer_rect,
                    egui::CornerRadius { nw: 0, ne: 0, sw: 8, se: 8 },
                    crate::settings::tinted(egui::Color32::from_rgb(0x1a, 0x1a, 0x1a)),
                    egui::Stroke::new(1.0_f32,
                        ui.visuals().widgets.noninteractive.bg_stroke.color),
                    egui::StrokeKind::Inside,
                ));
            });

        // Close on click outside the bar or body.
        let body_rect = area_resp.response.rect;
        let pointer_down = ui.input(|i| i.pointer.any_click());
        if pointer_down {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if !bar_rect.contains(pos) && !body_rect.contains(pos) {
                    ui.ctx().data_mut(|d| d.insert_temp(open_id, false));
                }
            }
        }
    }
}

/// Result bundle from rendering the dropdown pill bar.
struct PresetBarResult {
    /// Full pill-bar rect in screen coords, INCLUDING the star slot
    /// on the right. Used by the inline-expansion body to size itself
    /// flush with the bar's left and right edges.
    bar_rect: egui::Rect,
    bar_clicked: bool,
    star_clicked: bool,
    name_double_clicked: bool,
    name_alt_clicked: bool,
    /// When the bar is in edit mode and the user committed (Enter or
    /// focus loss), holds the final text. None means "no commit this
    /// frame".
    rename_commit: Option<String>,
    /// True for the single frame the user pressed Escape during edit.
    rename_cancel: bool,
}

/// Render the long pill bar showing `[▼ Preset name ☆]`. Returns
/// what was clicked. The bar itself is a clickable surface; the
/// star slot is a separate hit zone on the right.
fn preset_dropdown_bar(
    ui: &mut egui::Ui,
    width: f32,
    name_display: &str,
    dirty: bool,
    is_fav: bool,
    has_active_preset: bool,
    is_open: bool,
    editing: bool,
) -> PresetBarResult {
    let height = 30.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, height),
        egui::Sense::hover(),
    );

    // Bar background. Collapsed: full pill (rounded all corners).
    // Expanded: rounded TOP corners only, so the body below joins
    // seamlessly as one continuous shape. Bar fill matches the
    // dropdown body fill (#1a1a1a) for visual continuity.
    let bg = crate::settings::tinted(egui::Color32::from_rgb(0x1a, 0x1a, 0x1a));
    let stroke = egui::Stroke::new(1.0_f32,
        ui.visuals().widgets.noninteractive.bg_stroke.color);
    if is_open {
        let cr = egui::CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };
        ui.painter().rect(rect, cr, bg, stroke, egui::StrokeKind::Inside);
    } else {
        let r = (height * 0.5) as u8;
        let cr = egui::CornerRadius { nw: r, ne: r, sw: r, se: r };
        ui.painter().rect(rect, cr, bg, stroke, egui::StrokeKind::Inside);
    }

    // Star slot on the right (inside the pill).
    let star_w = 28.0;
    let star_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - star_w - 4.0, rect.top()),
        egui::vec2(star_w, height),
    );
    let star_resp = ui.interact(
        star_rect, ui.id().with("easy_preset_bar_star"),
        egui::Sense::click());
    let star_glyph = if is_fav { "★" } else { "☆" };
    let star_col = if !has_active_preset {
        ui.visuals().weak_text_color().gamma_multiply(0.5)
    } else if is_fav {
        egui::Color32::from_rgb(0xff, 0xc4, 0x4d)
    } else if star_resp.hovered() {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    ui.painter().text(
        star_rect.center(),
        egui::Align2::CENTER_CENTER,
        star_glyph,
        crate::settings::ui_font(16.0),
        star_col,
    );

    // Chevron slot on the left (inside the pill).
    let chev_w = 28.0;
    let chev_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 4.0, rect.top()),
        egui::vec2(chev_w, height),
    );
    ui.painter().text(
        chev_rect.center(),
        egui::Align2::CENTER_CENTER,
        if is_open { "▲" } else { "▼" },
        crate::settings::ui_font(13.0),
        ui.visuals().text_color(),
    );

    // Name slot fills the space between chevron and star.
    let name_rect = egui::Rect::from_min_max(
        egui::pos2(chev_rect.right() + 4.0, rect.top()),
        egui::pos2(star_rect.left() - 4.0, rect.bottom()),
    );

    // In editing mode the bar BECOMES the text field — paint nothing
    // here and let the caller-managed `rename_commit` / `rename_cancel`
    // outputs (computed below) drive the state transition.
    let mut rename_commit: Option<String> = None;
    let mut rename_cancel = false;
    if editing {
        let buffer_key = egui::Id::new("__preset_bar_inline_edit_buffer");
        // Pull the active edit buffer from ctx data so it survives
        // across frames without us needing to know the outer NodeId
        // here (caller owns the per-subpatch buffer id and seeds this
        // one).
        let mut buf: String = ui.ctx()
            .data(|d| d.get_temp::<String>(buffer_key))
            .unwrap_or_else(|| name_display.to_string());

        // Place the TextEdit inside name_rect via a child Ui pinned to
        // exact coordinates. `desired_width = name_rect.width()` makes
        // the field span the whole slot; the inner text is vertically
        // centered because the child Ui's layout cross-aligns Center.
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(name_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let resp = child.add(
            egui::TextEdit::singleline(&mut buf)
                .id(ui.id().with("easy_preset_inline_edit"))
                .desired_width(name_rect.width())
                .frame(false)
                .horizontal_align(egui::Align::Center)
                .font(crate::settings::ui_font(15.0))
                .text_color(ui.visuals().strong_text_color())
                .hint_text("Preset title"),
        );
        // Persist the in-progress buffer for the next frame.
        ui.ctx().data_mut(|d| d.insert_temp(buffer_key, buf.clone()));
        // Auto-focus on first frame of edit mode.
        if !resp.has_focus() && !resp.lost_focus() {
            resp.request_focus();
        }
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let esc   = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if esc {
            rename_cancel = true;
            ui.ctx().data_mut(|d| d.remove::<String>(buffer_key));
        } else if enter || resp.lost_focus() {
            rename_commit = Some(buf.clone());
            ui.ctx().data_mut(|d| d.remove::<String>(buffer_key));
        }
    } else {
        let name_text = if dirty {
            format!("{} *", name_display)
        } else {
            name_display.to_string()
        };
        ui.painter().text(
            name_rect.center(),
            egui::Align2::CENTER_CENTER,
            &name_text,
            crate::settings::ui_font(15.0),
            ui.visuals().strong_text_color(),
        );
    }

    // Click sensing. Skip entirely while editing so the TextEdit
    // receives clicks and the bar doesn't toggle the dropdown when
    // the user clicks inside the text field.
    let (bar_clicked, name_double_clicked, name_alt_clicked) = if editing {
        (false, false, false)
    } else {
        let bar_zone = egui::Rect::from_min_max(
            rect.min, egui::pos2(star_rect.left() - 2.0, rect.bottom()),
        );
        let bar_resp = ui.interact(
            bar_zone, ui.id().with("easy_preset_bar_open"),
            egui::Sense::click());
        let alt = ui.input(|i| i.modifiers.alt);

        // Detect double-click / alt-click on the name slot from raw
        // pointer events so they fire regardless of bar_resp shadowing.
        let pointer_pos = ui.input(|i| i.pointer.hover_pos());
        let in_name = pointer_pos.map(|p| name_rect.contains(p)).unwrap_or(false);
        let ndc = in_name
            && ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
        let nac = in_name && alt
            && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
        let bc = bar_resp.clicked() && !ndc && !nac;
        (bc, ndc, nac)
    };

    PresetBarResult {
        bar_rect: rect,
        bar_clicked,
        star_clicked: star_resp.clicked() && has_active_preset,
        name_double_clicked,
        name_alt_clicked,
        rename_commit,
        rename_cancel,
    }
}

/// (Deprecated — replaced by `preset_dropdown_bar` +
/// `maybe_show_rename_overlay`. Left here only because the old name
/// might still be referenced in stale tests during incremental
/// builds.)
#[allow(dead_code)]
fn preset_name_widget(
    ui: &mut egui::Ui,
    display_name: &str,
    dirty: bool,
    canvas: &mut Canvas,
    _easy_state: &mut EasyState,
) {
    let outer_id = find_subpatch(canvas);
    let editing_key = egui::Id::new(("easy_preset_name_editing",
        outer_id.map(|n| n.0).unwrap_or(0)));
    let buffer_key = egui::Id::new(("easy_preset_name_buffer",
        outer_id.map(|n| n.0).unwrap_or(0)));
    let editing: bool = ui.ctx().data(|d| d.get_temp(editing_key).unwrap_or(false));

    if editing {
        let mut buf: String = ui.ctx().data_mut(|d|
            d.get_temp_mut_or_insert_with(buffer_key, || display_name.to_string()).clone());
        let resp = ui.add(
            egui::TextEdit::singleline(&mut buf)
                .desired_width(220.0)
                .hint_text("Preset title"),
        );
        ui.ctx().data_mut(|d| d.insert_temp(buffer_key, buf.clone()));
        if !resp.has_focus() && !resp.lost_focus() {
            resp.request_focus();
        }
        // Commit on Enter, lost focus (click outside). Cancel on Esc.
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let esc   = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if enter || resp.lost_focus() {
            if !esc {
                if let Some(oid) = outer_id {
                    if let Some(sp) = canvas.snarl.get_node_mut(oid).and_then(|n| n.subpatch.as_mut()) {
                        sp.display_name = buf.trim().to_string();
                    }
                }
            }
            ui.ctx().data_mut(|d| {
                d.insert_temp(editing_key, false);
                d.remove::<String>(buffer_key);
            });
        }
    } else {
        let name_text = if dirty {
            format!("{} *", display_name)
        } else {
            display_name.to_string()
        };
        let label_resp = ui.add(egui::Label::new(
            egui::RichText::new(&name_text).size(crate::settings::ui_size(18.0)).strong(),
        ).sense(egui::Sense::click()))
            .on_hover_text("Double-click or Alt-click to rename");
        let alt_held = ui.input(|i| i.modifiers.alt);
        if label_resp.double_clicked() || (label_resp.clicked() && alt_held) {
            ui.ctx().data_mut(|d| {
                d.insert_temp(editing_key, true);
                d.insert_temp(buffer_key, display_name.to_string());
            });
        }
    }
}

/// Dropdown popup contents (Favorites / Factory / User). Mirrors
/// the mockup layout: centered section labels, full-width rows with
/// drag handle (hover), name, "(current)" tag, star on the right.
/// Favorites support drag-and-drop reorder via the handle.
fn preset_dropdown_body(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    easy_state: &mut EasyState,
    presets: &[PresetInfo],
    favorites: &mut Vec<PathBuf>,
) {
    if presets.is_empty() {
        ui.label(egui::RichText::new("No presets found.").weak());
        return;
    }

    let ctx = ui.ctx().clone();
    let active_path = easy_state.loaded_preset.as_ref().map(|(p, _)| p.clone());

    // ── Favorites ─────────────────────────────────────────────
    let fav_entries: Vec<&PresetInfo> = favorites.iter()
        .filter_map(|p| presets.iter().find(|info| info.path == *p))
        .collect();
    if !fav_entries.is_empty() {
        section_header(ui, "Favorites");
        // Drag-and-drop reorder state — index of the row currently
        // being dragged, persisted across frames via ctx data.
        let drag_id = ui.id().with("fav_drag_src");
        let mut to_move: Option<(usize, usize)> = None;
        for (i, p) in fav_entries.iter().enumerate() {
            let action = preset_row(
                ui, p, &active_path,
                true, /* is_fav — always in favorites section */
                "fav",
                Some((i, drag_id)),
            );
            handle_row_action(action, p, canvas, easy_state, presets, favorites, &ctx);
        }
        // Resolve drop position from pointer Y if a drag is in flight.
        if ui.ctx().is_being_dragged(drag_id) {
            // Find the row whose vertical center is closest to the
            // pointer; that becomes the drop target.
            if let Some(pointer_y) = ui.input(|i| i.pointer.hover_pos()).map(|p| p.y) {
                let rows_y_id = ui.id().with("fav_row_y_min");
                let row_height_id = ui.id().with("fav_row_height");
                let rows_top: f32 = ui.ctx().data(|d| d.get_temp(rows_y_id).unwrap_or(0.0));
                let row_h: f32 = ui.ctx().data(|d| d.get_temp(row_height_id).unwrap_or(24.0));
                if rows_top > 0.0 && row_h > 0.0 {
                    let rel = ((pointer_y - rows_top) / row_h).round() as isize;
                    let target = rel.clamp(0, (fav_entries.len() as isize - 1).max(0)) as usize;
                    let src: Option<usize> = ui.ctx().data(|d| d.get_temp(drag_id));
                    if let Some(src_idx) = src {
                        if src_idx != target { to_move = Some((src_idx, target)); }
                    }
                }
            }
        }
        if let Some((src, dst)) = to_move {
            if src < favorites.len() && dst < favorites.len() {
                let item = favorites.remove(src);
                favorites.insert(dst, item);
                ui.ctx().data_mut(|d| d.insert_temp(drag_id, dst));
            }
        }
        ui.add_space(4.0);
    }

    // ── Factory ───────────────────────────────────────────────
    let factory_entries: Vec<&PresetInfo> = presets.iter()
        .filter(|p| p.source == PresetSource::Factory)
        .collect();
    if !factory_entries.is_empty() {
        section_header(ui, "Factory presets");
        for p in &factory_entries {
            let is_fav = favorites.iter().any(|f| f == &p.path);
            let action = preset_row(ui, p, &active_path, is_fav, "factory", None);
            handle_row_action(action, p, canvas, easy_state, presets, favorites, &ctx);
        }
        ui.add_space(4.0);
    }

    // ── User ──────────────────────────────────────────────────
    let user_entries: Vec<&PresetInfo> = presets.iter()
        .filter(|p| p.source == PresetSource::User)
        .collect();
    if user_entries.is_empty() {
        section_header(ui, "User presets");
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("(Set a user presets folder in Settings)")
                .small().weak());
        });
    } else {
        section_header(ui, "User presets");
        for p in &user_entries {
            let is_fav = favorites.iter().any(|f| f == &p.path);
            let action = preset_row(ui, p, &active_path, is_fav, "user", None);
            handle_row_action(action, p, canvas, easy_state, presets, favorites, &ctx);
        }
    }
}

fn section_header(ui: &mut egui::Ui, label: &str) {
    ui.add_space(4.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(label).small().weak());
    });
    ui.add_space(2.0);
}

/// What the user did on a single preset row.
#[derive(Clone, Copy, Debug)]
enum RowAction {
    None,
    Pick,
    ToggleFavorite,
}

/// One preset row in the dropdown.
///
/// `section_tag` salts widget IDs so the same preset showing up in
/// both Favorites and Factory sections doesn't collide (egui id-clash).
/// `is_fav` is the authoritative favorite state (read by caller from
/// the favorites Vec). `drag_ctx` enables drag-handle + reorder logic
/// (Favorites section only).
fn preset_row(
    ui: &mut egui::Ui,
    p: &PresetInfo,
    active_path: &Option<PathBuf>,
    is_fav: bool,
    section_tag: &'static str,
    drag_ctx: Option<(usize, egui::Id)>,
) -> RowAction {
    let is_current = active_path.as_ref() == Some(&p.path);
    let avail_w = ui.available_width();
    let row_h = 24.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::hover(),
    );

    // Salted IDs — section_tag + path keeps every section's rows
    // distinct even when a preset legitimately appears in multiple
    // sections (e.g. a Factory preset also in Favorites).
    let row_id  = ui.id().with(("easy_preset_row",        section_tag, p.path.as_path()));
    let star_id = ui.id().with(("easy_preset_row_star",   section_tag, p.path.as_path()));
    let pick_id = ui.id().with(("easy_preset_row_pick",   section_tag, p.path.as_path()));
    let handle_id = ui.id().with(("easy_preset_row_handle", section_tag, p.path.as_path()));

    // Compute hover from pointer position directly — `allocate_exact_size`
    // returns a Response whose `hovered()` is suppressed by later
    // overlapping interactive widgets (star slot, pick zone), giving
    // a narrow strip near the star as the only "row hover" zone. We
    // want the WHOLE row to highlight, so we just check the pointer
    // against the row rect. Constrained to the dropdown body (the
    // immediate ui clip rect) so hovering above/below sibling rows
    // doesn't bleed through.
    let row_hovered = ui.input(|i| i.pointer.hover_pos())
        .map(|p| rect.contains(p) && ui.clip_rect().contains(p))
        .unwrap_or(false);
    if row_hovered {
        ui.painter().rect_filled(
            rect, 4.0,
            ui.visuals().widgets.hovered.bg_fill.gamma_multiply(0.5),
        );
    }
    // Gamepad-nav highlight: glow the row the controller is pointing at.
    let gp_highlight: Option<PathBuf> =
        ui.ctx().data(|d| d.get_temp(preset_nav_highlight_id()));
    if gp_highlight.as_ref() == Some(&p.path) {
        let accent = ui.visuals().selection.stroke.color;
        ui.painter().rect_filled(rect, 4.0, accent.gamma_multiply(0.25));
        ui.painter().rect_stroke(rect, 4.0,
            egui::Stroke::new(1.5_f32, accent), egui::StrokeKind::Inside);
    }

    // Record geometry once per favorites section so drag reorder
    // can compute drop targets from pointer Y.
    if drag_ctx.is_some() {
        if drag_ctx.map(|(i, _)| i) == Some(0) {
            ui.ctx().data_mut(|d| d.insert_temp(
                ui.id().with("fav_row_y_min"), rect.top()));
        }
        ui.ctx().data_mut(|d| d.insert_temp(
            ui.id().with("fav_row_height"), row_h));
    }

    let mut action = RowAction::None;

    // Star slot on the far right of the row.
    let star_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - 26.0, rect.top()),
        egui::vec2(22.0, row_h),
    );
    let star_resp = ui.interact(star_rect, star_id, egui::Sense::click());
    let star_glyph = if is_fav { "★" } else { "☆" };
    let star_col = if star_resp.hovered() {
        ui.visuals().strong_text_color()
    } else if is_fav {
        // Match the favorite-active color from the mockup.
        egui::Color32::from_rgb(0xff, 0xc4, 0x4d)
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().text(
        star_rect.center(),
        egui::Align2::CENTER_CENTER,
        star_glyph,
        crate::settings::ui_font(15.0),
        star_col,
    );
    if star_resp.clicked() {
        action = RowAction::ToggleFavorite;
    }

    // Drag handle slot on the far left (favorites only, on hover).
    let handle_w = 18.0_f32;
    if let Some((idx, drag_id)) = drag_ctx {
        let h_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 4.0, rect.top()),
            egui::vec2(handle_w, row_h),
        );
        let h_resp = ui.interact(h_rect, handle_id, egui::Sense::click_and_drag());
        if row_hovered || h_resp.hovered() || h_resp.dragged() {
            let stroke = egui::Stroke::new(1.5_f32, ui.visuals().weak_text_color());
            let cx = h_rect.center().x;
            let half = 5.0;
            for k in 0..3 {
                let y = h_rect.center().y - 5.0 + k as f32 * 5.0;
                ui.painter().line_segment(
                    [egui::pos2(cx - half, y), egui::pos2(cx + half, y)],
                    stroke);
            }
        }
        if h_resp.drag_started() {
            ui.ctx().data_mut(|d| d.insert_temp(drag_id, idx));
            ui.ctx().set_dragged_id(drag_id);
        }
    }

    // Name (with optional "(current)" tag). Indented past the
    // handle slot regardless of section so columns align.
    let text_x = rect.left() + handle_w + 8.0;
    let name_text = if is_current {
        format!("{}  (current)", p.display_name)
    } else {
        p.display_name.clone()
    };
    ui.painter().text(
        egui::pos2(text_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &name_text,
        crate::settings::ui_font(13.0),
        ui.visuals().text_color(),
    );

    // Body-click anywhere in the row (excluding the star slot)
    // picks the preset.
    let pick_zone = egui::Rect::from_min_max(
        rect.min,
        egui::pos2(star_rect.left() - 2.0, rect.bottom()),
    );
    let pick_resp = ui.interact(pick_zone, pick_id, egui::Sense::click());
    if pick_resp.clicked() {
        action = match action {
            RowAction::ToggleFavorite => RowAction::ToggleFavorite, // star wins if it was clicked
            _                          => RowAction::Pick,
        };
    }
    // Silence unused warning when the row_id isn't otherwise consumed.
    let _ = row_id;

    action
}

fn handle_row_action(
    action: RowAction,
    p: &PresetInfo,
    canvas: &mut Canvas,
    easy_state: &mut EasyState,
    presets: &[PresetInfo],
    favorites: &mut Vec<PathBuf>,
    ctx: &egui::Context,
) {
    match action {
        RowAction::None => {}
        RowAction::Pick => {
            let active = easy_state.loaded_preset.as_ref()
                .map(|(p2, _)| p2.as_path()) == Some(p.path.as_path());
            if !active {
                if is_subpatch_dirty(canvas, easy_state, presets) {
                    easy_state.pending_preset_switch = Some(p.path.clone());
                } else {
                    apply_preset(canvas, easy_state, p);
                }
            }
            // Close the dropdown after a pick.
            ctx.data_mut(|d| d.insert_temp(preset_dropdown_open_id(), false));
        }
        RowAction::ToggleFavorite => {
            if favorites.iter().any(|f| f == &p.path) {
                favorites.retain(|f| f != &p.path);
            } else {
                favorites.push(p.path.clone());
            }
        }
    }
}

/// Apply a sub-patch loaded directly from a file (Save/Load button
/// path). Mirrors apply_preset but doesn't require a PresetInfo —
/// the on-disk path is unknown to our index so we treat it as
/// "ad-hoc": loaded_preset is cleared.
fn apply_subpatch_inline(
    canvas: &mut Canvas,
    easy_state: &mut EasyState,
    sp: &UiSubPatch,
    source_path: Option<PathBuf>,
) {
    let existing: Option<NodeId> = find_subpatch(canvas);
    let node_id = match existing {
        Some(id) => id,
        None => {
            let mut node = NodeData {
                module_id: "subpatch".into(),
                display_name: sp.display_name.clone(),
                category: "Patch".into(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                params: HashMap::new(),
                subpatch: Some(Box::new(sp.clone())),
                extra: Default::default(),
            };
            mirror_pins(&mut node, sp);
            canvas.snarl.insert_node(canvas.spawn_position(), node)
        }
    };
    if let Some(n) = canvas.snarl.get_node_mut(node_id) {
        n.subpatch = Some(Box::new(sp.clone()));
        if !sp.display_name.is_empty() {
            n.display_name = sp.display_name.clone();
        }
        mirror_pins(n, sp);
    }
    let hash = hash_subpatch(sp);
    easy_state.loaded_preset = source_path.map(|p| (p, hash));
    easy_state.overlay_reload_node = Some(node_id.0);
    super::wiring::rewire(canvas);
    super::layout::reposition_io_nodes(canvas);
}

fn show_pending_modal(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    easy_state: &mut EasyState,
    presets: &[PresetInfo],
) {
    let Some(pending) = easy_state.pending_preset_switch.clone() else { return; };
    let pending_info = presets.iter().find(|p| p.path == pending).cloned();
    egui::Window::new("Replace sub-patch?")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            ui.label("Picking this preset will discard your changes to the current sub-patch.");
            ui.label("Continue?");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Replace").clicked() {
                    if let Some(p) = &pending_info {
                        apply_preset(canvas, easy_state, p);
                    }
                    easy_state.pending_preset_switch = None;
                }
                if ui.button("Cancel").clicked() {
                    easy_state.pending_preset_switch = None;
                }
            });
        });
}

// ── Compatibility check ─────────────────────────────────────────────

fn check_compat(canvas: &Canvas) -> Compat {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (_, n) in canvas.snarl.nodes_ids_data() {
        *counts.entry(n.value.module_id.clone()).or_default() += 1;
    }
    let allowed = ["device.source", "subpatch", "device.sink"];
    for k in counts.keys() {
        if !allowed.iter().any(|a| a == k) { return Compat::Incompatible; }
    }
    // ⭐ As many input devices as the sub-patch has AutoMap inlets.
    //
    // ❗ This was pinned at one, which made multi-device Easy mode
    // unreachable from the UI that offers it: the picker lets you choose up to
    // `input_capacity` gamepads — the wiring layer has always given each its own
    // port — and then the patch was declared incompatible the moment a second
    // one was chosen. A Joy-Con 2 grip is the obvious casualty, being two
    // devices for one pair of hands.
    //
    // The SAME helper the picker caps itself with, so the two cannot drift
    // apart again: a preset with one inlet still admits exactly one source, and
    // no preset can be configured into a state the check then rejects.
    if counts.get("device.source").copied().unwrap_or(0)
        > super::io_panel::input_capacity(canvas)
    {
        return Compat::Incompatible;
    }
    if counts.get("subpatch").copied().unwrap_or(0) > 1     { return Compat::Incompatible; }

    let mut by_kind: HashMap<String, usize> = HashMap::new();
    for (_, n) in canvas.snarl.nodes_ids_data() {
        if n.value.module_id != "device.sink" { continue; }
        if let Some(did) = n.value.params.get("device_id").and_then(|v| v.as_str()) {
            let prefix: String = did.split('.').take(2).collect::<Vec<_>>().join(".");
            *by_kind.entry(prefix).or_default() += 1;
        }
    }
    if by_kind.values().any(|v| *v > 1) { return Compat::Incompatible; }
    Compat::Ok
}

#[cfg(test)]
mod compat_tests {
    use super::*;
    use crate::canvas::NodeData;
    use flexinput_core::{PinDescriptor, SignalType};

    fn node(module_id: &str, automap_inlets: usize) -> NodeData {
        NodeData {
            module_id: module_id.into(),
            display_name: String::new(),
            category: String::new(),
            inputs: (0..automap_inlets)
                .map(|_| PinDescriptor::new("in", SignalType::AutoMap))
                .collect(),
            outputs: Vec::new(),
            params: Default::default(),
            subpatch: None,
            extra: Default::default(),
        }
    }

    fn canvas_with(inlets: usize, sources: usize) -> Canvas {
        let mut c = Canvas::default();
        c.snarl.insert_node(egui::pos2(0.0, 0.0), node("subpatch", inlets));
        for i in 0..sources {
            c.snarl
                .insert_node(egui::pos2(0.0, 40.0 * (i + 1) as f32), node("device.source", 0));
        }
        c
    }

    /// ⛔ A preset with N AutoMap inlets must accept N input devices.
    ///
    /// The picker offers exactly that many, so anything stricter here declares
    /// the patch broken for a configuration the UI just invited. A Joy-Con 2
    /// grip is two devices for one pair of hands, and this is what stopped it
    /// working in Easy mode.
    #[test]
    fn a_multi_inlet_preset_accepts_one_device_per_inlet() {
        for n in 1..=4 {
            assert!(
                matches!(check_compat(&canvas_with(n, n)), Compat::Ok),
                "{n} inlets with {n} sources was rejected",
            );
        }
    }

    /// …and one more than it has inlets is genuinely incompatible: there is no
    /// port left to wire it to.
    #[test]
    fn more_devices_than_inlets_is_still_incompatible() {
        assert!(matches!(check_compat(&canvas_with(2, 3)), Compat::Incompatible));
        assert!(matches!(check_compat(&canvas_with(1, 2)), Compat::Incompatible));
    }

    /// The single-inlet case must behave exactly as it always did — that is the
    /// whole existing preset library.
    #[test]
    fn a_single_inlet_preset_is_unchanged() {
        assert!(matches!(check_compat(&canvas_with(1, 1)), Compat::Ok));
        assert!(matches!(check_compat(&canvas_with(1, 0)), Compat::Ok));
    }
}

fn find_subpatch(canvas: &Canvas) -> Option<NodeId> {
    canvas.snarl.nodes_ids_data()
        .find(|(_, n)| n.value.module_id == "subpatch")
        .map(|(id, _)| id)
}

/// The Easy sub-patch whose layout is being edited, if it is.
pub(crate) fn layout_editing_subpatch(canvas: &Canvas) -> Option<NodeId> {
    find_subpatch(canvas)
        .filter(|&id| canvas.snarl.get_node(id).is_some_and(|n| n.extra.layout_unlocked))
}

/// The Easy left panel while the layout is being edited: the Inspector, in the Devices
/// panel's place - where the selected object sits and how big it is, and its options.
pub(crate) fn show_layout_inspector(ui: &mut egui::Ui, canvas: &mut Canvas, oid: NodeId) {
    egui::ScrollArea::vertical()
        .id_salt(("easy_layout_inspector", oid.0))
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            ui.add_space(44.0);
            let sel_module = crate::canvas::viewer::subpatch_selected_module_info(&canvas.snarl, oid);
            let Some(sp) = canvas.snarl.get_node_mut(oid).and_then(|n| n.subpatch.as_mut()) else { return };
            let Some(idx) = sp.selected_item.filter(|&i| i < sp.items.len()) else {
                ui.label(egui::RichText::new("Select an object on the layout to see its options here.").weak());
                return;
            };
            // A pin is named for the module it shows, and which part of it.
            let title = match &sp.items[idx] {
                crate::canvas::node::LayoutItem::Module(m) => {
                    let part = m.element_id.replace('_', " ");
                    let module = sp.snarl.get_node(NodeId(m.inner_node_id)).map(|n| n.display_name.clone()).unwrap_or_default();
                    Some(if module.is_empty() { part } else { format!("{module} · {part}") })
                }
                crate::canvas::node::LayoutItem::Deco(_) => None,
            };
            crate::canvas::viewer::layout_inspector(
                ui, &mut crate::canvas::viewer::LayoutStateMut::of_subpatch(sp), sel_module, title.as_deref(), None);
        });
}

// ── Sub-patch body render ───────────────────────────────────────────

/// The `Area` layer the Easy sub-patch body of `outer_id` renders in. Every nav
/// glow on the body names it as its host, so a window stacked over the body
/// covers the glow too (`widgets::nav_glow_layer`).
pub(crate) fn body_area_layer(outer_id: NodeId) -> egui::LayerId {
    egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new(("easy_subpatch_body_area", outer_id.0)),
    )
}

/// Ctx-data slot of the gamepad-nav focus inside the selected widget of the Easy
/// body of `outer_id`: `(pass, item index, body-local rect)`, written after the
/// body renders for the next pass's auto-scroll.
fn nav_focus_id(outer_id: NodeId) -> egui::Id {
    egui::Id::new(("easy_nav_focus", outer_id.0))
}

fn render_subpatch_body(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    _defaults: DeviceParamDefaults,
    _descriptors: &[ModuleDescriptor],
    _physical_devices: &[PhysicalDevice],
    _live_device_ids: &HashSet<String>,
    live_signals: &HashMap<(String, String), Signal>,
    panic_shortcut: &PanicShortcut,
    _device_rates: &HashMap<String, u32>,
    nav_cursor: Option<egui::Pos2>,
) {
    let Some(outer_id) = find_subpatch(canvas) else { return; };

    // Measure the sub-patch's natural body bbox so we can pick a
    // scale that fits the panel width without overflow.
    const PAD: f32 = 16.0;
    let (natural_w, natural_h) = canvas.snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .map(|sp| {
            let (mut mx, mut my) = (0.0f32, 0.0f32);
            for it in &sp.items {
                let (lp, ls) = it.bbox();
                mx = mx.max(lp[0] + ls[0]);
                my = my.max(lp[1] + ls[1]);
            }
            ((mx + PAD).max(120.0), (my + PAD).max(40.0))
        })
        .unwrap_or((120.0, 40.0));

    // With two tabs or more, their row across the top of the panel - staying put as the layout
    // scrolls, and not scaled with it - and the layout beneath.
    let (row_tabs, row_active, row_edit) = canvas.snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref().map(|sp| (sp.layout_tabs.clone(), sp.active_tab, n.extra.layout_unlocked)))
        .unwrap_or_default();
    if row_tabs.len() >= 2 {
        let (row_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), crate::canvas::viewer::TAB_ROW_H),
            egui::Sense::hover(),
        );
        let act = crate::canvas::viewer::tab_row(
            ui, row_rect, &row_tabs, row_active, row_edit, egui::Id::new(("easy_tab_row", outer_id.0)),
        );
        if act.any() {
            if let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) {
                act.apply(&mut sp.layout_tabs, &mut sp.items, &mut sp.active_tab, &mut sp.selected_item, &mut sp.selected_items);
            }
        }
    }

    let panel_rect = ui.available_rect_before_wrap();
    let panel_w = panel_rect.width().max(40.0);
    let panel_h = panel_rect.height().max(40.0);
    // Width-fit scale: stretch the body to fill the panel width, both
    // down (when the window is small) and up to a 1.5x cap (when there's
    // unused horizontal room). Scaled height drives vertical-scroll
    // overflow. The 1.5 cap keeps text/widgets from ballooning on very
    // wide windows; the 0.05 floor avoids a degenerate zero scale.
    const MAX_UPSCALE: f32 = 1.5;
    let scale = (panel_w / natural_w).clamp(0.05, MAX_UPSCALE);
    let scaled_w = natural_w * scale;
    let scaled_h = natural_h * scale;
    let scroll_overflow = (scaled_h - panel_h).max(0.0);

    // Persistent scroll offset (screen pixels, post-scale). Clamped.
    let scroll_id = egui::Id::new(("easy_subpatch_body_scroll", outer_id.0));
    let mut scroll_y: f32 = ui.ctx().data(|d| d.get_temp(scroll_id).unwrap_or(0.0));
    scroll_y = scroll_y.clamp(0.0, scroll_overflow);

    if scroll_overflow > 0.0 {
        let pointer_over = ui.ctx().input(|i| {
            i.pointer.hover_pos().map(|p| panel_rect.contains(p)).unwrap_or(false)
        });
        if pointer_over && !crate::canvas::wheel::widget_has_wheel(ui.ctx()) {
            let wheel = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                scroll_y = (scroll_y - wheel).clamp(0.0, scroll_overflow);
            }
        }
    }
    // Any finger that came down on the body scrolls it, unless it came down on a list
    // inside that can scroll: that one is the list's.
    let dy = crate::touch::finger_scroll(
        ui.ctx(), scroll_id, panel_rect, body_area_layer(outer_id),
        crate::touch::ScrollRank::Page, scroll_overflow > 0.0,
    );
    scroll_y = (scroll_y - dy).clamp(0.0, scroll_overflow);

    // Auto-scroll to keep the gamepad-nav focus (or the RS cursor) in view.
    let cur_pass = ui.ctx().cumulative_pass_nr();
    let focus_id = nav_focus_id(outer_id);
    let selected = canvas.snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .and_then(|sp| sp.selected_item);
    let prev_sel_id = egui::Id::new(("easy_nav_prev_sel", outer_id.0));
    let newly_selected = ui.ctx().data(|d| d.get_temp::<Option<usize>>(prev_sel_id)) != Some(selected);
    ui.ctx().data_mut(|d| d.insert_temp(prev_sel_id, selected));
    if scroll_overflow > 0.0 {
        let margin = 24.0_f32;
        let nav_owns: bool = {
            let stamp: Option<u64> = ui.ctx().data(|d|
                d.get_temp(egui::Id::new("gp_nav_owns_selection")));
            stamp == Some(cur_pass)
        };
        // Not while the mouse or a trackpad has the layout: following the pad's selection
        // fought every scroll of the wheel back to it.
        if nav_owns && !crate::widgets::pointer_owns(ui.ctx()) {
            // What to keep in view, in screen-Y post-scale, pre-scroll offset —
            // the space whose visible window is [scroll_y, scroll_y + panel_h].
            // Inside the selected widget it's the focus there (a card, a field),
            // stored body-local by last pass's render; else the widget itself.
            // Either way `nav_reveal_delta` never asks for both ends of something
            // taller than the panel: fitting a widget that can't fit is what made
            // it flicker up and down. A tall widget scrolls only as the focus in
            // it goes past an edge, its top going out of view.
            let focus = ui.ctx().data(|d| d.get_temp::<(u64, usize, egui::Rect)>(focus_id))
                .filter(|(p, idx, _)| cur_pass.saturating_sub(*p) <= 1 && Some(*idx) == selected)
                .map(|(_, _, r)| (r.top() * scale, r.bottom() * scale));
            let item = canvas.snarl.get_node(outer_id)
                .and_then(|n| n.subpatch.as_ref())
                .and_then(|sp| selected.and_then(|i| sp.items.get(i)))
                .map(|item| { let (p, s) = item.bbox(); (p[1] * scale, (p[1] + s[1]) * scale) });
            // Arriving on a widget with nothing above it shows the top of the
            // layout — the titles over it — so long as the widget still fits
            // whole under them. Once, on arrival: the wheel stays free after.
            if let (true, None, Some((_, bot))) = (newly_selected, focus, item) {
                let topmost = canvas.snarl.get_node(outer_id)
                    .and_then(|n| n.subpatch.as_ref())
                    .is_some_and(|sp| crate::gamepad_nav::nearest_item_toward(
                        &sp.items, selected, crate::gamepad_nav::NavStep::UP,
                        |i| crate::app::FlexInputApp::sp_item_is_editable(sp, i),
                    ).is_none());
                if topmost && bot + margin <= panel_h {
                    scroll_y = 0.0;
                }
            }
            if let Some((top, bot)) = focus.or(item) {
                let d = crate::widgets::nav_reveal_delta(top, bot, scroll_y, scroll_y + panel_h, margin);
                scroll_y = (scroll_y + d).clamp(0.0, scroll_overflow);
            }
        }
        // Keep the RS cursor in view too (when visible).
        if let Some(cpos) = nav_cursor {
            let cy = cpos.y - panel_rect.top();
            if cy < margin {
                scroll_y = (scroll_y - (margin - cy)).clamp(0.0, scroll_overflow);
            } else if cy > panel_h - margin {
                scroll_y = (scroll_y + (cy - (panel_h - margin))).clamp(0.0, scroll_overflow);
            }
        }
    }

    // Convert scroll offset to body-local coords (post-scale-inverse).
    let scroll_body = scroll_y / scale;

    // ── Pre-render snapshot for undo capture of pinned-widget value edits ────
    // The Easy-mode body renders the sub-patch's inner snarl directly through
    // `show_subpatch_body` below — it bypasses `Canvas::show()`, so the
    // value-edit detector there never sees these mutations. We replicate the
    // same gate-and-snapshot the canvas uses: clone the pre-mutation snarl only
    // on frames where the user is plausibly interacting, then hand it to
    // `track_value_edits` after the render so a settled gesture (slider release,
    // value entered, toggle click) commits exactly one undo entry.
    let needs_snapshot = ui.ctx().input(|i| {
        i.pointer.any_down()
            || crate::touch::fingers_working(i)
            || i.pointer.any_released()
            || i.pointer.any_pressed()
            || i.events.iter().any(|e| matches!(e,
                egui::Event::Key { pressed: true, .. } | egui::Event::Text(_)))
    });
    let pre_snapshot = if needs_snapshot { Some(canvas.undo_clone()) } else { None };

    let area_layer = body_area_layer(outer_id);
    // Body layer transform: scale + translate so body-local coords map to
    // (panel_rect.min + body_pos*scale - (0, scroll_y)). Fixed for the pass
    // (a scrollbar drag below lands next pass); the nav focus published while
    // the body renders maps back to body-local through it.
    let xform = egui::emath::TSTransform::new(
        panel_rect.min.to_vec2() - egui::vec2(0.0, scroll_y),
        scale,
    );
    egui::Area::new(area_layer.id)
        .order(area_layer.order)
        .movable(false)
        .interactable(true)
        .fixed_pos(panel_rect.min)
        .constrain_to(panel_rect)
        .show(ui.ctx(), |ui| {
            ui.set_clip_rect(panel_rect);

            // Dedicated body layer, transformed by `xform`.
            let body_layer = egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new(("easy_body_scale_layer", outer_id.0)),
            );
            ui.ctx().set_sublayer(ui.layer_id(), body_layer);
            ui.ctx().set_transform_layer(body_layer, xform);

            // Child UI is on body_layer; its max_rect & clip_rect are
            // expressed in BODY-LOCAL coords. body-local origin is at
            // (0, 0). The visible window in body coords is panel_rect
            // mapped through xform.inverse():
            //   body_min = (panel_rect.min - translate) / scale = (0, scroll_body)
            //   body_max = (panel_rect.max - translate) / scale
            let body_clip = egui::Rect::from_min_max(
                egui::pos2(0.0, scroll_body),
                egui::pos2(panel_w / scale, scroll_body + panel_h / scale),
            );
            let body_max_rect = egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(natural_w, natural_h),
            );
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .layer_id(body_layer)
                    .max_rect(body_max_rect),
            );
            child.set_clip_rect(body_clip);
            let _ = crate::canvas::viewer::show_subpatch_body(
                outer_id,
                &mut child,
                &mut canvas.snarl,
                live_signals,
                panic_shortcut,
                None,
                false,
            );

            // Custom scrollbar painted into the Area's parent layer
            // (NOT body_layer) so it stays stationary while scrolling.
            if scroll_overflow > 0.0 {
                let sb_w = 6.0_f32;
                let track_rect = egui::Rect::from_min_size(
                    egui::pos2(panel_rect.right() - sb_w - 2.0, panel_rect.top() + 2.0),
                    egui::vec2(sb_w, panel_h - 4.0),
                );
                let painter = ui.painter().with_clip_rect(panel_rect);
                painter.rect_filled(track_rect, 3.0,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14));
                let visible_frac = (panel_h / scaled_h).clamp(0.05, 1.0);
                let thumb_h = (track_rect.height() * visible_frac).max(20.0);
                let scroll_frac = scroll_y / scroll_overflow;
                let thumb_y = track_rect.top() + (track_rect.height() - thumb_h) * scroll_frac;
                let thumb_rect = egui::Rect::from_min_size(
                    egui::pos2(track_rect.left(), thumb_y),
                    egui::vec2(sb_w, thumb_h),
                );
                let thumb_id = scroll_id.with("__thumb");
                let thumb_resp = ui.interact(thumb_rect, thumb_id,
                    egui::Sense::click_and_drag());
                if thumb_resp.dragged() {
                    let track_travel = (track_rect.height() - thumb_h).max(1.0);
                    let body_per_track_px = scroll_overflow / track_travel;
                    scroll_y = (scroll_y + thumb_resp.drag_delta().y * body_per_track_px)
                        .clamp(0.0, scroll_overflow);
                }
                let thumb_col = if thumb_resp.dragged() {
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180)
                } else if thumb_resp.hovered() {
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140)
                } else {
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 90)
                };
                painter.rect_filled(thumb_rect, 3.0, thumb_col);
            }
            // A shadow along the top while the layout is scrolled down, and along the bottom
            // while there's more of it below: content cut off at the panel's edges scrolls.
            // Above the body and every pin's own layer under it, as the nav glows are.
            if scroll_overflow > 0.0 {
                let shadow = crate::widgets::nav_glow_layer(
                    ui.ctx(), egui::Order::Foreground, egui::Id::new(("easy_scroll_shadow", outer_id.0)), area_layer);
                let painter = ui.ctx().layer_painter(shadow).with_clip_rect(panel_rect);
                crate::widgets::paint_scroll_shadows(&painter, panel_rect, scroll_y > 0.5, scroll_y < scroll_overflow - 0.5);
            }
            let _ = scaled_w; // reserved for future right-edge layout
        });

    // ── Publish per-item SCREEN rects for gamepad cursor hit-testing ─────
    // The nav driver runs before this panel paints, so it can't compute these
    // itself. Stash them (in the SAME screen transform the glow uses) so next
    // frame's driver can hit-test the RS/gyro cursor against widgets and pick
    // whichever the cursor is over. One-frame latency is invisible at repaint
    // cadence. Vec of (item_index, screen_rect).
    {
        let mut rects: Vec<(usize, egui::Rect)> = Vec::new();
        if let Some(sp) = canvas.snarl.get_node(outer_id).and_then(|n| n.subpatch.as_ref()) {
            for (i, item) in sp.items.iter().enumerate() {
                let (lp, ls) = item.bbox();
                rects.push((i, xform * egui::Rect::from_min_size(
                    egui::pos2(lp[0], lp[1]), egui::vec2(ls[0].max(1.0), ls[1].max(1.0)))));
            }
        }
        let pass = ui.ctx().cumulative_pass_nr();
        ui.ctx().data_mut(|d| {
            d.insert_temp(egui::Id::new(("gp_nav_item_rects", outer_id.0)), (pass, rects))
        });
    }

    // ── Gamepad-nav selection glow ──────────────────────────────────────
    // The nav driver (run earlier this frame) stamps a temp value keyed by
    // outer_id with (pass_nr, is_editing) when a nav device is active. Draw a
    // steady glowing outline around the selected item, mapping its body-local
    // bbox through the transform the body rendered with (`xform`: the scroll
    // a scrollbar drag set above only lands next pass).
    let selected_rect = canvas.snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .and_then(|sp| selected.and_then(|i| sp.items.get(i)))
        .map(|item| {
            let (lp, ls) = item.bbox();
            xform * egui::Rect::from_min_size(
                egui::pos2(lp[0], lp[1]), egui::vec2(ls[0].max(1.0), ls[1].max(1.0)))
        });
    // On a module pinned whole, the pad is on one of its stops - a field, a graph - and that
    // is what's ringed, as a widget pinned on its own would be.
    let stop_rect = canvas.snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .and_then(|sp| selected.and_then(|i| sp.items.get(i)))
        .and_then(|it| match it { crate::canvas::node::LayoutItem::Module(m) => Some(m.inner_node_id), _ => None })
        .and_then(|inner| crate::canvas::viewer::module_part_focus_rect(ui.ctx(), inner));
    let glow: Option<(u64, bool)> =
        ui.ctx().data(|d| d.get_temp(egui::Id::new(("gp_nav_glow", outer_id.0))));
    if let Some((pass, editing)) = glow {
        if let (true, Some(rect)) = (pass == cur_pass, stop_rect.or(selected_rect)) {
            let accent = ui.visuals().selection.stroke.color;
            let [r, g, b, _] = accent.to_array();
            let base_round = 6.0_f32;

            // Edge ring + OUTWARD bloom only — never fills the widget
            // interior, so the control underneath stays fully visible
            // while editing. Bloom = concentric outside-strokes with
            // falling alpha (a true outward gradient).
            let painter = crate::widgets::nav_highlight_painter(
                ui.ctx(),
                egui::Id::new(("gp_nav_glow_top", outer_id.0)),
                area_layer,
                panel_rect,
            );
            let rings = 7;
            let max_grow = if editing { 9.0 } else { 6.0 };
            let peak = if editing { 150.0 } else { 90.0 };
            for i in 0..rings {
                let t = (i as f32 + 1.0) / rings as f32;
                let grow = t * max_grow;
                let a = (peak * (1.0 - t)).round() as u8;
                if a == 0 { continue; }
                painter.rect_stroke(
                    rect.expand(grow), base_round + grow,
                    egui::Stroke::new(2.0_f32,
                        egui::Color32::from_rgba_unmultiplied(r, g, b, a)),
                    egui::StrokeKind::Outside,
                );
            }
            painter.rect_stroke(
                rect.expand(1.0),
                base_round,
                egui::Stroke::new(if editing { 2.0 } else { 1.25_f32 },
                    accent.gamma_multiply(if editing { 1.0 } else { 0.75 })),
                egui::StrokeKind::Outside,
            );
        }
    }

    // The highlights inside the selected widget nav asked for this pass — the
    // mapping-card glow, the field ring and its label pill — paint now too, from
    // the rects the body just published, clipped to the panel like the ring
    // above. Painted by nav before the body, they trailed every scroll by a
    // frame and hung over the preset bar and the I/O panel once their card or
    // field had scrolled out of the panel.
    crate::app::draw_requested_card_glow(ui.ctx(), outer_id, area_layer, panel_rect);
    if let Some(item) = selected_rect {
        // On a module pinned whole, the field's label goes over the stop it's on.
        crate::app::draw_requested_field_hud(ui.ctx(), outer_id, stop_rect.unwrap_or(item), area_layer, panel_rect);
    }

    // Where the focus inside the selected widget is, body-local, for next pass's
    // auto-scroll. Published in global coordinates by this pass's render, so
    // this pass's transform maps it back.
    let focus = crate::app::nav_card_focus_rect(ui.ctx(), outer_id)
        .or_else(|| crate::app::nav_field_focus_rect(ui.ctx(), outer_id))
        .or(stop_rect)
        .map(|g| xform.inverse() * g)
        .filter(|r| r.is_finite());
    ui.ctx().data_mut(|d| match (focus, selected) {
        (Some(r), Some(idx)) => d.insert_temp(focus_id, (cur_pass, idx, r)),
        _ => d.remove::<(u64, usize, egui::Rect)>(focus_id),
    });

    // Capture any pinned-widget value edit made this frame into undo/redo
    // (commit-on-settle, coalesced to one entry per gesture). Only runs when we
    // took a pre-snapshot above, i.e. on interaction frames — idle frames skip
    // the serialize entirely.
    if needs_snapshot {
        canvas.track_value_edits(ui.ctx(), pre_snapshot.as_ref());
    }

    ui.ctx().data_mut(|d| d.insert_temp(scroll_id, scroll_y));
}

// ── Dirty detection + preset application ────────────────────────────

/// [`is_subpatch_dirty`] for the preset bar, drawn every frame. Working it out
/// serializes the whole sub-patch, so it's only done after an edit, a new preset,
/// or a second: long enough for an indicator, but anything that acts on the answer
/// asks [`is_subpatch_dirty`] itself.
fn is_subpatch_dirty_cached(canvas: &Canvas, easy_state: &mut EasyState, presets: &[PresetInfo]) -> bool {
    let key = (canvas.mutation_gen, easy_state.loaded_preset.as_ref().map(|(_, h)| *h));
    if let Some((at, k, dirty)) = easy_state.dirty_cache {
        if k == key && at.elapsed() < std::time::Duration::from_secs(1) {
            return dirty;
        }
    }
    let dirty = is_subpatch_dirty(canvas, easy_state, presets);
    easy_state.dirty_cache = Some((std::time::Instant::now(), key, dirty));
    dirty
}

fn is_subpatch_dirty(canvas: &Canvas, easy_state: &EasyState, presets: &[PresetInfo]) -> bool {
    let Some(outer_id) = find_subpatch(canvas) else { return false; };
    let Some(sp) = canvas.snarl.get_node(outer_id).and_then(|n| n.subpatch.as_ref()) else {
        return false;
    };
    let current_hash = hash_subpatch(sp);
    if let Some((_, loaded_hash)) = &easy_state.loaded_preset {
        if *loaded_hash == current_hash { return false; }
    }
    !presets.iter().any(|p| p.content_hash == current_hash)
}

/// Gamepad-driven preset dropdown navigation. Opened by a Start tap; the nav
/// driver feeds directional steps / select / open state via `gamepad_nav`.
/// Renders a simple highlighted list and applies the chosen preset on select.
/// Build the preset list in the SAME visual order the dropdown body renders:
/// Favorites (in favorites order) → Factory → User. Returns the ordered paths.
fn preset_display_order(presets: &[PresetInfo], favorites: &[PathBuf]) -> Vec<PathBuf> {
    let mut order: Vec<PathBuf> = Vec::new();
    // Favorites first, in favorites order, only those that resolve.
    for f in favorites {
        if presets.iter().any(|p| &p.path == f) && !order.contains(f) {
            order.push(f.clone());
        }
    }
    // Then Factory, then User — skipping anything already shown as a favorite.
    for src in [PresetSource::Factory, PresetSource::User] {
        for p in presets.iter().filter(|p| p.source == src) {
            if !order.contains(&p.path) {
                order.push(p.path.clone());
            }
        }
    }
    order
}

/// Ctx-data id holding the path the gamepad is currently highlighting in the
/// dropdown, so `preset_row` can draw a glow on the matching row.
fn preset_nav_highlight_id() -> egui::Id {
    egui::Id::new("flexinput_easy_preset_nav_highlight")
}

/// Gamepad-driven preset selection — drives the REAL dropdown. Tap-Start opens
/// it; dpad/stick move the highlight through the displayed order; South applies;
/// East closes. The highlighted path is published to ctx data so `preset_row`
/// glows the matching row.
fn show_gamepad_preset_nav(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    easy_state: &mut EasyState,
    presets: &[PresetInfo],
    favorites: &[PathBuf],
    gamepad_nav: &mut crate::gamepad_nav::GamepadNav,
) {
    let open_id = preset_dropdown_open_id();
    if !gamepad_nav.preset_nav_open {
        // Closed by the driver (East / re-tap Start). If we still hold a stale
        // highlight, we were the one that opened the real dropdown — close it
        // too, and clear the highlight.
        let had_highlight = ui.ctx().data(|d|
            d.get_temp::<PathBuf>(preset_nav_highlight_id()).is_some());
        if had_highlight {
            ui.ctx().data_mut(|d| {
                d.remove::<PathBuf>(preset_nav_highlight_id());
                d.insert_temp(open_id, false);
            });
        }
        return;
    }
    // Keep the real dropdown open while nav owns it.
    ui.ctx().data_mut(|d| d.insert_temp(open_id, true));

    let order = preset_display_order(presets, favorites);
    if order.is_empty() {
        gamepad_nav.preset_nav_open = false;
        ui.ctx().data_mut(|d| d.insert_temp(open_id, false));
        return;
    }
    let n = order.len();
    if gamepad_nav.preset_nav_index >= n {
        gamepad_nav.preset_nav_index = 0;
    }
    let step = std::mem::take(&mut gamepad_nav.preset_nav_step);
    if step != 0 {
        let cur = gamepad_nav.preset_nav_index as i32;
        gamepad_nav.preset_nav_index = (cur + step).rem_euclid(n as i32) as usize;
    }
    // Apply selection → same path as a mouse Pick.
    if std::mem::take(&mut gamepad_nav.preset_nav_select) {
        let path = order[gamepad_nav.preset_nav_index].clone();
        if let Some(p) = presets.iter().find(|p| p.path == path) {
            let active = easy_state.loaded_preset.as_ref()
                .map(|(p2, _)| p2.as_path()) == Some(p.path.as_path());
            if !active {
                if is_subpatch_dirty(canvas, easy_state, presets) {
                    easy_state.pending_preset_switch = Some(p.path.clone());
                } else {
                    apply_preset(canvas, easy_state, p);
                }
            }
        }
        gamepad_nav.preset_nav_open = false;
        ui.ctx().data_mut(|d| {
            d.insert_temp(open_id, false);
            d.remove::<PathBuf>(preset_nav_highlight_id());
        });
        return;
    }
    // Publish the highlighted path for `preset_row` to glow.
    let hl = order[gamepad_nav.preset_nav_index].clone();
    ui.ctx().data_mut(|d| d.insert_temp(preset_nav_highlight_id(), hl));
    request_repaint_throttled(ui.ctx());
}

fn apply_preset(canvas: &mut Canvas, easy_state: &mut EasyState, p: &PresetInfo) {
    let owned;
    let bytes: &[u8] = if let Some(b) = p.embedded_bytes {
        b
    } else {
        owned = match std::fs::read(&p.path) { Ok(b) => b, Err(_) => return };
        &owned
    };
    let Ok(file): Result<crate::app::SubPatchFile, _> = serde_json::from_slice(&bytes) else { return; };

    let existing: Option<NodeId> = find_subpatch(canvas);
    let node_id = match existing {
        Some(id) => id,
        None => {
            let mut node = NodeData {
                module_id: "subpatch".into(),
                display_name: p.display_name.clone(),
                category: "Patch".into(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                params: HashMap::new(),
                subpatch: Some(Box::new(file.sub_patch.clone())),
                extra: Default::default(),
            };
            mirror_pins(&mut node, &file.sub_patch);
            canvas.snarl.insert_node(canvas.spawn_position(), node)
        }
    };
    if let Some(n) = canvas.snarl.get_node_mut(node_id) {
        let mut sp = file.sub_patch.clone();
        // A freshly loaded preset must start with no in-progress capture/learn
        // gesture — clear transient remapper-family capture state so stale
        // captures don't block a fresh Learn.
        crate::app::clear_transient_capture_state(&mut sp);
        n.subpatch = Some(Box::new(sp));
        n.display_name = p.display_name.clone();
        mirror_pins(n, &file.sub_patch);
    }

    easy_state.loaded_preset = Some((p.path.clone(), p.content_hash));
    // The freshly-loaded sub-patch carries its own overlay/config pins — ask
    // `show` to re-materialize them onto the tab overlays for this node.
    easy_state.overlay_reload_node = Some(node_id.0);
    super::wiring::rewire(canvas);
    super::layout::reposition_io_nodes(canvas);
}

fn mirror_pins(node: &mut NodeData, sp: &UiSubPatch) {
    use flexinput_core::PinDescriptor;
    node.inputs = sp.pins_in.iter()
        .map(|p| PinDescriptor::new(&p.name, p.signal_type))
        .collect();
    node.outputs = sp.pins_out.iter()
        .map(|p| PinDescriptor::new(&p.name, p.signal_type))
        .collect();
}

/// Lazy restore of `easy_state.loaded_preset` after app reopen.
///
/// `PersistedTab`/`UiPatch` stores only the preset path; the in-memory
/// hash is initialized to 0 as a sentinel. On first frame after load
/// we either confirm the link by hashing the saved file's contents
/// against the live sub-patch, or — if the file is gone or its bytes
/// no longer match — search the preset index for a content-hash match
/// and link to whichever preset still matches.
fn restore_preset_link(canvas: &Canvas, easy_state: &mut EasyState, presets: &[PresetInfo]) {
    let Some((_, hash)) = easy_state.loaded_preset.as_ref() else { return; };
    if *hash != 0 { return; } // already restored this session
    let Some(outer_id) = find_subpatch(canvas) else {
        easy_state.loaded_preset = None;
        return;
    };
    let live_hash = canvas.snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .map(|sp| hash_subpatch(sp));
    let Some(live_hash) = live_hash else {
        easy_state.loaded_preset = None;
        return;
    };
    let saved_path = easy_state.loaded_preset.as_ref().unwrap().0.clone();
    // Path-first match: same file still on disk and bytes match live.
    if let Ok(bytes) = std::fs::read(&saved_path) {
        if let Ok(file) = serde_json::from_slice::<crate::app::SubPatchFile>(&bytes) {
            if hash_subpatch(&file.sub_patch) == live_hash {
                easy_state.loaded_preset = Some((saved_path, live_hash));
                return;
            }
        }
    }
    // Hash fallback: any preset in the index whose disk hash matches
    // the live sub-patch wins. Loses the link only if the user has
    // edited the sub-patch in any way that changes its serialization.
    if let Some(p) = presets.iter().find(|p| p.content_hash == live_hash) {
        easy_state.loaded_preset = Some((p.path.clone(), live_hash));
    } else {
        easy_state.loaded_preset = None;
    }
}

fn hash_subpatch(sp: &UiSubPatch) -> u64 {
    let bytes = serde_json::to_vec(sp).unwrap_or_default();
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in &bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// The Easy body as gamepad nav drives it, headless: a pinned Remapper taller
/// than the panel, selected, and the highlights nav asks for.
#[cfg(test)]
mod nav_scroll_tests {
    use super::*;
    use egui::pos2;

    const SCREEN: egui::Rect = egui::Rect { min: pos2(0.0, 0.0), max: pos2(800.0, 700.0) };
    /// The body's panel: preset bar above it, I/O panel to its left.
    const PANEL: egui::Rect = egui::Rect { min: pos2(100.0, 150.0), max: pos2(500.0, 450.0) };
    /// The pin's top in the body, and the body's width (its widest item + PAD).
    const PIN_TOP: f32 = 120.0;
    const BODY_W: f32 = 380.0 + 16.0;

    struct Rig {
        ctx: egui::Context,
        canvas: Canvas,
        outer: NodeId,
        t: f64,
    }

    /// What nav sets up for the body in a pass, besides owning the selection.
    #[derive(Clone, Copy)]
    enum Nav {
        /// The selected widget itself (`EditLevel::Widget`).
        Widget,
        /// In its card list, card `n` selected (`EditLevel::RemapScroll`).
        Card(usize),
        /// In its card list, on its first action button (Learn).
        Action,
    }

    /// One pass's result: the body's scroll, the focus nav keeps in view (global),
    /// and everything painted.
    struct Pass {
        scroll: f32,
        focus: Option<egui::Rect>,
        shapes: Vec<egui::epaint::ClippedShape>,
    }

    impl Rig {
        /// A short decoration on top, then a Remapper with `cards` cards whose pin
        /// is `pin_h` tall: taller than the 300 px panel. The Remapper is selected.
        fn new(cards: usize, pin_h: f32) -> Self {
            Self::with_pins(cards, &[(PIN_TOP, pin_h)], 1)
        }

        /// A short decoration on top (a title), then a pin of the one Remapper
        /// per `(top, height)`, 380 wide; item `selected` selected (the
        /// decoration is item 0).
        fn with_pins(cards: usize, pins: &[(f32, f32)], selected: usize) -> Self {
            let mut items = vec![serde_json::json!(
                { "Deco": { "Rect": { "pos": [0.0, 0.0], "size": [380.0, 40.0],
                    "fill": [40, 40, 40, 255], "stroke": [0, 0, 0, 0],
                    "stroke_px": 0.0, "corner_radius": 0.0 } } })];
            items.extend(pins.iter().map(|&(top, h)| serde_json::json!(
                { "Module": { "inner_node_id": 0, "element_id": "whole_module",
                    "pos": [0.0, top], "size": [380.0, h] } })));
            let mut sp: UiSubPatch = serde_json::from_value(serde_json::json!({
                "display_name": "Body", "pins_in": [], "pins_out": [], "items": items,
            })).expect("sub-patch json");
            let card = serde_json::json!({ "in": ["btn_south"], "out": ["btn_east"] });
            let mut params = HashMap::new();
            params.insert("mappings".to_string(), serde_json::Value::Array(vec![card; cards]));
            let inner = sp.snarl.insert_node(pos2(0.0, 0.0), NodeData {
                module_id: "module.remapper".into(), display_name: "Remapper".into(),
                category: "AutoMap".into(), inputs: vec![], outputs: vec![], params,
                subpatch: None, extra: Default::default(),
            });
            assert_eq!(inner.0, 0);
            sp.selected_item = Some(selected);
            let mut canvas = Canvas::default();
            let outer = canvas.snarl.insert_node(pos2(0.0, 0.0), NodeData {
                module_id: "subpatch".into(), display_name: String::new(), category: String::new(),
                inputs: vec![], outputs: vec![], params: Default::default(),
                subpatch: Some(Box::new(sp)), extra: Default::default(),
            });
            Rig { ctx: egui::Context::default(), canvas, outer, t: 0.0 }
        }

        /// Nav moves the selection to item `i`.
        fn select(&mut self, i: usize) {
            let sp = self.canvas.snarl.get_node_mut(self.outer)
                .and_then(|n| n.subpatch.as_mut()).expect("sub-patch");
            sp.selected_item = Some(i);
            sp.selected_items = vec![i];
        }

        fn scroll_id(&self) -> egui::Id {
            egui::Id::new(("easy_subpatch_body_scroll", self.outer.0))
        }

        /// One pass, with nav published the way its driver does before the body.
        fn pass(&mut self, nav: Nav) -> Pass {
            self.t += 1.0 / 60.0;
            let raw = egui::RawInput { screen_rect: Some(SCREEN), time: Some(self.t), ..Default::default() };
            let scroll_id = self.scroll_id();
            let Rig { ctx, canvas, outer, .. } = self;
            let outer = *outer;
            let inner = NodeId(0);
            let mut focus = None;
            let out = ctx.run(raw, |ctx| {
                let pass = ctx.cumulative_pass_nr();
                ctx.data_mut(|d| {
                    d.insert_temp(egui::Id::new("gp_nav_owns_selection"), pass);
                    d.insert_temp(egui::Id::new(("gp_nav_glow", outer.0)), (pass, !matches!(nav, Nav::Widget)));
                    let action = match nav {
                        Nav::Card(n) => {
                            d.insert_temp(egui::Id::new(("gp_nav_remap_card", inner.0, "mappings")), (pass, n, false));
                            usize::MAX
                        }
                        Nav::Action => 0,
                        Nav::Widget => usize::MAX,
                    };
                    d.insert_temp(egui::Id::new(("gp_nav_remap_action", inner.0, "mappings")), (pass, action));
                });
                if !matches!(nav, Nav::Widget) {
                    crate::app::request_card_glow(ctx, outer, inner, "mappings");
                }
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.scope_builder(egui::UiBuilder::new().max_rect(PANEL), |ui| {
                        render_subpatch_body(
                            ui, canvas, DeviceParamDefaults::default(), &[], &[], &HashSet::new(),
                            &HashMap::new(), &PanicShortcut::default(), &HashMap::new(), None,
                        );
                    });
                });
                focus = crate::app::nav_card_focus_rect(ctx, outer);
            });
            let scroll = ctx.data(|d| d.get_temp::<f32>(scroll_id)).unwrap_or(0.0);
            Pass { scroll, focus, shapes: out.shapes }
        }

        /// Run `n` passes; the scroll each ended on.
        fn run(&mut self, nav: Nav, n: usize) -> Vec<f32> {
            (0..n).map(|_| self.pass(nav).scroll).collect()
        }
    }

    fn scale() -> f32 {
        PANEL.width() / BODY_W
    }

    /// The scroll moved one way and then held still: no back-and-forth.
    fn assert_settles(scrolls: &[f32], what: &str) {
        let tail = &scrolls[scrolls.len() - 8..];
        assert!(tail.iter().all(|s| (s - tail[0]).abs() < 0.5), "{what}: still moving {scrolls:?}");
        let steps: Vec<f32> = scrolls.windows(2).map(|w| w[1] - w[0]).filter(|d| d.abs() > 0.5).collect();
        assert!(steps.windows(2).all(|w| w[0].signum() == w[1].signum()),
            "{what}: moved back and forth {scrolls:?}");
    }

    /// The reported bug: selecting a widget taller than the body made the body
    /// flicker up and down, every frame. It comes to rest showing the widget's top.
    #[test]
    fn a_widget_taller_than_the_body_settles_on_its_top() {
        let mut r = Rig::new(12, 700.0);
        let scrolls = r.run(Nav::Widget, 30);
        assert_settles(&scrolls, "selecting the widget");
        let top = PIN_TOP * scale() - 24.0;
        assert!((scrolls[29] - top).abs() < 1.0, "came to rest at {} not the widget's top {top}", scrolls[29]);
    }

    /// Inside it, the body scrolls only as the focused card goes past the panel's
    /// edge, the widget's top going out of view for it, and every card comes to
    /// rest in the panel.
    #[test]
    fn the_body_follows_the_focused_card_and_lets_the_top_go() {
        let mut r = Rig::new(12, 700.0);
        let rest = *r.run(Nav::Widget, 20).last().unwrap();
        // The first card is in view already: entering the list moves nothing.
        let scrolls = r.run(Nav::Card(0), 15);
        assert!(scrolls.iter().all(|s| (s - rest).abs() < 0.5), "the first card moved the body: {scrolls:?}");
        // Down the list, a card at a time.
        for n in 1..12 {
            let scrolls = r.run(Nav::Card(n), 15);
            assert_settles(&scrolls, &format!("card {n}"));
            let card = r.pass(Nav::Card(n)).focus.expect("the focused card's rect");
            assert!(PANEL.expand(1.0).contains_rect(card), "card {n} at {card:?}, outside the panel {PANEL:?}");
        }
        assert!(r.pass(Nav::Card(11)).scroll > PIN_TOP * scale(), "the widget's top is still in view");
        // And back up to the action row at its top.
        let scrolls = r.run(Nav::Action, 15);
        assert_settles(&scrolls, "back to Learn");
        let learn = r.pass(Nav::Action).focus.expect("the Learn button's rect");
        assert!(PANEL.expand(1.0).contains_rect(learn), "Learn at {learn:?}, outside the panel");
    }

    /// Going up onto the top widget scrolls the body all the way up, showing
    /// the title over it — unless the widget wouldn't fit whole under it, when
    /// it is the widget's own top that shows.
    #[test]
    fn up_onto_the_top_widget_shows_the_top_of_the_layout() {
        // A short top widget, a second one far below it.
        let mut r = Rig::with_pins(3, &[(60.0, 100.0), (400.0, 100.0)], 2);
        let below = *r.run(Nav::Widget, 10).last().unwrap();
        assert!(below > 60.0 * scale(), "the lower widget is scrolled to: {below}");
        r.select(1);
        let scrolls = r.run(Nav::Widget, 10);
        assert_settles(&scrolls, "up onto the top widget");
        assert_eq!(scrolls[9], 0.0, "the layout's top isn't in view");
        // The wheel is free once it's there: nav doesn't pull it back up.
        let scroll_id = r.scroll_id();
        r.ctx.data_mut(|d| d.insert_temp(scroll_id, 30.0_f32));
        assert_eq!(r.pass(Nav::Widget).scroll, 30.0, "the top widget is still in view");

        // A top widget too tall to fit whole under the title: it is the widget
        // that shows, whole, not the title.
        let mut r = Rig::with_pins(3, &[(60.0, 260.0), (400.0, 100.0)], 2);
        r.run(Nav::Widget, 10);
        r.select(1);
        let scrolls = r.run(Nav::Widget, 10);
        assert_settles(&scrolls, "up onto a tall top widget");
        let (rest, top, bot) = (scrolls[9], 60.0 * scale(), 320.0 * scale());
        assert!(rest > 0.0 && rest <= top && rest + PANEL.height() >= bot,
            "came to rest at {rest}, not showing the widget ({top}..{bot}) whole");
    }

    /// Once the mouse or a trackpad is used, the pad's selection stops pulling the body back
    /// to it and its highlight fades - the wheel used to be dragged back every frame - until
    /// the pad picks or moves something again.
    #[test]
    fn the_pointer_has_the_layout_until_the_pad_moves_again() {
        let mut r = Rig::with_pins(3, &[(60.0, 100.0), (400.0, 100.0)], 2);
        let followed = *r.run(Nav::Widget, 10).last().unwrap();
        assert!(followed > 60.0 * scale(), "the pad's selection is scrolled to: {followed}");
        // The wheel scrolls back to the top, the pointer having been used.
        crate::widgets::note_pointer_activity(&r.ctx);
        let scroll_id = r.scroll_id();
        r.ctx.data_mut(|d| d.insert_temp(scroll_id, 0.0_f32));
        let scrolls = r.run(Nav::Widget, 20);
        assert!(scrolls.iter().all(|s| *s == 0.0), "the body stays where the wheel put it: {scrolls:?}");
        assert!(crate::widgets::nav_highlight_alpha(&r.ctx) < 0.05, "the pad's highlight faded");
        // The pad moves: its selection is followed again, its highlight back.
        crate::widgets::note_pad_activity(&r.ctx);
        let back = *r.run(Nav::Widget, 20).last().unwrap();
        assert!((back - followed).abs() < 1.0, "followed again: {back} vs {followed}");
        assert!(crate::widgets::nav_highlight_alpha(&r.ctx) > 0.95, "the highlight is back");
    }

    /// A Two-Way curve's rows pinned on the layout answer a click in Easy mode, for
    /// the plain curve and the AutoMap one, in either pin style: its lane toggle,
    /// clicked, sets the lane it edits.
    #[test]
    fn a_two_way_curves_pinned_lane_toggle_answers_a_click() {
        for module in ["module.twoway_response_curve", "module.automap_twoway_response_curve"] {
            for style in [1u8, 2] {
                let desc = flexinput_modules::all_modules()
                    .into_iter()
                    .map(|r| r.descriptor)
                    .find(|d| d.id == module)
                    .expect("registered");
                let pins = [
                    // All in the panel's view (300 high, the body at 1.25x).
                    ("curve", [8.0, 8.0], [296.0, 60.0]),
                    ("scale_row", [8.0, 72.0], [296.0, 20.0]),
                    ("lane_toggle", [8.0, 100.0], [140.0, 18.0]),
                    ("range_row", [152.0, 100.0], [152.0, 18.0]),
                    ("hyst_row", [8.0, 124.0], [224.0, 20.7]),
                    ("grid_row", [8.0, 148.0], [296.0, 20.0]),
                    ("interp_row", [8.0, 172.0], [296.0, 18.0]),
                ];
                let items: Vec<serde_json::Value> = pins
                    .iter()
                    .map(|(e, p, s)| serde_json::json!({ "Module": {
                        "inner_node_id": 0, "element_id": e, "pos": p, "size": s, "pin_style": style } }))
                    .collect();
                let mut sp: UiSubPatch = serde_json::from_value(serde_json::json!({
                    "display_name": "Body", "pins_in": [], "pins_out": [], "items": items,
                })).expect("sub-patch json");
                let inner = sp.snarl.insert_node(pos2(0.0, 0.0), NodeData::from(&desc));
                assert_eq!(inner.0, 0);
                let mut canvas = Canvas::default();
                let outer = canvas.snarl.insert_node(pos2(0.0, 0.0), NodeData {
                    module_id: "subpatch".into(), display_name: String::new(), category: String::new(),
                    inputs: vec![], outputs: vec![], params: Default::default(),
                    subpatch: Some(Box::new(sp)), extra: Default::default(),
                });
                let ctx = egui::Context::default();
                let mut t = 0.0;
                let mut pass = |canvas: &mut Canvas, events: Vec<egui::Event>| {
                    t += 1.0 / 60.0;
                    let raw = egui::RawInput { screen_rect: Some(SCREEN), time: Some(t), events, ..Default::default() };
                    ctx.run(raw, |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            ui.scope_builder(egui::UiBuilder::new().max_rect(PANEL), |ui| {
                                render_subpatch_body(
                                    ui, canvas, DeviceParamDefaults::default(), &[], &[], &HashSet::new(),
                                    &HashMap::new(), &PanicShortcut::default(), &HashMap::new(), None,
                                );
                            });
                        });
                    })
                };
                let mut last = None;
                for _ in 0..6 {
                    last = Some(pass(&mut canvas, vec![]));
                }
                // Where the Down cell is, on screen.
                let at = if style == 2 {
                    let r = ctx
                        .read_response(egui::Id::new(("segmented", ("ps2_field", inner.0, "lane_toggle", 0usize), 1usize)))
                        .unwrap_or_else(|| panic!("{module} style {style}: no lane toggle"));
                    ctx.layer_transform_to_global(r.layer_id).unwrap_or_default() * r.rect.center()
                } else {
                    last.as_ref().and_then(|o| o.shapes.iter().find_map(|s| match &s.shape {
                        egui::Shape::Text(t) if t.galley.text() == "↓ Down" => Some(t.visual_bounding_rect().center()),
                        _ => None,
                    })).unwrap_or_else(|| panic!("{module} style {style}: no ↓ Down"))
                };
                let button = |pressed| egui::Event::PointerButton {
                    pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default(),
                };
                pass(&mut canvas, vec![egui::Event::PointerMoved(at)]);
                pass(&mut canvas, vec![button(true)]);
                pass(&mut canvas, vec![button(false)]);
                pass(&mut canvas, vec![]);
                let lane = canvas.snarl.get_node(outer)
                    .and_then(|n| n.subpatch.as_ref())
                    .and_then(|sp| sp.snarl.get_node(inner))
                    .and_then(|n| n.params.get("active_lane").and_then(|v| v.as_str()).map(str::to_string));
                assert_eq!(lane.as_deref(), Some("dn"), "{module} style {style}: the click at {at:?} didn't set the lane");
            }
        }
    }

    /// Nothing the body or nav paints for it lands outside the panel: not the
    /// cards, not their glow, not the glow of an action row scrolled out of view.
    #[test]
    fn nothing_paints_outside_the_panel() {
        // At the last card: the list and the body both scrolled down.
        let mut r = Rig::new(12, 700.0);
        r.run(Nav::Card(11), 30);
        let last_card = r.pass(Nav::Card(11));
        // On Learn while it sits on screen just above the panel, over the preset
        // bar: a body held 64 px past the widget's top (enough to clear the action
        // row's 26-point buttons), as a pass in which nav has only just asked for it
        // (no focus yet for the auto-scroll to chase).
        let mut r = Rig::new(12, 700.0);
        r.run(Nav::Widget, 10);
        let scroll_id = r.scroll_id();
        r.ctx.data_mut(|d| d.insert_temp(scroll_id, PIN_TOP * scale() + 64.0));
        let learn = r.pass(Nav::Action);
        let above = learn.focus.expect("Learn's rect");
        assert!(above.bottom() < PANEL.top() && above.top() > SCREEN.top(),
            "Learn at {above:?} isn't on screen above the panel");
        for (what, p) in [("at the last card", &last_card), ("on Learn out of view", &learn)] {
            for s in &p.shapes {
                let vis = s.shape.visual_bounding_rect().intersect(s.clip_rect);
                // The central panel's own fill spans the screen.
                if !vis.is_positive() || vis.width() >= SCREEN.width() - 1.0 { continue; }
                assert!(PANEL.expand(1.0).contains_rect(vis),
                    "{what}: a shape paints at {vis:?}, outside the panel {PANEL:?}");
            }
        }
    }
}

#[cfg(test)]
mod layout_editor_shots {
    use super::*;
    use crate::canvas::node::LayoutItem;

    /// An Easy layout of `items`, one selected, laid out by Easy mode's center panel at
    /// `screen`: the canvas, and a frame to run with pointer events, which hands back the
    /// frame's shapes.
    struct EasyScene {
        ctx: egui::Context,
        canvas: Canvas,
        oid: egui_snarl::NodeId,
        easy_state: EasyState,
        overlay: crate::canvas::OverlayLayout,
        config: crate::canvas::OverlayLayout,
        nav: crate::gamepad_nav::GamepadNav,
        favorites: Vec<std::path::PathBuf>,
        screen: egui::Rect,
        t: u32,
        shot: crate::canvas::shot::Shot,
    }

    impl EasyScene {
        fn new(items: Vec<LayoutItem>, selected: usize, editing: bool, screen: egui::Vec2) -> Self {
            let desc = flexinput_modules::all_modules()
                .into_iter()
                .map(|r| r.descriptor)
                .find(|d| d.id == "subpatch")
                .expect("the sub-patch module");
            let ctx = egui::Context::default();
            crate::app::fonts::setup_fonts(&ctx);
            let mut canvas = Canvas::new();
            let oid = canvas.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&desc));
            if let Some(n) = canvas.snarl.get_node_mut(oid) {
                let mut sp = UiSubPatch::default();
                sp.display_name = "Shooter".into();
                sp.items = items;
                sp.selected_item = Some(selected);
                sp.selected_items = vec![selected];
                n.subpatch = Some(Box::new(sp));
                n.extra.layout_unlocked = editing;
            }
            Self {
                ctx,
                canvas,
                oid,
                easy_state: EasyState::default(),
                overlay: Default::default(),
                config: Default::default(),
                nav: Default::default(),
                favorites: Vec::new(),
                screen: egui::Rect::from_min_size(egui::Pos2::ZERO, screen),
                t: 0,
                shot: Default::default(),
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<egui::epaint::ClippedShape> {
            self.frame_held(events, egui::Modifiers::NONE)
        }

        /// A frame with `modifiers` held.
        fn frame_held(&mut self, events: Vec<egui::Event>, modifiers: egui::Modifiers) -> Vec<egui::epaint::ClippedShape> {
            self.t += 1;
            let raw = egui::RawInput { screen_rect: Some(self.screen), time: Some(self.t as f64 / 30.0), events, modifiers, ..Default::default() };
            let Self { ctx, canvas, easy_state, overlay, config, nav, favorites, shot, .. } = self;
            let out = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui, canvas, easy_state, overlay, config, None,
                        Default::default(), &[], &[], &Default::default(), &Default::default(),
                        &Default::default(), &Default::default(), favorites, nav,
                    );
                });
            });
            shot.absorb(&out.textures_delta);
            out.shapes
        }

        fn items(&self) -> &[LayoutItem] {
            &self.canvas.snarl.get_node(self.oid).and_then(|n| n.subpatch.as_ref()).expect("the sub-patch").items
        }

        fn selected(&self) -> Option<usize> {
            self.canvas.snarl.get_node(self.oid).and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.selected_item)
        }
    }

    /// The parts a whole-module widget of node `inner` published last pass on the Easy body,
    /// as `(nav pass, [(element, rect on the screen)])`.
    fn widget_parts(ctx: &egui::Context, inner: usize) -> Option<(u64, Vec<(String, egui::Rect)>)> {
        let parts: Option<(u64, Vec<crate::canvas::viewer::ModulePart>)> =
            ctx.data(|d| d.get_temp(crate::canvas::viewer::module_parts_id(inner, egui::ViewportId::ROOT)));
        parts.map(|(p, v)| (p, v.into_iter().map(|m| (m.element, m.rect)).collect()))
    }

    /// Every painted circle in `shapes`: its centre, radius and fill.
    fn painted_circles(shapes: &[egui::epaint::ClippedShape]) -> Vec<(egui::Pos2, f32, egui::Color32)> {
        fn walk(s: &egui::Shape, out: &mut Vec<(egui::Pos2, f32, egui::Color32)>) {
            match s {
                egui::Shape::Circle(c) => out.push((c.center, c.radius, c.fill)),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|c| walk(&c.shape, &mut out));
        out
    }

    /// A rectangle, `fill`, at `pos` `size` big - with `gradient` for its fill if one is given.
    fn rect_item(pos: [f32; 2], size: [f32; 2], fill: [u8; 4], gradient: Option<crate::canvas::node::Gradient>) -> LayoutItem {
        LayoutItem::Deco(crate::canvas::node::LayoutDecoration::Rect {
            tab: 0,
            pos, size, fill, fill_gradient: gradient, stroke: [0; 4], stroke_px: 0.0, corner_radius: 0.0,
            anchor: Default::default(),
        })
    }

    /// The selected object's gradient handles work where another object lies over them: they
    /// go on after every object, so nothing drawn above takes the pointer from them, and a
    /// press on one is the handle's, not a click selecting what's on top.
    #[test]
    fn gradient_handles_work_under_another_object() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use crate::canvas::node::{Gradient, GradientKind};
        let pink = [255, 80, 160, 255];
        let below = rect_item([16.0, 16.0], [220.0, 120.0], [0; 4], Some(Gradient::new(GradientKind::Linear, [40, 120, 255, 255], pink)));
        // Over the gradient's end, which sits on the lower one's right edge.
        let above = rect_item([150.0, 16.0], [200.0, 120.0], [90, 90, 90, 255], None);
        let mut scene = EasyScene::new(vec![below, above], 0, true, egui::vec2(1100.0, 500.0));
        let mut shapes = Vec::new();
        for _ in 0..6 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(egui::pos2(1000.0, 450.0))]);
        }
        let pink_c = egui::Color32::from_rgba_unmultiplied(pink[0], pink[1], pink[2], pink[3]);
        let (end, _, _) = painted_circles(&shapes)
            .into_iter()
            .find(|(_, r, f)| *r > 3.0 && *f == pink_c)
            .expect("the gradient's end handle is drawn");
        let button = |pressed| egui::Event::PointerButton {
            pos: end, button: egui::PointerButton::Primary, pressed, modifiers: Default::default(),
        };
        scene.frame(vec![egui::Event::PointerMoved(end)]);
        scene.frame(vec![button(true)]);
        for k in 1..=6 {
            scene.frame(vec![egui::Event::PointerMoved(end - egui::vec2(10.0 * k as f32, 0.0))]);
        }
        scene.frame(vec![egui::Event::PointerButton {
            pos: end - egui::vec2(60.0, 0.0), button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default(),
        }]);
        scene.frame(vec![]);
        let Some(LayoutItem::Deco(crate::canvas::node::LayoutDecoration::Rect { fill_gradient: Some(g), .. })) = scene.items().first() else {
            panic!("the lower rectangle keeps its gradient");
        };
        assert!(g.end[0] < 0.9, "the end was dragged in: {:?}", g.end);
        assert_eq!(scene.selected(), Some(0), "the drag didn't select the rectangle on top");
    }

    /// The snap grid's lines in `shapes`: faint lines in the grid's blue (their width is the
    /// layout's scale, so not that).
    fn grid_lines(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn grid_blue(c: egui::Color32) -> bool {
            let [r, g, b, a] = c.to_srgba_unmultiplied();
            a > 0 && a <= 40 && r.abs_diff(150) <= 4 && g.abs_diff(200) <= 4 && b.abs_diff(255) <= 4
        }
        fn walk(s: &egui::Shape, n: &mut usize) {
            match s {
                egui::Shape::LineSegment { stroke, .. } if grid_blue(stroke.color) => *n += 1,
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, n)),
                _ => {}
            }
        }
        let mut n = 0;
        shapes.iter().for_each(|c| walk(&c.shape, &mut n));
        n
    }

    /// The four sides of the selection's outline in `shapes`, as the rect they go round.
    fn selection_outline(shapes: &[egui::epaint::ClippedShape]) -> Option<egui::Rect> {
        let sel = egui::Color32::from_rgba_unmultiplied(255, 220, 120, 230);
        fn walk(s: &egui::Shape, sel: egui::Color32, out: &mut Vec<egui::Pos2>) {
            match s {
                egui::Shape::LineSegment { points, stroke } if stroke.color == sel => out.extend(points),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, sel, out)),
                _ => {}
            }
        }
        let mut pts = Vec::new();
        shapes.iter().for_each(|c| walk(&c.shape, sel, &mut pts));
        (!pts.is_empty()).then(|| egui::Rect::from_points(&pts))
    }

    /// The snap grid shows only while an object is moved: not while the layout is just open
    /// for editing, and it goes again once the object is let go.
    #[test]
    fn the_snap_grid_shows_only_while_an_object_moves() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let items = vec![rect_item([16.0, 16.0], [220.0, 120.0], [90, 90, 90, 255], None)];
        let mut scene = EasyScene::new(items, 0, true, egui::vec2(1100.0, 500.0));
        if let Some(sp) = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()) {
            sp.snap_enabled = true;
        }
        let away = egui::pos2(1000.0, 480.0);
        let mut shapes = Vec::new();
        for _ in 0..8 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        assert_eq!(grid_lines(&shapes), 0, "no grid while nothing moves");
        let outline = selection_outline(&shapes).expect("the selection's outline is drawn");
        let grab = outline.min + egui::vec2(40.0, 40.0);
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        scene.frame(vec![egui::Event::PointerMoved(grab)]);
        scene.frame(vec![button(grab, true)]);
        let mut during = 0;
        for k in 1..=10 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(grab + egui::vec2(4.0 * k as f32, 0.0))]);
            during = during.max(grid_lines(&shapes));
        }
        assert!(during > 10, "the grid shows while the object moves ({during} lines)");
        let end = grab + egui::vec2(40.0, 0.0);
        scene.frame(vec![button(end, false)]);
        for _ in 0..12 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        assert_eq!(grid_lines(&shapes), 0, "the grid goes once the object is let go");
    }

    /// Editing a layout, a held modifier says what it does there, as the canvas does for its
    /// own: Ctrl's move and resize catches, Alt's resize about the centre; nothing unheld.
    #[test]
    fn a_held_modifier_says_what_it_does_in_the_layout_editor() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let items = vec![rect_item([16.0, 16.0], [220.0, 120.0], [90, 90, 90, 255], None)];
        let mut scene = EasyScene::new(items, 0, true, egui::vec2(1100.0, 500.0));
        let says = |shapes: &[egui::epaint::ClippedShape], words: &str| painted_texts(shapes).iter().any(|(t, _, _)| t.starts_with(words));
        let shapes = scene.frame_held(vec![], egui::Modifiers::NONE);
        assert!(!says(&shapes, "Ctrl+Resize") && !says(&shapes, "Alt+Resize"), "nothing while nothing's held");
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = scene.frame_held(vec![], egui::Modifiers::CTRL);
        }
        assert!(says(&shapes, "Ctrl+Resize") && says(&shapes, "Ctrl+Drag"), "Ctrl's catches");
        for _ in 0..2 {
            shapes = scene.frame_held(vec![], egui::Modifiers::ALT);
        }
        assert!(says(&shapes, "Alt+Resize"), "Alt's resize about the centre");
    }

    /// The pointer on the grid's pill shows the grid, so a change to its size can be seen -
    /// and doesn't hang the app (the pill asked the context for the pass while holding it).
    #[test]
    fn the_grid_shows_while_the_pointer_is_on_its_pill() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let items = vec![rect_item([16.0, 16.0], [220.0, 120.0], [90, 90, 90, 255], None)];
        let mut scene = EasyScene::new(items, 0, true, egui::vec2(1100.0, 500.0));
        if let Some(sp) = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()) {
            sp.snap_enabled = true;
        }
        let away = egui::pos2(1000.0, 480.0);
        let mut shapes = Vec::new();
        for _ in 0..6 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        assert_eq!(grid_lines(&shapes), 0, "no grid to start with");
        let pill = painted_texts(&shapes).into_iter().find(|(t, _, _)| t == "grid").map(|(_, r, _)| r).expect("the grid's pill");
        let mut most = 0;
        for _ in 0..10 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(pill.center())]);
            most = most.max(grid_lines(&shapes));
        }
        assert!(most > 10, "the grid shows under the pointer on its pill ({most} lines)");
    }

    /// A layout with two tabs or more has their row across the top of the panel and is laid
    /// out beneath it: the objects of the tab showing show, a click on another tab shows that
    /// one, and while the layout is edited the row's + adds a tab and a double click renames
    /// one.
    #[test]
    fn a_layout_shows_one_tab_at_a_time_under_its_row_of_tabs() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let red = [220, 40, 40, 255];
        let green = [40, 200, 60, 255];
        let mut first = rect_item([16.0, 16.0], [100.0, 60.0], red, None);
        first.set_tab(Some(0));
        let mut second = rect_item([140.0, 16.0], [100.0, 60.0], green, None);
        second.set_tab(Some(1));
        let mut scene = EasyScene::new(vec![first, second], 0, false, egui::vec2(1100.0, 500.0));
        if let Some(sp) = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()) {
            sp.layout_tabs = vec!["One".into(), "Two".into()];
        }
        let colour = |c: [u8; 4]| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
        let rect_of = |shapes: &[egui::epaint::ClippedShape], c: [u8; 4]| {
            painted_rects(shapes).into_iter().find(|(_, f)| *f == colour(c)).map(|(r, _)| r)
        };
        // The lowest of the words: the row's, beneath the header (whose tab picker, editing,
        // names the tab showing too).
        let text_of = |shapes: &[egui::epaint::ClippedShape], t: &str| {
            painted_texts(shapes).into_iter().filter(|(s, _, _)| s == t).map(|(_, r, _)| r)
                .max_by(|a, b| a.center().y.total_cmp(&b.center().y))
        };
        let away = egui::pos2(1000.0, 480.0);
        let mut shapes = Vec::new();
        for _ in 0..6 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        let shown = rect_of(&shapes, red).expect("the first tab's object shows");
        assert!(rect_of(&shapes, green).is_none(), "the second tab's doesn't");
        let one = text_of(&shapes, "One").expect("the row names the first tab");
        let two = text_of(&shapes, "Two").expect("and the second");
        assert!((one.center().y - two.center().y).abs() < 1.0 && two.left() > one.right(), "side by side, in a row");
        assert!(shown.top() >= one.bottom(), "the layout is beneath the row: {shown:?} under {one:?}");
        let click = |scene: &mut EasyScene, at: egui::Pos2| {
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
            scene.frame(vec![egui::Event::PointerMoved(at)]);
            scene.frame(vec![button(true)]);
            scene.frame(vec![button(false)]);
        };
        click(&mut scene, two.center());
        for _ in 0..3 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        let sp = |scene: &EasyScene| scene.canvas.snarl.get_node(scene.oid).and_then(|n| n.subpatch.as_ref()).map(|sp| (sp.active_tab, sp.layout_tabs.clone()));
        assert_eq!(sp(&scene).map(|s| s.0), Some(1), "the click showed the second tab");
        assert!(rect_of(&shapes, green).is_some(), "the second tab's object shows");
        assert!(rect_of(&shapes, red).is_none(), "the first tab's doesn't");

        // Editing: the + at the row's end adds a tab, which shows.
        if let Some(n) = scene.canvas.snarl.get_node_mut(scene.oid) {
            n.extra.layout_unlocked = true;
        }
        for _ in 0..3 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        let two = text_of(&shapes, "Two").expect("the second tab");
        // The tab showing is lit, and has its × now; the + is just past it, the last tab.
        let lit = scene.ctx.style().visuals.selection.bg_fill;
        let cell = painted_rects(&shapes).into_iter()
            .find(|(r, f)| *f == lit && r.contains(two.center()))
            .map(|(r, _)| r)
            .expect("the second tab's cell, lit");
        assert!(cell.right() > two.right() + 14.0, "room for its ×: {cell:?} past {two:?}");
        click(&mut scene, egui::pos2(cell.right() + 2.0 + (crate::canvas::viewer::TAB_ROW_H - 4.0) * 0.5, cell.center().y));
        assert_eq!(sp(&scene).map(|s| s.1.len()), Some(3), "the row's + added a tab: {:?}", sp(&scene));
        assert_eq!(sp(&scene).map(|s| s.0), Some(2), "and it shows");
        // A double click renames a tab in place (well after the + click, or egui counts three).
        for _ in 0..30 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        let one = text_of(&shapes, "One").expect("the first tab");
        let at = one.center();
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        scene.frame(vec![egui::Event::PointerMoved(at)]);
        scene.frame(vec![button(true)]);
        scene.frame(vec![button(false)]);
        scene.frame(vec![button(true)]);
        scene.frame(vec![button(false)]);
        scene.frame(vec![]);
        let select_all = egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND };
        scene.frame(vec![select_all]);
        scene.frame(vec![egui::Event::Text("Aim".into())]);
        let enter = egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
        scene.frame(vec![enter]);
        scene.frame(vec![egui::Event::PointerMoved(away)]);
        assert_eq!(sp(&scene).map(|s| s.1[0].clone()), Some("Aim".to_string()), "renamed: {:?}", sp(&scene));
    }

    /// A module pinned whole is drawn as a widget of its own size - its body laid out at
    /// that size - and offers its parts (the graph, the option rows) for the pad to walk.
    #[test]
    fn a_module_pinned_whole_draws_its_body_and_offers_its_parts() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(1100.0, 600.0));
        let curve_desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == "module.response_curve")
            .expect("the curve module");
        let inner = {
            let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
            let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&curve_desc));
            sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                "inner_node_id": inner.0, "element_id": "whole_module", "pos": [16.0, 16.0], "size": [320.0, 300.0],
            })).expect("a pin")));
            sp.selected_item = None;
            sp.selected_items.clear();
            inner
        };
        for _ in 0..6 {
            scene.frame(vec![egui::Event::PointerMoved(egui::pos2(1000.0, 580.0))]);
        }
        let parts: Option<(u64, Vec<(String, egui::Rect)>)> = widget_parts(&scene.ctx, inner.0);
        let (_, parts) = parts.expect("the widget published its parts");
        let names: Vec<&str> = parts.iter().map(|(e, _)| e.as_str()).collect();
        assert!(names.contains(&"curve"), "the graph is a part: {names:?}");
        assert!(names.len() >= 2, "and the option rows: {names:?}");
        // All inside the widget's box, which is the size it was given - on the screen, where
        // Easy mode scales the layout to its panel (here as far as it goes, 1.5x).
        let union = parts.iter().fold(egui::Rect::NOTHING, |u, (_, r)| u.union(*r));
        assert!(union.width() <= 1.5 * 330.0 && union.height() <= 1.5 * 310.0, "laid out within the widget: {union:?}");
        // A curve's rows aren't drawn a control per field the pad walks (its range row walks
        // four, and draws three): no field rects are made up for them.
        let node = scene.canvas.snarl.get_node(scene.oid).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner).cloned());
        for (part, _) in &parts {
            let want = crate::app::element_fields("module.response_curve", part, node.as_ref()).len();
            let fields: Option<(u64, Vec<egui::Rect>)> =
                scene.ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_field_rects", inner.0, part.clone()))));
            if let Some((_, fields)) = fields {
                assert_eq!(fields.len(), want, "{part}: a rect per field, or none");
            }
        }
    }

    /// A module pinned whole and zoomed out is laid out bigger and drawn smaller: its rows
    /// come out shorter and its graph takes the room they leave, all still inside the widget.
    #[test]
    fn a_zoomed_out_module_widget_draws_its_rows_smaller_and_its_graph_larger() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let parts_at = |zoom: Option<f32>| -> Vec<(String, egui::Rect)> {
            let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(1100.0, 600.0));
            let curve_desc = flexinput_modules::all_modules()
                .into_iter()
                .map(|r| r.descriptor)
                .find(|d| d.id == "module.response_curve")
                .expect("the curve module");
            let inner = {
                let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
                let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&curve_desc));
                sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                    "inner_node_id": inner.0, "element_id": "whole_module", "pos": [16.0, 16.0], "size": [320.0, 300.0],
                    "zoom": zoom,
                })).expect("a pin")));
                sp.selected_item = None;
                sp.selected_items.clear();
                inner
            };
            for _ in 0..8 {
                scene.frame(vec![egui::Event::PointerMoved(egui::pos2(1000.0, 580.0))]);
            }
            let parts: Option<(u64, Vec<(String, egui::Rect)>)> = widget_parts(&scene.ctx, inner.0);
            parts.expect("the widget published its parts").1
        };
        let rect_of = |parts: &[(String, egui::Rect)], e: &str| parts.iter().find(|(p, _)| p == e).map(|(_, r)| *r).unwrap_or_else(|| panic!("{e}: {parts:?}"));
        let whole = parts_at(None);
        let half = parts_at(Some(0.5));
        let (graph1, graph2) = (rect_of(&whole, "curve"), rect_of(&half, "curve"));
        let (row1, row2) = (rect_of(&whole, "range_row"), rect_of(&half, "range_row"));
        assert!(graph2.height() > graph1.height() * 1.05, "the graph takes more room: {graph1:?} -> {graph2:?}");
        assert!((row2.height() - row1.height() * 0.5).abs() < row1.height() * 0.15, "a row comes out half as tall: {row1:?} -> {row2:?}");
        let outer = whole.iter().fold(egui::Rect::NOTHING, |u, (_, r)| u.union(*r));
        let zoomed = half.iter().fold(egui::Rect::NOTHING, |u, (_, r)| u.union(*r));
        assert!(outer.expand(2.0).contains(zoomed.min) && zoomed.width() <= outer.width() + 2.0, "still inside the widget: {zoomed:?} in {outer:?}");
    }

    /// Editing the layout, a zoomed widget is an object like any other: a right-click on it
    /// opens its menu (its own layer took the pointer from the layout's handles).
    #[test]
    fn a_zoomed_widget_keeps_its_menu_while_the_layout_is_edited() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let curve_desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == "module.response_curve")
            .expect("the curve module");
        let mut scene = EasyScene::new(Vec::new(), 0, true, egui::vec2(1100.0, 600.0));
        {
            let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
            let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&curve_desc));
            sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                "inner_node_id": inner.0, "element_id": "whole_module", "pos": [16.0, 16.0], "size": [320.0, 300.0],
                "zoom": 0.6,
            })).expect("a pin")));
            sp.selected_item = Some(0);
            sp.selected_items = vec![0];
        }
        let away = egui::pos2(1000.0, 580.0);
        let mut shapes = Vec::new();
        for _ in 0..6 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        let outline = selection_outline(&shapes).expect("the widget's outline");
        let at = outline.center();
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed, modifiers: Default::default() };
        scene.frame(vec![egui::Event::PointerMoved(at)]);
        scene.frame(vec![button(true)]);
        scene.frame(vec![button(false)]);
        for _ in 0..2 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(at)]);
        }
        assert!(painted_texts(&shapes).iter().any(|(t, _, _)| t == "Send to surface"), "the menu opened");
    }

    /// Editing the layout, a zoomed module's card window - 3DOF to 2D's Lean cards - is
    /// drawn zoomed with the rest, inside the widget, and leaves the pointer to the layout: a
    /// right-click on its Learn button opens the widget's menu. On a layer of its own it came
    /// out full size, past the widget's edge.
    #[test]
    fn a_zoomed_widgets_cards_scale_with_it_while_the_layout_is_edited() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut scene = EasyScene::new(Vec::new(), 0, true, egui::vec2(1300.0, 700.0));
        {
            let desc = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor)
                .find(|d| d.id == "processing.gyro_3dof").expect("3DOF");
            let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
            let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&desc));
            sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                "inner_node_id": inner.0, "element_id": "whole_module", "pos": [0.0, 0.0], "size": [988.0, 592.0],
                "zoom": 0.74,
            })).expect("a pin")));
            sp.selected_item = Some(0);
            sp.selected_items = vec![0];
        }
        let away = egui::pos2(1290.0, 690.0);
        let mut shapes = Vec::new();
        for _ in 0..8 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        let outline = selection_outline(&shapes).expect("the widget's outline");
        let learn: Vec<egui::Rect> = painted_texts(&shapes).into_iter().filter(|(t, _, _)| t == "Learn").map(|(_, r, _)| r).collect();
        assert!(!learn.is_empty(), "the Lean cards' Learn buttons are drawn");
        let tune: Vec<egui::Rect> = painted_texts(&shapes).into_iter().filter(|(t, _, _)| t == "lean threshold").map(|(_, r, _)| r).collect();
        let tune = tune.first().copied().expect("the lean threshold pill");
        for r in &learn {
            assert!(outline.expand(1.0).contains_rect(*r), "inside the widget {outline:?}: {r:?}");
            assert!(r.height() < tune.height() * 1.25, "zoomed as the rest is: {r:?} beside {tune:?}");
        }
        let at = learn[0].center();
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed, modifiers: Default::default() };
        scene.frame(vec![egui::Event::PointerMoved(at)]);
        scene.frame(vec![button(true)]);
        scene.frame(vec![button(false)]);
        for _ in 0..2 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(at)]);
        }
        assert!(painted_texts(&shapes).iter().any(|(t, _, _)| t == "Send to surface"), "the menu opened");
    }

    /// A zoomed widget answers the pointer where it's drawn: dragging its trail pill, drawn
    /// smaller, changes the trail.
    #[test]
    fn a_zoomed_module_widget_answers_the_pointer_where_it_is_drawn() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(1100.0, 600.0));
        let curve_desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == "module.response_curve")
            .expect("the curve module");
        let inner = {
            let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
            let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&curve_desc));
            sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                "inner_node_id": inner.0, "element_id": "whole_module", "pos": [16.0, 16.0], "size": [320.0, 300.0],
                "zoom": 0.6,
            })).expect("a pin")));
            sp.selected_item = None;
            sp.selected_items.clear();
            inner
        };
        let away = egui::pos2(1000.0, 580.0);
        let mut shapes = Vec::new();
        for _ in 0..8 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        let pill = painted_texts(&shapes).into_iter().find(|(t, _, _)| t.ends_with(" ms")).map(|(_, r, _)| r).expect("the trail pill");
        let trail = |scene: &EasyScene| scene.canvas.snarl.get_node(scene.oid).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner)).and_then(|n| n.params.get("trail_ms").and_then(|v| v.as_i64()));
        let before = trail(&scene);
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        let at = pill.center();
        scene.frame(vec![egui::Event::PointerMoved(at)]);
        scene.frame(vec![button(at, true)]);
        for k in 1..=10 {
            scene.frame(vec![egui::Event::PointerMoved(at + egui::vec2(6.0 * k as f32, 0.0))]);
        }
        scene.frame(vec![button(at + egui::vec2(60.0, 0.0), false)]);
        assert_ne!(trail(&scene), before, "the drag on the pill changed the trail");
    }

    /// The pad walks a module pinned whole stop by stop - its graph whole, each of its rows'
    /// fields on its own - with nothing to go into first: the curve's Log/Exp, Abs and Snap
    /// are three stops, each inside its row.
    #[test]
    fn a_module_widget_offers_each_field_as_a_stop() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(1100.0, 600.0));
        let curve_desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == "module.automap_response_curve")
            .expect("the AutoMap curve module");
        let inner = {
            let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
            let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&curve_desc));
            sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                "inner_node_id": inner.0, "element_id": "whole_module", "pos": [16.0, 16.0], "size": [320.0, 300.0],
            })).expect("a pin")));
            sp.selected_item = None;
            sp.selected_items.clear();
            inner
        };
        for _ in 0..6 {
            scene.frame(vec![egui::Event::PointerMoved(egui::pos2(1000.0, 580.0))]);
        }
        let mid = scene.canvas.snarl.get_node(scene.oid).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .map(|n| crate::canvas::viewer::curve_ui_module_id(n).to_string())
            .expect("the curve node");
        let stops = crate::app::FlexInputApp::nav_widget_stops(&scene.ctx, &mid, "whole_module", inner.0, egui::ViewportId::ROOT)
            .expect("the widget has stops");
        assert!(stops.iter().any(|(p, f, _)| p == "curve" && f.is_none()), "the graph is one stop: {stops:?}");
        let scale: Vec<_> = stops.iter().filter(|(p, _, _)| p == "scale_row").collect();
        assert_eq!(scale.iter().map(|(_, f, _)| *f).collect::<Vec<_>>(), [Some(0), Some(1), Some(2)], "Log/Exp, Abs and Snap: {stops:?}");
        let (_, parts) = widget_parts(&scene.ctx, inner.0).expect("the parts");
        let row = parts.iter().find(|(e, _)| e == "scale_row").map(|(_, r)| *r).expect("the scale row");
        assert!(scale.iter().all(|(_, _, r)| row.expand(1.0).contains(r.center())), "each inside its row {row:?}: {scale:?}");
    }

    /// The fields of a part of a module pinned whole, where its row doesn't say where they
    /// are, are the controls drawn in it - when there are as many as the pad walks there - so
    /// the pad's field ring has somewhere to go.
    #[test]
    fn a_module_widgets_fields_are_found_where_its_rows_draw_them() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(1100.0, 600.0));
        let desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == "module.delay")
            .expect("the delay module");
        let inner = {
            let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
            let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&desc));
            sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                "inner_node_id": inner.0, "element_id": "whole_module", "pos": [16.0, 16.0], "size": [220.0, 90.0],
            })).expect("a pin")));
            sp.selected_item = None;
            sp.selected_items.clear();
            inner
        };
        for _ in 0..6 {
            scene.frame(vec![egui::Event::PointerMoved(egui::pos2(1000.0, 580.0))]);
        }
        let parts: Option<(u64, Vec<(String, egui::Rect)>)> = widget_parts(&scene.ctx, inner.0);
        let (_, parts) = parts.expect("the widget published its parts");
        let (part, row) = parts.iter().find(|(e, _)| e == "ms").cloned().expect("its delay is a part");
        let want = crate::app::element_fields("module.delay", "ms", None).len();
        let fields: Option<(u64, Vec<egui::Rect>)> =
            scene.ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_field_rects", inner.0, part.clone()))));
        let (_, fields) = fields.expect("the delay's field was found");
        assert_eq!(fields.len(), want, "a rect per field");
        assert!(fields.iter().all(|f| row.expand(1.0).contains(f.center())), "inside its row {row:?}: {fields:?}");
    }

    /// A module node pinned whole on the Easy body, its params set from `params`.
    fn pin_whole(scene: &mut EasyScene, module: &str, params: serde_json::Value, size: [f32; 2]) -> egui_snarl::NodeId {
        let desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == module)
            .unwrap_or_else(|| panic!("{module}"));
        let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
        let mut node = NodeData::from(&desc);
        if let serde_json::Value::Object(m) = params {
            node.params.extend(m);
        }
        let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), node);
        sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
            "inner_node_id": inner.0, "element_id": "whole_module", "pos": [16.0, 16.0], "size": size,
        })).expect("a pin")));
        sp.selected_item = None;
        sp.selected_items.clear();
        inner
    }

    /// A stick's AutoMap curve - drawn by the vector curve body, not the plain one a gyro's
    /// gets - tells the pad where its graph is (for the cursor's adds and deletes) and rings
    /// the dot the pad has selected. It did neither, so only the gyro's curve answered.
    #[test]
    fn a_sticks_curve_shows_the_pads_selected_dot() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(1100.0, 600.0));
        let pin_key = flexinput_engine::eval::AUTOMAP_CURVE_PIN_PARAM;
        let inner = pin_whole(&mut scene, "module.automap_response_curve", serde_json::json!({ pin_key: "left_stick" }), [340.0, 330.0]);
        let mid = scene.canvas.snarl.get_node(scene.oid).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .map(|n| crate::canvas::viewer::curve_ui_module_id(n).to_string())
            .expect("the curve node");
        assert_eq!(mid, "module.vec_response_curve", "a stick's curve is the vector body");
        let accent = scene.ctx.style().visuals.selection.stroke.color;
        let rings = |shapes: &[egui::epaint::ClippedShape]| -> Vec<egui::Pos2> {
            fn walk(s: &egui::Shape, accent: egui::Color32, out: &mut Vec<egui::Pos2>) {
                match s {
                    // The ring round the selected dot: radius 7 at a 2-point stroke, drawn
                    // at the widget's scale.
                    egui::Shape::Circle(c) if c.stroke.color == accent && c.stroke.width > 0.0 && (c.radius / c.stroke.width - 3.5).abs() < 0.05 => out.push(c.center),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, accent, out)),
                    _ => {}
                }
            }
            let mut out = Vec::new();
            shapes.iter().for_each(|c| walk(&c.shape, accent, &mut out));
            out
        };
        let away = egui::pos2(1000.0, 580.0);
        let mut shapes = Vec::new();
        for _ in 0..6 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        assert!(rings(&shapes).is_empty(), "no dot ringed before the pad selects one");
        let geom: Option<(u64, egui::Rect, f32, f32, f32, f32)> =
            scene.ctx.data(|d| d.get_temp(crate::canvas::viewer::nav_curve_geom_id(inner.0, egui::ViewportId::ROOT)));
        let (_, graph, ..) = geom.expect("the graph told the pad where it is");
        let (_, parts) = widget_parts(&scene.ctx, inner.0).expect("the parts");
        let part = parts.iter().find(|(e, _)| e == "curve").map(|(_, r)| *r).expect("the graph part");
        assert!(part.expand(1.0).contains(graph.center()), "where it's drawn: {graph:?} in {part:?}");
        for _ in 0..3 {
            let pass = crate::widgets::nav_pass(&scene.ctx);
            scene.ctx.data_mut(|d| d.insert_temp(egui::Id::new(("gp_nav_curve_sel", inner.0)), (pass, 0usize, false)));
            shapes = scene.frame(vec![egui::Event::PointerMoved(away)]);
        }
        let ringed = rings(&shapes);
        assert_eq!(ringed.len(), 1, "the selected dot is ringed: {ringed:?}");
        assert!(graph.expand(8.0).contains(ringed[0]), "on the graph: {ringed:?} in {graph:?}");
    }

    /// 3DOF to 2D pinned whole: every setting its body draws live is a stop for the pad - the
    /// Orientation row's rotation, drift fix and re-centre too, which weren't - and a row
    /// that's all greyed out (the Orientation row on the pad's own pose, the steering options
    /// while it points) is passed by. Each section's heading is a stop too, so a folded one
    /// can be opened.
    #[test]
    fn a_3dof_widget_offers_each_of_its_settings_as_a_stop() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let stops_with = |params: serde_json::Value| {
            let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(1100.0, 800.0));
            let inner = pin_whole(&mut scene, "processing.gyro_3dof", params, [520.0, 760.0]);
            for _ in 0..6 {
                scene.frame(vec![egui::Event::PointerMoved(egui::pos2(1090.0, 790.0))]);
            }
            let stops = crate::app::FlexInputApp::nav_widget_stops(&scene.ctx, "processing.gyro_3dof", "whole_module", inner.0, egui::ViewportId::ROOT)
                .expect("the widget has stops");
            let (_, parts) = widget_parts(&scene.ctx, inner.0).expect("the parts");
            for (p, f, r) in &stops {
                if f.is_some() {
                    let row = parts.iter().find(|(e, _)| e == p).map(|(_, r)| *r).unwrap_or_else(|| panic!("{p}: {parts:?}"));
                    assert!(row.expand(1.0).contains(r.center()), "{p} {f:?} inside its row {row:?}: {r:?}");
                }
            }
            stops.into_iter().map(|(p, f, _)| (p, f)).collect::<Vec<_>>()
        };
        let fields_of = |stops: &[(String, Option<usize>)], part: &str| -> Vec<Option<usize>> {
            stops.iter().filter(|(p, _)| p == part).map(|(_, f)| *f).collect()
        };
        // Folded, as the Orientation and Invert sections start: their headings are stops, so
        // the pad can open them - and nothing in them is.
        let stops = stops_with(serde_json::json!({}));
        for heading in ["fold:mode", "fold:orient", "fold:axes", "fold:lean"] {
            assert_eq!(fields_of(&stops, heading), [None], "{heading}: {stops:?}");
        }
        assert!(fields_of(&stops, "orient_scale").is_empty() && fields_of(&stops, "gyro_invert").is_empty(), "{stops:?}");

        let open = |mut m: serde_json::Value| {
            m["fold_orient"] = false.into();
            m["fold_axes"] = false.into();
            m
        };
        let stops = stops_with(open(serde_json::json!({ "orient_auto_recenter": true })));
        assert_eq!(fields_of(&stops, "orient_scale"), [Some(0)], "Rotation ×: {stops:?}");
        assert_eq!(fields_of(&stops, "orient_drift"), [Some(0)], "Drift fix: {stops:?}");
        assert_eq!(fields_of(&stops, "orient_recenter"), [Some(0), Some(1)], "Re-center and Still below: {stops:?}");
        assert_eq!(fields_of(&stops, "orient_source"), [Some(0)], "the source: {stops:?}");
        assert_eq!(fields_of(&stops, "hold_offset"), [Some(0), Some(1), Some(2)], "the hold offsets: {stops:?}");
        assert_eq!(fields_of(&stops, "gyro_invert"), [Some(0), Some(1), Some(2)], "gyro inverts: {stops:?}");
        assert!(fields_of(&stops, "edge_turn").len() >= 2, "Edge turn's settings: {stops:?}");
        assert_eq!(fields_of(&stops, "lean_threshold"), [Some(0)], "the lean threshold: {stops:?}");
        assert!(fields_of(&stops, "steering_opts").is_empty(), "steering's options pass by while it points: {stops:?}");

        // Re-centring off: its Still below greys out and is passed by.
        let stops = stops_with(open(serde_json::json!({})));
        assert_eq!(fields_of(&stops, "orient_recenter"), [Some(0)], "only the switch: {stops:?}");

        // On the pad's own pose, rotation and drift fix grey out and pass by.
        let stops = stops_with(open(serde_json::json!({ "orient_source": "device" })));
        assert!(fields_of(&stops, "orient_scale").is_empty() && fields_of(&stops, "orient_drift").is_empty(), "{stops:?}");
    }

    /// 3DOF to 2D pinned whole with `extra` on its pin: its stops, and the sections the
    /// widget found in its body.
    fn widget_with(extra: serde_json::Value, params: serde_json::Value) -> (Vec<(String, Option<usize>)>, Vec<(String, String)>) {
        let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(1100.0, 800.0));
        let desc = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor)
            .find(|d| d.id == "processing.gyro_3dof").expect("3DOF");
        let inner = {
            let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
            let mut node = NodeData::from(&desc);
            if let serde_json::Value::Object(m) = params {
                node.params.extend(m);
            }
            let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), node);
            let mut pin = serde_json::json!({
                "inner_node_id": inner.0, "element_id": "whole_module", "pos": [16.0, 16.0], "size": [520.0, 760.0],
            });
            if let serde_json::Value::Object(m) = extra {
                pin.as_object_mut().expect("an object").extend(m);
            }
            sp.items.push(LayoutItem::Module(serde_json::from_value(pin).expect("a pin")));
            sp.selected_item = None;
            sp.selected_items.clear();
            inner
        };
        for _ in 0..6 {
            scene.frame(vec![egui::Event::PointerMoved(egui::pos2(1090.0, 790.0))]);
        }
        let stops = crate::app::FlexInputApp::nav_widget_stops(&scene.ctx, "processing.gyro_3dof", "whole_module", inner.0, egui::ViewportId::ROOT)
            .expect("the widget has stops");
        let found = crate::canvas::viewer::widget_sections(&scene.ctx, inner.0, "processing.gyro_3dof");
        (stops.into_iter().map(|(p, f, _)| (p, f)).collect(), found)
    }

    /// The Inspector picks which of a module's sections its widget shows, and can keep them
    /// all open: a section left out is gone - heading, settings and stops - while the
    /// Inspector still lists it; kept open, a section folded on the module shows its settings
    /// in the widget, with no heading to fold it by.
    #[test]
    fn a_module_widget_shows_the_sections_its_pin_picks() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let has = |stops: &[(String, Option<usize>)], part: &str| stops.iter().any(|(p, _)| p == part);

        let (stops, found) = widget_with(serde_json::json!({}), serde_json::json!({ "fold_orient": false }));
        let keys: Vec<&str> = found.iter().map(|(k, _)| k.as_str()).collect();
        for key in ["mode", "orient", "axes", "lean"] {
            assert!(keys.contains(&key), "the Inspector is offered {key}: {found:?}");
        }
        assert!(has(&stops, "fold:orient") && has(&stops, "orient_scale"), "{stops:?}");

        let (stops, found) = widget_with(serde_json::json!({ "hidden_sections": ["orient"] }), serde_json::json!({ "fold_orient": false }));
        assert!(!has(&stops, "fold:orient") && !has(&stops, "orient_scale") && !has(&stops, "orient_source"), "left out: {stops:?}");
        assert!(has(&stops, "fold:mode"), "the rest stay: {stops:?}");
        assert!(found.iter().any(|(k, _)| k == "orient"), "a section left out is still offered: {found:?}");

        // Folded on the module (Orientation starts folded), kept open in the widget.
        let (stops, _) = widget_with(serde_json::json!({ "sections_open": true }), serde_json::json!({}));
        assert!(has(&stops, "orient_scale") && has(&stops, "gyro_invert"), "open: {stops:?}");
        assert!(!stops.iter().any(|(p, _)| p.starts_with("fold:")), "nothing to fold: {stops:?}");
    }

    /// JSM Config pinned whole wears its pin's graph colours - its curve's pane - and its
    /// text colour on the tuning faders' names. Pinned whole, the module draws its own body,
    /// which handed its graphs no colours, so the Inspector's changed nothing there.
    #[test]
    fn a_jsm_module_pinned_whole_wears_its_pins_colours() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let red = egui::Color32::from_rgb(200, 10, 10);
        let green = egui::Color32::from_rgb(10, 200, 10);
        let painted = |styled: bool| -> (bool, Option<egui::Color32>) {
            let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(900.0, 760.0));
            let desc = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor)
                .find(|d| d.id == "module.jsm").expect("JSM Config");
            {
                let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
                let mut node = NodeData::from(&desc);
                node.params.insert("jsm_tabs".into(), serde_json::json!([{ "name": "aim", "text": "MIN_GYRO_SENS = 1\nMAX_GYRO_SENS = 2\n" }]));
                node.params.insert("jsm_show_knobs".into(), serde_json::json!(true));
                let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), node);
                let ov = styled.then(|| serde_json::json!({ "background": [200, 10, 10, 255], "text": [10, 200, 10, 255] }));
                sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                    "inner_node_id": inner.0, "element_id": "whole_module", "pos": [16.0, 16.0], "size": [560.0, 620.0],
                    "graph_override": ov,
                })).expect("a pin")));
                sp.selected_item = None;
                sp.selected_items.clear();
            }
            let mut shapes = Vec::new();
            for _ in 0..6 {
                shapes = scene.frame(vec![egui::Event::PointerMoved(egui::pos2(890.0, 750.0))]);
            }
            fn walk(s: &egui::Shape, red: egui::Color32, pane: &mut bool, name: &mut Option<egui::Color32>) {
                match s {
                    egui::Shape::Rect(r) if r.fill == red => *pane = true,
                    egui::Shape::Text(t) if t.galley.text() == "MIN_GYRO_SENS" => {
                        *name = t.galley.job.sections.first().map(|sec| sec.format.color);
                    }
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, red, pane, name)),
                    _ => {}
                }
            }
            let (mut pane, mut name) = (false, None);
            shapes.iter().for_each(|c| walk(&c.shape, red, &mut pane, &mut name));
            (pane, name)
        };
        let (pane, name) = painted(true);
        assert!(pane, "no pane in the pin's colour");
        assert_eq!(name, Some(green), "the fader's name isn't in the pin's text colour");
        let (pane, name) = painted(false);
        assert!(!pane, "a pane in that colour without the pin's colours");
        assert!(name.is_some_and(|c| c != green), "the fader's name: {name:?}");
    }

    /// A pin's text capped at a size stays as it is at that size under a larger text size
    /// setting: RWS Aim pinned whole, capped at 15 points, has its fields where they are at 15
    /// with the setting at 18 - where the larger font's wider rows wrap onto more lines.
    #[test]
    fn a_pins_capped_text_keeps_its_rows_under_a_larger_text_size() {
        use crate::canvas::viewer::simple_bodies::{apply_text_styles, with_test_font_size};
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let fields = |size: f32, cap: Option<f32>| -> Vec<(String, egui::Rect)> {
            with_test_font_size(size, || {
                let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(900.0, 760.0));
                // egui's own text styles at the setting, as the app sets them.
                let mut style = (*scene.ctx.style()).clone();
                apply_text_styles(&mut style);
                scene.ctx.set_style(style);
                let desc = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor)
                    .find(|d| d.id == "processing.rws").expect("RWS Aim");
                let inner = {
                    let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
                    let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&desc));
                    sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                        "inner_node_id": inner.0, "element_id": "whole_module", "pos": [16.0, 16.0], "size": [440.0, 600.0],
                        "text_max": cap,
                    })).expect("a pin")));
                    sp.selected_item = None;
                    sp.selected_items.clear();
                    inner.0
                };
                for _ in 0..6 {
                    scene.frame(vec![egui::Event::PointerMoved(egui::pos2(890.0, 750.0))]);
                }
                let (_, parts) = widget_parts(&scene.ctx, inner).expect("the widget's fields");
                let origin = parts.iter().fold(egui::pos2(f32::INFINITY, f32::INFINITY), |o, (_, r)| o.min(r.min));
                parts.into_iter().map(|(e, r)| (e, r.translate(-origin.to_vec2()))).collect()
            })
        };
        let same = |a: &[(String, egui::Rect)], b: &[(String, egui::Rect)]| {
            a.len() == b.len()
                && a.iter().zip(b).all(|((ea, ra), (eb, rb))| ea == eb && (ra.min - rb.min).length() < 0.6 && (ra.size() - rb.size()).length() < 0.6)
        };
        let plain = fields(15.0, None);
        let larger = fields(18.0, None);
        let capped = fields(18.0, Some(15.0));
        assert!(plain.len() > 4, "the widget has fields: {plain:?}");
        assert!(!same(&plain, &larger), "the larger setting moves nothing, so this shows nothing");
        assert!(same(&plain, &capped), "capped at 15 under 18, the fields moved:\n{plain:?}\n{capped:?}");
        // A cap the setting is under changes nothing.
        assert!(same(&plain, &fields(15.0, Some(17.0))), "a cap above the setting moved the fields");
    }

    /// A pin's graph labels are its own: 3DOF to 2D's preview pinned at 150% draws its
    /// legend half as large again as one left as it is.
    #[test]
    fn a_pins_graph_labels_take_its_own_size() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let legend = |labels: Option<f32>| -> f32 {
            let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(900.0, 600.0));
            let desc = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor)
                .find(|d| d.id == "processing.gyro_3dof").expect("3DOF");
            {
                let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
                let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&desc));
                sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                    "inner_node_id": inner.0, "element_id": "preview", "pos": [16.0, 16.0], "size": [420.0, 300.0],
                    "graph_labels": labels,
                })).expect("a pin")));
                sp.selected_item = None;
                sp.selected_items.clear();
            }
            let mut shapes = Vec::new();
            for _ in 0..6 {
                shapes = scene.frame(vec![egui::Event::PointerMoved(egui::pos2(890.0, 590.0))]);
            }
            painted_texts(&shapes).into_iter().find(|(t, _, _)| t == "output").map(|(_, r, _)| r.height())
                .expect("the legend's first line")
        };
        let (plain, big) = (legend(None), legend(Some(1.5)));
        assert!((big / plain - 1.5).abs() < 0.15, "the legend at 150% is {big} tall beside {plain}");
    }

    /// 3DOF to 2D pinned whole sizes freely: its Lean card lists keep their own height, so
    /// the layout holds the widget to no "top part and one card" floor - read at the pin's
    /// width, that floor held its height to its width, and widening it only made it taller.
    #[test]
    fn a_module_widgets_card_lists_hold_it_to_no_height() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(1300.0, 800.0));
        let desc = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor)
            .find(|d| d.id == "processing.gyro_3dof").expect("3DOF");
        let inner = {
            let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
            let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&desc));
            sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                "inner_node_id": inner.0, "element_id": "whole_module", "pos": [0.0, 0.0], "size": [988.0, 592.0],
            })).expect("a pin")));
            sp.selected_item = None;
            sp.selected_items.clear();
            inner
        };
        for _ in 0..8 {
            scene.frame(vec![egui::Event::PointerMoved(egui::pos2(1290.0, 790.0))]);
        }
        let key = crate::canvas::viewer::pin_ws_nat_key(&scene.ctx, scene.oid.0, inner.0, "whole_module");
        let floor: Option<f32> = scene.ctx.data(|d| d.get_temp(crate::canvas::viewer::pin_min_body_h_id(key)));
        assert_eq!(floor, None, "the widget is held to a height");
    }

    /// A layout taller than the panel shades the edge its cut-off content is past: the
    /// bottom at first, the top as well once it's scrolled part way, so it reads as scrolling.
    #[test]
    fn a_layout_that_scrolls_shades_its_cut_off_edges() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let items = vec![
            rect_item([16.0, 16.0], [200.0, 80.0], [60, 60, 60, 255], None),
            rect_item([16.0, 1600.0], [200.0, 80.0], [60, 60, 60, 255], None),
        ];
        let mut scene = EasyScene::new(items, 0, false, egui::vec2(700.0, 500.0));
        let shades = |shapes: &[egui::epaint::ClippedShape]| -> Vec<f32> {
            fn walk(s: &egui::Shape, out: &mut Vec<f32>) {
                match s {
                    egui::Shape::Mesh(m) if m.vertices.len() == 4
                        && m.vertices.iter().any(|v| v.color == egui::Color32::from_black_alpha(110)) =>
                    {
                        let ys: Vec<f32> = m.vertices.iter().map(|v| v.pos.y).collect();
                        out.push(ys.iter().sum::<f32>() / 4.0);
                    }
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            let mut out = Vec::new();
            shapes.iter().for_each(|c| walk(&c.shape, &mut out));
            out
        };
        let mid = 250.0;
        let mut shapes = Vec::new();
        for _ in 0..4 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(egui::pos2(350.0, 300.0))]);
        }
        let at_top = shades(&shapes);
        assert!(at_top.iter().any(|y| *y > mid) && !at_top.iter().any(|y| *y < mid), "only the bottom: {at_top:?}");
        let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -300.0), modifiers: Default::default() };
        scene.frame(vec![egui::Event::PointerMoved(egui::pos2(350.0, 300.0)), wheel]);
        for _ in 0..3 {
            shapes = scene.frame(vec![egui::Event::PointerMoved(egui::pos2(350.0, 300.0))]);
        }
        let between = shades(&shapes);
        assert!(between.iter().any(|y| *y > mid) && between.iter().any(|y| *y < mid), "both edges: {between:?}");
    }

    /// A picture of a Response Curve pinned whole, the pad on its range row:
    /// `cargo test -p flexinput-ui --lib shoot_easy_module_widget -- --ignored`, PNG
    /// `target/module_shots/easy_module_widget.png`.
    #[test]
    #[ignore]
    fn shoot_easy_module_widget() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // As it is, and zoomed out to 60%.
        for (zoom, name) in [(None, "easy_module_widget"), (Some(0.6_f32), "easy_module_widget_zoomed")] {
            let mut scene = EasyScene::new(Vec::new(), 0, false, egui::vec2(800.0, 700.0));
            let curve_desc = flexinput_modules::all_modules()
                .into_iter()
                .map(|r| r.descriptor)
                .find(|d| d.id == "module.response_curve")
                .expect("the curve module");
            let inner = {
                let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
                let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&curve_desc));
                sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                    "inner_node_id": inner.0, "element_id": "whole_module", "pos": [16.0, 16.0], "size": [340.0, 330.0],
                    "zoom": zoom,
                })).expect("a pin")));
                sp.selected_item = None;
                sp.selected_items.clear();
                inner
            };
            let mut shapes = Vec::new();
            for _ in 0..8 {
                let pass = scene.ctx.cumulative_pass_nr();
                scene.ctx.data_mut(|d| d.insert_temp(crate::canvas::viewer::module_part_focus_id(inner.0), (pass, "range_row".to_string(), None::<usize>, false)));
                shapes = scene.frame(vec![]);
            }
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../target/module_shots/{name}.png"));
            scene.shot.save(&scene.ctx, &shapes, scene.screen, 1.0, &path).expect("the picture saves");
            println!("{}", path.display());
        }
    }

    /// Pictures of 3DOF to 2D pinned whole and zoomed to 74%, browsing and editing the layout:
    /// `cargo test -p flexinput-ui --lib shoot_easy_3dof_zoomed -- --ignored`, PNGs
    /// `target/module_shots/easy_3dof_zoomed_{browse,editing}.png`.
    #[test]
    #[ignore]
    fn shoot_easy_3dof_zoomed() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for editing in [false, true] {
            let mut scene = EasyScene::new(Vec::new(), 0, editing, egui::vec2(1300.0, 700.0));
            let desc = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor)
                .find(|d| d.id == "processing.gyro_3dof").expect("3DOF");
            {
                let sp = scene.canvas.snarl.get_node_mut(scene.oid).and_then(|n| n.subpatch.as_mut()).expect("the sub-patch");
                let mut node = NodeData::from(&desc);
                node.params.insert("axis".into(), serde_json::json!("orient"));
                node.params.insert("fold_orient".into(), serde_json::json!(false));
                node.params.insert("fold_axes".into(), serde_json::json!(false));
                let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), node);
                sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                    "inner_node_id": inner.0, "element_id": "whole_module", "pos": [0.0, 0.0], "size": [988.0, 592.0],
                    "zoom": 0.74,
                })).expect("a pin")));
                sp.selected_item = None;
                sp.selected_items.clear();
            }
            let mut shapes = Vec::new();
            for _ in 0..8 {
                shapes = scene.frame(vec![egui::Event::PointerMoved(egui::pos2(1290.0, 690.0))]);
            }
            let name = if editing { "easy_3dof_zoomed_editing" } else { "easy_3dof_zoomed_browse" };
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/module_shots").join(format!("{name}.png"));
            scene.shot.save(&scene.ctx, &shapes, scene.screen, 1.0, &path).expect("the picture saves");
            println!("{}", path.display());
        }
    }

    /// Pictures of the Easy header at a narrow width - browsing and editing - so nothing in the
    /// row runs into the mode buttons:
    /// `cargo test -p flexinput-ui --lib shoot_easy_narrow_header -- --ignored`, PNGs
    /// `target/module_shots/easy_header_narrow_{browse,editing}.png`.
    #[test]
    #[ignore]
    fn shoot_easy_narrow_header() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for editing in [false, true] {
            let items = vec![rect_item([16.0, 16.0], [120.0, 80.0], [60, 60, 60, 180], None)];
            let mut scene = EasyScene::new(items, 0, editing, egui::vec2(430.0, 160.0));
            let mut shapes = Vec::new();
            for _ in 0..8 {
                shapes = scene.frame(vec![]);
            }
            let name = if editing { "easy_header_narrow_editing" } else { "easy_header_narrow_browse" };
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/module_shots").join(format!("{name}.png"));
            scene.shot.save(&scene.ctx, &shapes, scene.screen, 2.0, &path).expect("the picture saves");
            println!("{}", path.display());
        }
    }

    /// Every painted rect in `shapes`, with its fill.
    fn painted_rects(shapes: &[egui::epaint::ClippedShape]) -> Vec<(egui::Rect, egui::Color32)> {
        fn walk(s: &egui::Shape, out: &mut Vec<(egui::Rect, egui::Color32)>) {
            match s {
                egui::Shape::Rect(r) => out.push((r.rect, r.fill)),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|c| walk(&c.shape, &mut out));
        out
    }

    /// Editing a layout, an object is drawn whole until the pointer is over it, and then
    /// eases see-through so what it covers shows.
    #[test]
    fn a_layout_object_goes_see_through_under_the_pointer() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == "subpatch")
            .expect("the sub-patch module");
        let ctx = egui::Context::default();
        let mut canvas = Canvas::new();
        let oid = canvas.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&desc));
        let fill = egui::Color32::from_rgba_unmultiplied(200, 40, 40, 255);
        if let Some(n) = canvas.snarl.get_node_mut(oid) {
            let mut sp = UiSubPatch::default();
            sp.items.push(LayoutItem::Deco(crate::canvas::node::LayoutDecoration::Rect { tab: 0, fill_gradient: None,
                pos: [16.0, 16.0],
                size: [120.0, 80.0],
                fill: fill.to_srgba_unmultiplied(),
                stroke: [0; 4],
                stroke_px: 0.0,
                corner_radius: 0.0,
                anchor: Default::default(),
            }));
            n.subpatch = Some(Box::new(sp));
            n.extra.layout_unlocked = true;
        }
        let mut easy_state = EasyState::default();
        let (mut overlay, mut config) = Default::default();
        let mut nav = crate::gamepad_nav::GamepadNav::default();
        let mut favorites = Vec::new();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 600.0));
        let mut run = |i: u32, events: Vec<egui::Event>| {
            let raw = egui::RawInput {
                screen_rect: Some(screen),
                time: Some(i as f64 / 30.0),
                events,
                ..Default::default()
            };
            ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui, &mut canvas, &mut easy_state, &mut overlay, &mut config, None,
                        Default::default(), &[], &[], &Default::default(), &Default::default(),
                        &Default::default(), &Default::default(), &mut favorites, &mut nav,
                    );
                });
            })
            .shapes
        };
        // The object: the rect painted in its red, as opaque as it was set.
        let red = |c: egui::Color32| c.r() > 0 && c.g() < c.r() / 3 && c.b() < c.r() / 3;
        let mut shapes = Vec::new();
        for i in 0..6 {
            shapes = run(i, vec![]);
        }
        let (at, whole) = painted_rects(&shapes).into_iter().find(|(_, c)| red(*c)).expect("the object is painted");
        assert_eq!(whole.a(), 255, "away from the pointer it's drawn whole");
        for i in 6..20 {
            shapes = run(i, vec![egui::Event::PointerMoved(at.center())]);
        }
        let (_, under) = painted_rects(&shapes).into_iter().find(|(_, c)| red(*c)).expect("the object is painted");
        assert!(under.a() < 140, "under the pointer it goes see-through: {under:?}");
    }

    /// An Easy layout with mapping cards pinned smaller than their design - a Remapper whole and
    /// a 3DOF Lean list - locked or being edited, scrolled a little so the top of the layout is
    /// under the header row: the last frame's shapes, and its textures for a picture.
    fn pinned_cards_frame(editing: bool) -> (egui::Context, Vec<egui::epaint::ClippedShape>, crate::canvas::shot::Shot) {
        let module = |id: &str| flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == id)
            .expect("the module");
        let ctx = egui::Context::default();
        crate::app::fonts::setup_fonts(&ctx);
        let mut canvas = Canvas::new();
        let oid = canvas.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&module("subpatch")));
        if let Some(n) = canvas.snarl.get_node_mut(oid) {
            let mut sp = UiSubPatch::default();
            let remap = sp.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&module("module.remapper")));
            let gyro = sp.snarl.insert_node(egui::pos2(400.0, 0.0), NodeData::from(&module("processing.gyro_3dof")));
            let pin = |node: egui_snarl::NodeId, element: &str, pos: [f32; 2], size: [f32; 2]| {
                LayoutItem::Module(serde_json::from_value(serde_json::json!({
                    "inner_node_id": node.0, "element_id": element, "pos": pos, "size": size,
                })).expect("a pin"))
            };
            sp.items.push(pin(remap, "whole_module", [16.0, 16.0], [250.0, 200.0]));
            sp.items.push(pin(gyro, "lean_left", [300.0, 16.0], [300.0, 160.0]));
            sp.items.push(LayoutItem::Deco(crate::canvas::viewer::make_default_decoration("rect")));
            if let Some(LayoutItem::Deco(crate::canvas::node::LayoutDecoration::Rect { pos, size, .. })) = sp.items.last_mut() {
                *pos = [16.0, 900.0];
                *size = [100.0, 40.0];
            }
            n.subpatch = Some(Box::new(sp));
            n.extra.layout_unlocked = editing;
        }
        let mut easy_state = EasyState::default();
        let (mut overlay, mut config) = Default::default();
        let mut nav = crate::gamepad_nav::GamepadNav::default();
        let mut favorites = Vec::new();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 420.0));
        let mut shot = crate::canvas::shot::Shot::default();
        let mut shapes = Vec::new();
        for i in 0..12 {
            let events = if (5..9).contains(&i) {
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -12.0), modifiers: Default::default(),
                }]
            } else {
                // Away from the pins, so none of them is see-through under it.
                vec![egui::Event::PointerMoved(egui::pos2(1000.0, 400.0))]
            };
            let raw = egui::RawInput { screen_rect: Some(screen), time: Some(i as f64 / 30.0), events, ..Default::default() };
            let out = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui, &mut canvas, &mut easy_state, &mut overlay, &mut config, None,
                        Default::default(), &[], &[], &Default::default(), &Default::default(),
                        &Default::default(), &Default::default(), &mut favorites, &mut nav,
                    );
                });
            });
            shot.absorb(&out.textures_delta);
            shapes = out.shapes;
        }
        (ctx, shapes, shot)
    }

    /// Every painted text in `shapes` with its words, where it lands and what it's clipped to.
    fn painted_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Rect, egui::Rect)> {
        fn walk(s: &egui::Shape, clip: egui::Rect, out: &mut Vec<(String, egui::Rect, egui::Rect)>) {
            match s {
                egui::Shape::Text(t) => out.push((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2()), clip)),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, clip, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|c| walk(&c.shape, c.clip_rect, &mut out));
        out
    }

    /// A pinned mapping list being edited is drawn as it is locked: its words the size they
    /// are locked, so they stay inside their buttons on a pin smaller than its design (they came
    /// out the design's size), and cut where the layout is cut (they drew over the header row
    /// above a layout scrolled down).
    #[test]
    fn edited_pins_draw_their_cards_as_locked() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let find = |texts: &[(String, egui::Rect, egui::Rect)], words: &str| {
            texts.iter().find(|(t, _, _)| t.starts_with(words)).cloned().unwrap_or_else(|| panic!("{words:?} is painted"))
        };
        let locked = painted_texts(&pinned_cards_frame(false).1);
        let edited = painted_texts(&pinned_cards_frame(true).1);
        for words in ["Learn", "Choose"] {
            let (_, l, lclip) = find(&locked, words);
            let (_, e, eclip) = find(&edited, words);
            assert!((l.height() - e.height()).abs() < 1.0 && (l.width() - e.width()).abs() < 2.0,
                "{words:?} is drawn {e:?} being edited, {l:?} locked");
            assert!(eclip.top() >= lclip.top() - 1.0,
                "{words:?} is cut at {eclip:?} being edited, {lclip:?} locked: it reaches over the header");
        }
    }

    /// Pictures of [`pinned_cards_frame`], locked and being edited:
    /// `cargo test -p flexinput-ui --lib shoot_easy_pinned_cards -- --ignored`, PNGs
    /// `target/module_shots/easy_cards_{locked,editing}.png`.
    #[test]
    #[ignore]
    fn shoot_easy_pinned_cards() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for editing in [false, true] {
            let (ctx, shapes, shot) = pinned_cards_frame(editing);
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 420.0));
            let name = if editing { "easy_cards_editing" } else { "easy_cards_locked" };
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/module_shots")
                .join(format!("{name}.png"));
            shot.save(&ctx, &shapes, screen, 1.0, &path).expect("the picture saves");
            println!("{}", path.display());
        }
    }

    /// Pictures of Easy mode's header row and left panel, the layout being edited and not:
    /// `cargo test -p flexinput-ui --lib shoot_easy_layout_editor -- --ignored`, PNGs in
    /// `target/module_shots/easy_header_{browse,editing}.png`.
    #[test]
    #[ignore]
    fn shoot_easy_layout_editor() {
        let _index = super::super::presets::INDEX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == "subpatch")
            .expect("the sub-patch module");
        // Browsing; editing with the gradient rectangle selected; editing with a pinned
        // Response Curve selected; editing a layout with tabs, its strip selected.
        for (editing, curve, tabs) in [(false, false, false), (true, false, false), (true, true, false), (true, false, true)] {
            let ctx = egui::Context::default();
            crate::app::fonts::setup_fonts(&ctx);
            let mut canvas = Canvas::new();
            let oid = canvas.snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&desc));
            if let Some(n) = canvas.snarl.get_node_mut(oid) {
                let mut sp = UiSubPatch::default();
                sp.display_name = "Shooter".into();
                if curve {
                    let curve_desc = flexinput_modules::all_modules()
                        .into_iter()
                        .map(|r| r.descriptor)
                        .find(|d| d.id == "module.response_curve")
                        .expect("the curve module");
                    let mut node = NodeData::from(&curve_desc);
                    node.display_name = "Left stick curve".into();
                    let inner = sp.snarl.insert_node(egui::pos2(0.0, 0.0), node);
                    sp.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
                        "inner_node_id": inner.0, "element_id": "curve", "pos": [16.0, 16.0], "size": [296.0, 232.0],
                    })).expect("a pin")));
                }
                use crate::canvas::node::{Gradient, GradientKind, LayoutDecoration};
                let mut rect = crate::canvas::viewer::make_default_decoration("rect");
                if let LayoutDecoration::Rect { fill_gradient, size, .. } = &mut rect {
                    *size = [220.0, 120.0];
                    let mut g = Gradient::new(GradientKind::Linear, [40, 120, 255, 255], [255, 80, 160, 255]);
                    g.add_stop_at(0.6);
                    g.stops[1].color = [255, 230, 90, 255];
                    *fill_gradient = Some(g);
                }
                let mut ellipse = crate::canvas::viewer::make_default_decoration("ellipse");
                if let LayoutDecoration::Ellipse { fill_gradient, pos, .. } = &mut ellipse {
                    *pos = [280.0, 30.0];
                    *fill_gradient = Some(Gradient::new(GradientKind::Radial, [255, 255, 255, 255], [20, 160, 120, 0]));
                }
                sp.items.push(LayoutItem::Deco(rect));
                sp.items.push(LayoutItem::Deco(ellipse));
                sp.selected_item = Some(0);
                sp.selected_items = vec![0];
                if tabs {
                    sp.layout_tabs = vec!["Aim".into(), "Move".into(), "Menu".into()];
                    if let Some(it) = sp.items.last_mut() {
                        it.set_tab(Some(1));
                    }
                }
                n.subpatch = Some(Box::new(sp));
                n.extra.layout_unlocked = editing;
            }
            let mut easy_state = EasyState::default();
            let (mut overlay, mut config) = Default::default();
            let mut nav = crate::gamepad_nav::GamepadNav::default();
            let mut favorites = Vec::new();
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 560.0));
            let mut shot = crate::canvas::shot::Shot::default();
            let mut shapes = Vec::new();
            for i in 0..8 {
                let raw = egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(i as f64 / 30.0),
                    ..Default::default()
                };
                let out = ctx.run(raw, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let total = ui.available_rect_before_wrap();
                        let left = egui::Rect::from_min_size(total.min, egui::vec2(280.0, total.height()));
                        let center = egui::Rect::from_min_max(egui::pos2(left.right() + 6.0, total.top()), total.max);
                        ui.painter().rect_filled(left, 0.0, egui::Color32::from_gray(0x1a));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(left.shrink2(egui::vec2(10.0, 0.0))), |ui| {
                            match layout_editing_subpatch(&canvas) {
                                Some(id) => show_layout_inspector(ui, &mut canvas, id),
                                None => {
                                    ui.add_space(44.0);
                                    ui.label("(the Devices panel)");
                                }
                            }
                        });
                        ui.scope_builder(egui::UiBuilder::new().max_rect(center), |ui| {
                            show(
                                ui, &mut canvas, &mut easy_state, &mut overlay, &mut config, None,
                                Default::default(), &[], &[], &Default::default(), &Default::default(),
                                &Default::default(), &Default::default(), &mut favorites, &mut nav,
                            );
                        });
                    });
                });
                shot.absorb(&out.textures_delta);
                shapes = out.shapes;
            }
            let name = match (editing, curve) {
                _ if tabs => "easy_layout_tabs",
                (_, true) => "easy_inspector_curve",
                (true, _) => "easy_header_editing",
                _ => "easy_header_browse",
            };
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/module_shots")
                .join(format!("{name}.png"));
            shot.save(&ctx, &shapes, screen, 1.0, &path).expect("the picture saves");
            println!("{}", path.display());
        }
    }
}
