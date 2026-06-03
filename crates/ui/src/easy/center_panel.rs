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
    user_presets_folder: Option<&Path>,
    defaults: DeviceParamDefaults,
    descriptors: &[ModuleDescriptor],
    physical_devices: &[PhysicalDevice],
    live_device_ids: &HashSet<String>,
    live_signals: &HashMap<(String, String), Signal>,
    panic_shortcut: &PanicShortcut,
    device_rates: &HashMap<String, u32>,
    favorites: &mut Vec<PathBuf>,
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

    show_preset_picker(ui, canvas, easy_state, &presets, favorites);
    show_pending_modal(ui, canvas, easy_state, &presets);

    // Layout-editing properties panel — directly below the preset bar, only
    // while 🎨 (layout edit) is active and a sub-patch exists. This mirrors the
    // controls row at the top of the sub-patch node in Advanced layout mode:
    // snap toggle + grid step, Add-decoration buttons, and the per-selection
    // style inspector.
    if matches!(compat, Compat::Ok) {
        if let Some(oid) = find_subpatch(canvas) {
            let unlocked = canvas.snarl.get_node(oid)
                .map(|n| n.extra.layout_unlocked)
                .unwrap_or(false);
            if unlocked {
                ui.add_space(4.0);
                // Render the inspector strip into a FOREGROUND-order layer so its
                // color pickers and buttons always win pointer input over the
                // sub-patch body, which lives in an Order::Middle Area. Without
                // this, body widgets that scroll up underneath the inspector
                // (their interact rects ride a transformed sublayer that the
                // body clip doesn't bound for hit-testing) sit "in front" of the
                // Background-layer inspector and silently swallow clicks.
                //
                // A layered child UI doesn't advance the parent's cursor, so we
                // measure the rendered height and reserve it explicitly, keeping
                // the body below the inspector in the normal flow.
                let start = ui.cursor().min;
                let avail_w = ui.available_width();
                let inspector_layer = egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new(("easy_layout_inspector_layer", oid.0)),
                );
                let built = ui.scope_builder(
                    egui::UiBuilder::new()
                        .layer_id(inspector_layer)
                        .max_rect(egui::Rect::from_min_size(
                            start,
                            egui::vec2(avail_w, ui.available_height()),
                        )),
                    |ui| {
                        ui.set_width(avail_w);
                        egui::Frame::group(ui.style())
                            .inner_margin(egui::Margin::symmetric(8, 6))
                            .show(ui, |ui| {
                                ui.set_width(avail_w - 16.0);
                                ui.label(egui::RichText::new("Layout editing").small().strong().weak());
                                egui::ScrollArea::horizontal()
                                    .id_salt(("easy_layout_controls_scroll", oid.0))
                                    .show(ui, |ui| {
                                        crate::canvas::viewer::layout_editing_controls(
                                            ui, &mut canvas.snarl, oid);
                                    });
                            });
                    },
                );
                // Reserve the inspector's footprint in the parent flow so the
                // body Area below starts beneath it.
                let used = built.response.rect;
                ui.allocate_rect(used, egui::Sense::hover());
            }
        }
    }

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
                render_subpatch_body(
                    ui, canvas, defaults,
                    descriptors, physical_devices, live_device_ids,
                    live_signals, panic_shortcut, device_rates,
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
    let dirty = is_subpatch_dirty(canvas, easy_state, presets);

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

    let mut bar_rect_out: Option<egui::Rect> = None;

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
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    // Advance the parent cursor past the row we just claimed.
    ui.allocate_rect(row_rect, egui::Sense::hover());
    {
        let ui = &mut row_ui;
        // Left padding so the cluster is roughly centered.
        let label_w = 56.0; // "Preset:" label
        // 🎨 🔧 (layout/edit) + 💾 📂 (save/load) buttons + spacing.
        let trailing_w = 150.0;
        // Bar width = whatever fits between label and the trailing
        // buttons, clamped so it doesn't collapse on narrow windows.
        let bar_w = (ui.available_width() - label_w - trailing_w - 16.0)
            .clamp(220.0, 560.0);
        let pad = ((ui.available_width() - label_w - bar_w - trailing_w - 16.0) * 0.5)
            .max(0.0);
        ui.add_space(pad);

        ui.label(egui::RichText::new("Preset:").strong());

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
                    // default filename when saving a .fxsp). Saving
                    // from the sub-patch editor will then write a file
                    // named after the preset title, so the next time
                    // it appears in the preset list it uses the same
                    // name.
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

        // ── Save (💾) + Load (📂) buttons ───────────────────────────
        // Save/Load come FIRST (left), with the layout-edit/patch-edit
        // pair after them (right), separated by a small gap.
        ui.add_space(10.0);
        if ui.add_sized([28.0, 28.0],
            egui::Button::new(egui::RichText::new("💾").size(14.0)))
            .on_hover_text("Save sub-patch to .fxsp")
            .clicked()
        {
            if let Some(oid) = outer_id {
                if let Some(sp) = canvas.snarl.get_node(oid).and_then(|n| n.subpatch.as_ref()) {
                    if let Some(path) = crate::app::save_subpatch_file(sp) {
                        let hash = hash_subpatch(sp);
                        easy_state.loaded_preset = Some((path, hash));
                    }
                }
            }
        }
        if ui.add_sized([28.0, 28.0],
            egui::Button::new(egui::RichText::new("📂").size(14.0)))
            .on_hover_text("Load sub-patch from .fxsp")
            .clicked()
        {
            if let Some(loaded) = crate::app::load_subpatch_file() {
                apply_subpatch_inline(canvas, easy_state, &loaded, None);
            }
        }

        // ── Layout-edit (🎨) + Edit (🔧) buttons ────────────────────
        // Separated slightly from Save/Load. 🎨 toggles the sub-patch's
        // layout-unlocked state (drag/resize/select decorations in
        // place); 🔧 opens the full Advanced sub-patch editor window.
        ui.add_space(10.0);
        let layout_unlocked = outer_id
            .and_then(|id| canvas.snarl.get_node(id))
            .map(|n| n.extra.layout_unlocked)
            .unwrap_or(false);
        let palette_btn = egui::SelectableLabel::new(
            layout_unlocked, egui::RichText::new("🎨").size(14.0));
        if ui.add_sized([28.0, 28.0], palette_btn)
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
            }
        }
        if ui.add_sized([28.0, 28.0],
            egui::Button::new(egui::RichText::new("🔧").size(14.0)))
            .on_hover_text("Open the sub-patch editor (add/wire modules)")
            .clicked()
        {
            if let Some(oid) = outer_id {
                // Same mechanism the Advanced canvas uses: stash the
                // request; `show_subpatch_editors` consumes it next
                // frame and spawns the editor viewport window.
                canvas.pending_edit_subpatch = Some(oid);
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
                    egui::Color32::from_rgb(0x1a, 0x1a, 0x1a),
                    egui::Stroke::new(1.0,
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
    /// Screen rect of the name text area.
    name_rect: egui::Rect,
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
    let bg = egui::Color32::from_rgb(0x1a, 0x1a, 0x1a);
    let stroke = egui::Stroke::new(1.0,
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
        egui::FontId::proportional(16.0),
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
        egui::FontId::proportional(13.0),
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
                .font(egui::FontId::proportional(15.0))
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
            egui::FontId::proportional(15.0),
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
        name_rect,
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
            egui::RichText::new(&name_text).size(18.0).strong(),
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
        egui::FontId::proportional(15.0),
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
            let stroke = egui::Stroke::new(1.5, ui.visuals().weak_text_color());
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
        egui::FontId::proportional(13.0),
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
    super::wiring::rewire(canvas);
    super::layout::reposition_io_nodes(canvas);
}

fn preset_entry(
    ui: &mut egui::Ui,
    p: &PresetInfo,
    canvas: &mut Canvas,
    easy_state: &mut EasyState,
    favorites: &mut Vec<PathBuf>,
    presets: &[PresetInfo],
) {
    let is_fav = favorites.iter().any(|f| f == &p.path);
    let star = if is_fav { "★" } else { "☆" };
    let star_resp = ui.small_button(star)
        .on_hover_text(if is_fav { "Remove from favorites" } else { "Add to favorites" });
    if star_resp.clicked() {
        if is_fav {
            favorites.retain(|f| f != &p.path);
        } else {
            favorites.push(p.path.clone());
        }
    }

    let active = easy_state.loaded_preset.as_ref().map(|(p2, _)| p2.as_path()) == Some(p.path.as_path());
    let resp = ui.add(egui::SelectableLabel::new(active, &p.display_name))
        .on_hover_text(p.path.display().to_string());
    if resp.clicked() && !active {
        if is_subpatch_dirty(canvas, easy_state, presets) {
            easy_state.pending_preset_switch = Some(p.path.clone());
        } else {
            apply_preset(canvas, easy_state, p);
        }
        ui.close();
    }
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
    if counts.get("device.source").copied().unwrap_or(0) > 1 { return Compat::Incompatible; }
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

fn find_subpatch(canvas: &Canvas) -> Option<NodeId> {
    canvas.snarl.nodes_ids_data()
        .find(|(_, n)| n.value.module_id == "subpatch")
        .map(|(id, _)| id)
}

// ── Sub-patch body render ───────────────────────────────────────────

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
        if pointer_over {
            let wheel = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                scroll_y = (scroll_y - wheel).clamp(0.0, scroll_overflow);
            }
        }
    }

    // Convert scroll offset to body-local coords (post-scale-inverse).
    let scroll_body = scroll_y / scale;

    let area_id = egui::Id::new(("easy_subpatch_body_area", outer_id.0));
    egui::Area::new(area_id)
        .order(egui::Order::Middle)
        .movable(false)
        .interactable(true)
        .fixed_pos(panel_rect.min)
        .constrain_to(panel_rect)
        .show(ui.ctx(), |ui| {
            ui.set_clip_rect(panel_rect);

            // Dedicated body layer: scale + translate so body-local
            // coords map to (panel_rect.min + body_pos*scale - (0, scroll_y)).
            let body_layer = egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new(("easy_body_scale_layer", outer_id.0)),
            );
            ui.ctx().set_sublayer(ui.layer_id(), body_layer);
            let xform = egui::emath::TSTransform::new(
                panel_rect.min.to_vec2() - egui::vec2(0.0, scroll_y),
                scale,
            );
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
            let _ = scaled_w; // reserved for future right-edge layout
        });

    ui.ctx().data_mut(|d| d.insert_temp(scroll_id, scroll_y));
}

// ── Dirty detection + preset application ────────────────────────────

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

fn apply_preset(canvas: &mut Canvas, easy_state: &mut EasyState, p: &PresetInfo) {
    let Ok(bytes) = std::fs::read(&p.path) else { return; };
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
        n.subpatch = Some(Box::new(file.sub_patch.clone()));
        n.display_name = p.display_name.clone();
        mirror_pins(n, &file.sub_patch);
    }

    easy_state.loaded_preset = Some((p.path.clone(), p.content_hash));
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
