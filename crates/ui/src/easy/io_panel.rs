//! Easy-mode left panel — combined Input + Output picker.
//!
//! Top section ("Choose input device"): scrollable list of physical
//! gamepad cards. Each card holds icon + name + Calibrate row +
//! Deadzone / Gyro × sliders. All cards always render the controls
//! (inactive cards preview the defaults disabled) for visual
//! stability.
//!
//! Bottom section ("Choose output devices"): a single gamepad output
//! card with a model selector (None / Xbox 360 / DualShock 4 /
//! DualSense) and, when a pad is active, its Rumble-range control;
//! then a full-width "Keyboard and Mouse" card with the Mouse speed
//! slider inline. All gamepad models are HIDMaestro-backed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eframe::egui;
use egui_snarl::NodeId;
use flexinput_devices::{ControllerKind, PhysicalDevice};
use flexinput_virtual::{create_device, kind_prefix};
use serde_json::Value;

use crate::canvas::remapper_icons;

/// Card key for the virtual gamepad-output card's slot requests.
pub const XINPUT_CARD_OUTPUT: &str = "output";

/// Pending XInput slot-assignment requests from the slot circles, as
/// `(card_key, target_slot)`. The app drains these each frame
/// ([`drain_xinput_slot_requests`]) and dispatches them to the elevated helper's
/// reorder engine. A static avoids threading a request channel through the whole
/// Easy-panel render call chain (the circles are drawn deep inside it).
static XINPUT_SLOT_REQUESTS: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());

/// Queue a request to move `card_key` to player `slot`.
pub fn request_xinput_slot(card_key: &str, slot: usize) {
    if let Ok(mut q) = XINPUT_SLOT_REQUESTS.lock() {
        q.push((card_key.to_string(), slot));
    }
}

/// Drain queued slot-assignment requests (called by the app each frame).
pub fn drain_xinput_slot_requests() -> Vec<(String, usize)> {
    XINPUT_SLOT_REQUESTS
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

/// Optimistic record of which player slot each XInput device was last assigned to,
/// keyed by `device_id`. The reorder engine places a device at the slot the user
/// clicked, so we remember that immediately on dispatch and use it to draw the
/// "this device's slot" glow even when several XInput devices are present (XInput
/// exposes only slot→state, so we cannot otherwise correlate instance→slot). An
/// entry is trusted only while that slot is actually occupied.
static XINPUT_SLOT_ASSIGNMENTS: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

/// Remember that `device_id` was assigned to `slot` (called by the app when it
/// dispatches a slot request to the reorder engine).
pub fn record_xinput_slot_assignment(device_id: &str, slot: usize) {
    if let Ok(mut g) = XINPUT_SLOT_ASSIGNMENTS.lock() {
        g.get_or_insert_with(HashMap::new).insert(device_id.to_string(), slot);
    }
}

/// The slot `device_id` was last assigned to, if any.
fn assigned_xinput_slot(device_id: &str) -> Option<usize> {
    XINPUT_SLOT_ASSIGNMENTS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(device_id).copied()))
}
use crate::canvas::{header_controls, Canvas, DeviceParamDefaults};
use crate::panels::device_icon::{ping_device_icon, render_device_icon};
use crate::panels::virtual_devices::SharedDevicePool;

/// Shared queue of rumble-ping requests drained by the I/O thread.
pub type PingRequests = Arc<Mutex<Vec<String>>>;

// Card colors — chosen to pop on the dark left-panel background
// (#1a1a1a, painted by the app.rs call-site). The active variant
// uses egui's selection color so it matches whatever theme accent
// the user has configured.
const CARD_FILL_INACTIVE:    egui::Color32 = egui::Color32::from_rgb(0x2a, 0x2a, 0x2a);
const CARD_STROKE_INACTIVE:  egui::Color32 = egui::Color32::from_rgb(0x3a, 0x3a, 0x3a);

const PANEL_PADDING: f32 = 10.0;
const SECTION_GAP:   f32 = 14.0;
const CARD_GAP:      f32 = 8.0;
const CARD_ROUND:    f32 = 12.0;

// Input card geometry
const INPUT_CARD_ICON_H: f32 = 48.0;

// Output card geometry
const OUTPUT_CARD_ICON_H:    f32 = 36.0;
// Single gamepad output card: title row + selector row + Rumble-range row, with
// an icon in the left accent column. ~2 + 18 + 4 + 24 + 6 + 24 + insets ≈ 92 px.
// Sized for the active (rumble-visible) case so the layout stays stable whether
// or not a pad is deployed.
const OUTPUT_GAMEPAD_CARD_H: f32 = 92.0;
// Keymouse: title row + Mouse speed (label+value) row + slider row.
// ~18 + 18 + 14 + small gaps + insets ≈ 76 px.
const OUTPUT_KBM_CARD_H:     f32 = 78.0;

// Easy-mode gamepad outputs are HIDMaestro-backed (ViGEm removed). Old patches
// that used the ViGEm kinds are migrated to these on load.
const KIND_XINPUT:    &str = "virtual.hm.xinput";
const KIND_DS4:       &str = "virtual.hm.ds4";
const KIND_DUALSENSE: &str = "virtual.hm.dualsense";
const KIND_KEYMOUSE:  &str = "virtual.keymouse";

/// The selectable gamepad output models, in dropdown order: `(kind id, label)`.
/// One single gamepad output card cycles between these (and "None"); picking a
/// model deploys it and replaces any other gamepad already active.
const GAMEPAD_KINDS: &[(&str, &str)] = &[
    (KIND_XINPUT,    "Xbox 360"),
    (KIND_DS4,       "DualShock 4"),
    (KIND_DUALSENSE, "DualSense"),
];

/// Display label for a gamepad kind id (falls back to a generic name).
fn gamepad_label(kind_id: &str) -> &'static str {
    GAMEPAD_KINDS.iter().find(|(k, _)| *k == kind_id).map(|(_, l)| *l).unwrap_or("Gamepad")
}

pub fn show(
    ui: &mut egui::Ui,
    devices: &[PhysicalDevice],
    canvas: &mut Canvas,
    shared_pool: &SharedDevicePool,
    default_collapsed: bool,
    defaults: DeviceParamDefaults,
    calibrate_request: &mut Option<NodeId>,
    device_rates_hz: &HashMap<String, u32>,
    ping_requests: &PingRequests,
    nav_mode: &mut HashMap<String, bool>,
    nav_mode_default: bool,
    nav_excluded: &std::collections::HashSet<String>,
) {
    // Collector for gamepad-nav left-panel targets (rect + action), published
    // to a ctx temp at the end so the nav driver can hit-test the RS/gyro cursor
    // against device cards, sliders, checkboxes, and output toggles.
    let mut nav_targets: Vec<crate::gamepad_nav::LeftNavTarget> = Vec::new();

    // Reserve a fixed bottom area for the output section; the input
    // list scrolls within whatever remains above it. The output
    // section height is the sum of the two card heights + spacing +
    // section header.
    let total = ui.available_rect_before_wrap();
    let output_h = OUTPUT_GAMEPAD_CARD_H + CARD_GAP + OUTPUT_KBM_CARD_H
        + 28.0 /* header */ + SECTION_GAP + PANEL_PADDING;
    let top_h = (total.height() - output_h).max(120.0);
    let top_rect = egui::Rect::from_min_size(
        total.min,
        egui::vec2(total.width(), top_h),
    );
    let bot_rect = egui::Rect::from_min_size(
        egui::pos2(total.min.x, total.min.y + top_h),
        egui::vec2(total.width(), output_h),
    );

    ui.scope_builder(egui::UiBuilder::new().max_rect(top_rect), |ui| {
        // Force the clip rect on this scope to the input section's
        // bounds so card painter calls (and child-UI widgets that
        // narrow their own clip) can be re-intersected against this
        // outer viewport — preventing input-card content from spilling
        // into the output section when the list overflows.
        ui.set_clip_rect(top_rect);
        show_input_section(
            ui, devices, canvas, default_collapsed, defaults,
            calibrate_request, device_rates_hz, ping_requests,
            nav_mode, nav_mode_default, nav_excluded, &mut nav_targets,
        );
    });
    ui.scope_builder(egui::UiBuilder::new().max_rect(bot_rect), |ui| {
        ui.set_clip_rect(bot_rect);
        show_output_section(ui, canvas, shared_pool, default_collapsed, defaults,
            &mut nav_targets);
    });

    // Publish the collected nav targets (stamped with the current pass).
    let pass = ui.ctx().cumulative_pass_nr();
    ui.ctx().data_mut(|d| {
        d.insert_temp(crate::gamepad_nav::left_targets_id(), (pass, nav_targets))
    });
}

/// Paint top + bottom gradient strips over the ScrollArea's
/// `viewport` (its `inner_rect`), hinting that more content exists
/// when the list overflows. Strip alpha fades in over the first
/// ~24 px of scroll travel so the cue doesn't pop in/out abruptly.
/// Mirrors the device-panel side fades from Advanced mode.
fn paint_scroll_fades(
    ui: &egui::Ui,
    viewport: egui::Rect,
    content_h: f32,
    offset_y: f32,
) {
    let viewport_h = viewport.height();
    if content_h <= viewport_h { return; }
    let max_offset = (content_h - viewport_h).max(0.0);
    let frac_above = (offset_y / 24.0).clamp(0.0, 1.0);
    let frac_below = ((max_offset - offset_y) / 24.0).clamp(0.0, 1.0);

    let paint_fade = |from_top: bool, frac: f32| {
        if frac <= 0.0 { return; }
        let band_h = 18.0_f32;
        let steps  = 6_i32;
        for i in 0..steps {
            let t0 = i as f32 / steps as f32;
            let t1 = (i + 1) as f32 / steps as f32;
            // Stronger near the edge, fading toward the middle.
            let alpha = ((1.0 - t0) * 160.0 * frac) as u8;
            let (y0, y1) = if from_top {
                (viewport.top()    + t0 * band_h,
                 viewport.top()    + t1 * band_h)
            } else {
                (viewport.bottom() - t1 * band_h,
                 viewport.bottom() - t0 * band_h)
            };
            let strip = egui::Rect::from_min_max(
                egui::pos2(viewport.left(),  y0),
                egui::pos2(viewport.right(), y1),
            );
            ui.painter().with_clip_rect(viewport).rect_filled(
                strip,
                egui::CornerRadius::ZERO,
                egui::Color32::from_black_alpha(alpha),
            );
        }
    };
    paint_fade(true,  frac_above);
    paint_fade(false, frac_below);
}

// ── Input ───────────────────────────────────────────────────────────

fn show_input_section(
    ui: &mut egui::Ui,
    devices: &[PhysicalDevice],
    canvas: &mut Canvas,
    default_collapsed: bool,
    defaults: DeviceParamDefaults,
    calibrate_request: &mut Option<NodeId>,
    device_rates_hz: &HashMap<String, u32>,
    ping_requests: &PingRequests,
    nav_mode: &mut HashMap<String, bool>,
    nav_mode_default: bool,
    nav_excluded: &std::collections::HashSet<String>,
    nav_targets: &mut Vec<crate::gamepad_nav::LeftNavTarget>,
) {
    ui.add_space(PANEL_PADDING);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new("Choose input device").size(15.0).strong());
    });
    ui.add_space(SECTION_GAP * 0.5);

    let gamepads: Vec<&PhysicalDevice> = devices.iter()
        .filter(|d| !matches!(d.kind, ControllerKind::MidiIn | ControllerKind::MidiOut))
        .collect();
    // Every selected device, not just the first: a preset with several AutoMap
    // inlets accepts one input device per inlet.
    let active_dev_ids: Vec<String> =
        active_sources(canvas).into_iter().map(|(_, d, _)| d).collect();
    let capacity = input_capacity(canvas);

    // Snapshot the actual viewport rect from INSIDE the ScrollArea's
    // closure (= the same clip rect the input_card painter calls see).
    // Reading `out.inner_rect` after the fact is slightly off from the
    // real clip — the fade band ends up misaligned with where cards
    // get cut. Setting this via interior mutability sidesteps that.
    use std::cell::Cell;
    let viewport_cell: Cell<Option<egui::Rect>> = Cell::new(None);

    let out = egui::ScrollArea::vertical()
        .id_salt("easy_input_scroll")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            // Capture the true clip rect cards will be drawn against.
            viewport_cell.set(Some(ui.clip_rect()));
            if gamepads.is_empty() {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("No gamepads detected.").weak());
                    ui.label(egui::RichText::new("Plug one in and it will appear here.").weak());
                });
                return;
            }
            for d in &gamepads {
                let is_active = active_dev_ids.iter().any(|a| a == &d.id);
                // FlexInput's own loopback virtual: nav is disabled (and forced
                // off) so it can't drive the UI from our own output.
                let nav_disabled = nav_excluded.contains(&d.id);
                let nav_on = if nav_disabled {
                    false
                } else {
                    *nav_mode.entry(d.id.clone()).or_insert(nav_mode_default)
                };
                let mut nav_toggle: Option<bool> = None;
                if input_card(
                    ui, d, is_active, canvas, calibrate_request, device_rates_hz, defaults,
                    ping_requests, nav_on, nav_disabled, &mut nav_toggle, nav_targets,
                ) && (capacity > 1 || !is_active) {
                    toggle_source(canvas, d, default_collapsed, defaults);
                    super::wiring::rewire(canvas);
                }
                if let Some(v) = nav_toggle {
                    nav_mode.insert(d.id.clone(), v);
                }
                ui.add_space(CARD_GAP);
            }
        });

    // Paint scroll-edge fade shadows over the visible viewport when
    // content overflows, hinting that more cards exist above/below.
    let viewport = viewport_cell.get().unwrap_or(out.inner_rect);
    paint_scroll_fades(ui, viewport, out.content_size.y, out.state.offset.y);
}

/// Active accent fill applied to PART of a card (top half of input
/// cards, left of keymouse card). The rest of the card stays on the
/// inactive fill so sliders / controls read on the usual background.
fn active_accent_fill(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().selection.bg_fill.gamma_multiply(0.30)
}

/// Render one input card. Returns true if any non-control area of
/// the card was clicked (signal to activate this device).
///
/// Card structure (split fill):
///
///   ┌─────────────────────────────────────┐
///   │ ICON   Name             Calibrate  │  ← active accent fill (top half)
///   │                          261 Hz     │
///   ├─────────────────────────────────────┤
///   │  Deadzone ▭▭▭▭▭▭▭▭▭▭▭▭▭▭▭ 0.06     │  ← inactive fill (sliders)
///   │  Gyro ×   ▭▭▭▭▭▭▭▭▭▭▭▭▭▭▭ 3.50     │
///   └─────────────────────────────────────┘
fn input_card(
    ui: &mut egui::Ui,
    d: &PhysicalDevice,
    is_active: bool,
    canvas: &mut Canvas,
    calibrate_request: &mut Option<NodeId>,
    device_rates_hz: &HashMap<String, u32>,
    defaults: DeviceParamDefaults,
    ping_requests: &PingRequests,
    nav_on: bool,
    nav_disabled: bool,
    nav_toggle: &mut Option<bool>,
    nav_targets: &mut Vec<crate::gamepad_nav::LeftNavTarget>,
) -> bool {
    let panel_avail = ui.available_width();
    let card_w = (panel_avail - 2.0 * PANEL_PADDING).max(180.0);
    // Top: icon (48 px) + tight inset. Bottom: two slider rows ~22 px
    // each + small gap + insets, plus a digital-trigger toggle row.
    let top_h = INPUT_CARD_ICON_H + 8.0;
    let trigger_row_h = 22.0;
    let bot_h = 60.0 + trigger_row_h;
    let card_h = top_h + bot_h;

    // Allocate the full card rect inside the parent VERTICAL layout
    // (the scroll area). Padding on left/right is handled by sizing
    // the inner rect to card_w (< panel width) and centering it.
    let total_h = card_h;
    let (full_row, _) = ui.allocate_exact_size(
        egui::vec2(panel_avail, total_h),
        egui::Sense::hover(),
    );
    let card_rect = egui::Rect::from_min_size(
        egui::pos2(full_row.left() + PANEL_PADDING, full_row.top()),
        egui::vec2(card_w, card_h),
    );

    // Viewport clip — intersect every painter call below with the
    // parent ui's existing clip rect (the ScrollArea's viewport) AND
    // the card_rect so card content never escapes either bound.
    let viewport_clip = ui.clip_rect();
    let card_visible_clip = card_rect.intersect(viewport_clip);

    // ── Pass 1: paint card bg + (if active) top-half accent ──
    let stroke_col = if is_active {
        ui.visuals().selection.stroke.color
    } else {
        CARD_STROKE_INACTIVE
    };
    let stroke_w = if is_active { 1.5 } else { 1.0 };
    ui.painter()
        .with_clip_rect(viewport_clip)
        .rect(card_rect, CARD_ROUND, CARD_FILL_INACTIVE,
            egui::Stroke::new(stroke_w, stroke_col), egui::StrokeKind::Inside);
    if is_active {
        let top_band = egui::Rect::from_min_size(
            card_rect.min,
            egui::vec2(card_rect.width(), top_h),
        );
        let cr = egui::CornerRadius {
            nw: CARD_ROUND as u8, ne: CARD_ROUND as u8, sw: 0, se: 0,
        };
        ui.painter()
            .with_clip_rect(card_visible_clip)
            .rect_filled(top_band, cr, active_accent_fill(ui));
    }

    // Card-body click sensing. Register BEFORE any inner widgets so
    // later-added interactive widgets (Calibrate button, sliders,
    // DragValues) win the hit test on their own rects. If registered
    // last, this would swallow every click on a child widget.
    let body_resp = ui.interact(
        card_rect,
        ui.id().with(("easy_input_card", d.id.as_str())),
        egui::Sense::click(),
    );

    // ── Pass 2: render widgets inside the reserved rect ──
    let top_rect = egui::Rect::from_min_size(
        card_rect.min, egui::vec2(card_w, top_h));
    let bot_rect = egui::Rect::from_min_size(
        egui::pos2(card_rect.left(), card_rect.top() + top_h),
        egui::vec2(card_w, bot_h));

    // Card content horizontal padding — same value on left + right so
    // the icon, name/Calibrate column, and bottom slider rows all use
    // identical gutters.
    const CARD_HPAD: f32 = 10.0;

    // Top section: icon on the left, with a vertically-centered right
    // column carrying (name, Calibrate+Hz).
    let icon_x = top_rect.left() + CARD_HPAD;
    let icon_y = top_rect.top() + 4.0;
    let icon_w = INPUT_CARD_ICON_H;
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(icon_x, icon_y),
        egui::vec2(icon_w, INPUT_CARD_ICON_H),
    );
    let mut icon_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(icon_rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    icon_ui.set_clip_rect(card_visible_clip);
    // The device icon doubles as a "ping" button: clicking it pulses the
    // physical pad's rumble for 200 ms so the user can tell which hardware
    // this card maps to. Only meaningful for pads that have rumble motors.
    let resp = ping_device_icon(
        &mut icon_ui,
        remapper_icons::device_card_svg(d.kind),
        INPUT_CARD_ICON_H,
    );
    if resp.clicked() {
        if let Ok(mut q) = ping_requests.lock() {
            q.push(d.id.clone());
        }
    }

    // Right column: name (row 1) + Calibrate+Hz (row 2), y-centered
    // against the icon. ~36 px tall → indent top by (icon_h - 36) / 2.
    let right_col_h = 36.0_f32;
    let right_col_top = top_rect.top() + 4.0
        + (INPUT_CARD_ICON_H - right_col_h) * 0.5;
    let right_col_left = icon_rect.right() + 8.0;
    let right_col_rect = egui::Rect::from_min_max(
        egui::pos2(right_col_left, right_col_top),
        egui::pos2(card_rect.right() - CARD_HPAD, right_col_top + right_col_h),
    );
    let mut top_right = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(right_col_rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    top_right.set_clip_rect(card_visible_clip);
    top_right.label(egui::RichText::new(&d.display_name)
        .size(14.0).strong());
    top_right.add_space(1.0);
    top_right.horizontal(|ui| {
        if is_active {
            // This card's own node — see `source_node_for`.
            if let Some(node_id) = source_node_for(canvas, &d.id) {
                header_controls::render_calibrate_row(
                    ui, node_id, &d.id,
                    device_rates_hz, calibrate_request,
                );
            }
        } else {
            ui.add_enabled(false, egui::Button::new(
                egui::RichText::new("Calibrate").small()));
            ui.label(egui::RichText::new(format!("{} Hz",
                device_rates_hz.get(&d.id).copied().unwrap_or(0)))
                .color(egui::Color32::from_rgb(140, 110, 60)).small());
        }
        // UI-navigation toggle — accent-colored when on, matching the
        // active-card selection hue used for the nav glow. Lives at the
        // right edge of the header row on every card (any pad can drive
        // the UI, not just the active source).
        // Rasterize the controller-nav SVG once per (tint) and cache the texture
        // in ctx memory. Tinted white when ON (against the accent fill), gray
        // Render the SVG with its NATIVE colors (outlines/fills preserved) — a
        // fully-transparent tint (alpha 0) makes rasterize_svg_recolored skip its
        // recolor pass entirely. On/off state is conveyed by the button's fill +
        // stroke below, not by tinting the glyph.
        let tex: Option<egui::TextureHandle> = {
            let key = egui::Id::new("controller_nav_icon_native");
            let cached = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(key));
            cached.or_else(|| {
                const NAV_SVG: &str = include_str!("../../../../app/assets/controller_nav.svg");
                crate::canvas::viewer::rasterize_svg_recolored(
                    NAV_SVG, 32, 32, "override", egui::Color32::TRANSPARENT)
                    .map(|img| {
                        let t = ui.ctx().load_texture("controller_nav_icon", img,
                            egui::TextureOptions::LINEAR);
                        ui.ctx().data_mut(|d| d.insert_temp(key, t.clone()));
                        t
                    })
            })
        };
        let btn = match &tex {
            Some(t) => egui::Button::image(
                egui::Image::new((t.id(), egui::vec2(15.0, 15.0)))
                    // Dim the glyph when disabled so the grayed state reads.
                    .tint(if nav_disabled {
                        egui::Color32::from_gray(110)
                    } else {
                        egui::Color32::WHITE
                    })),
            None => egui::Button::new(egui::RichText::new("🎮").size(13.0)),
        };
        let nav_resp = ui.add_enabled(
            !nav_disabled,
            btn.fill(if nav_on {
                    ui.visuals().selection.bg_fill
                } else {
                    egui::Color32::TRANSPARENT
                })
                .stroke(if nav_on {
                    egui::Stroke::new(1.0, ui.visuals().selection.stroke.color)
                } else {
                    egui::Stroke::NONE
                }),
        ).on_hover_text(if nav_disabled {
            "UI navigation unavailable — this is FlexInput's own virtual output (shown as physical). Driving the UI from it would feed back into your own mappings."
        } else if nav_on {
            "UI navigation ON — this gamepad drives FlexInput's UI while focused (mapped output suppressed). Click to disable."
        } else {
            "UI navigation OFF — click to let this gamepad drive FlexInput's UI while focused."
        });
        if nav_resp.clicked() && !nav_disabled {
            *nav_toggle = Some(!nav_on);
        }
    });

    // Bottom section: two stacked slider rows with bigger, more
    // readable labels. Layout per row: [LABEL  | slider fills | value].
    let mut bot_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(bot_rect.shrink2(egui::vec2(CARD_HPAD, 4.0)))
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    bot_ui.set_clip_rect(card_visible_clip);
    if is_active {
        if let Some(node_id) = find_source_node_for(canvas, &d.id) {
            let dev_id_owned = d.id.clone();
            if let Some(params) = canvas.snarl.get_node_mut(node_id).map(|n| &mut n.params) {
                input_slider_rows(&mut bot_ui, params, &dev_id_owned, defaults, true);
                digital_trigger_toggle(&mut bot_ui, params, d.kind, true);
            }
        }
    } else {
        let mut preview: HashMap<String, Value> = HashMap::new();
        preview.insert("deadzone".into(),
            Value::from(defaults.stick_deadzone as f64));
        preview.insert("gyro_multiplier".into(),
            Value::from(defaults.gyro_mult as f64));
        input_slider_rows(&mut bot_ui, &mut preview, &d.id, defaults, false);
        digital_trigger_toggle(&mut bot_ui, &mut preview, d.kind, false);
    }

    // XInput physical input cards carry the player-slot circles too: a physical
    // Xbox can be forced onto a chosen slot (e.g. so a single-player game that
    // only reads slot 0 sees this controller). No-op for non-XInput pads.
    if device_id_is_xinput(&d.id) {
        bot_ui.add_space(3.0);
        bot_ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Player slot").size(11.0).weak());
            ui.add_space(4.0);
            canvas_node_xinput_slots(ui, &d.id);
        });
    }

    // ── Publish gamepad-nav targets for this card ──────────────────────
    // Card top-half = "select this input device". Active card also exposes
    // its sliders + digital-triggers checkbox as edit targets. Rects mirror
    // the layout `input_slider_rows` / `digital_trigger_toggle` produce so the
    // cursor hit-test lands where the user sees the control.
    {
        use crate::gamepad_nav::{LeftNavAction, LeftNavTarget};
        // Select target: the header band only (avoid covering the sliders).
        nav_targets.push(LeftNavTarget {
            rect: top_rect,
            action: LeftNavAction::SelectInput { device_id: d.id.clone() },
        });
        if is_active {
            if let Some(node_id) = find_source_node_for(canvas, &d.id) {
                use crate::canvas::viewer::device_source_caps;
                let (has_dz, has_gy, _) = device_source_caps(&d.id, true);
                // Bottom inner rect matches `bot_rect.shrink2((CARD_HPAD, 4.0))`.
                let inner = bot_rect.shrink2(egui::vec2(CARD_HPAD, 4.0));
                let row_h = 20.0_f32;
                let mut y = inner.top();
                if has_dz {
                    nav_targets.push(LeftNavTarget {
                        rect: egui::Rect::from_min_size(
                            egui::pos2(inner.left(), y), egui::vec2(inner.width(), row_h)),
                        action: LeftNavAction::AdjustParam {
                            node: node_id, key: "deadzone".into(),
                            lo: 0.0, hi: 0.5, step: 0.005,
                            default: defaults.stick_deadzone, log: false,
                        },
                    });
                    y += row_h;
                }
                if has_gy {
                    nav_targets.push(LeftNavTarget {
                        rect: egui::Rect::from_min_size(
                            egui::pos2(inner.left(), y), egui::vec2(inner.width(), row_h)),
                        action: LeftNavAction::AdjustParam {
                            node: node_id, key: "gyro_multiplier".into(),
                            lo: 0.1, hi: 50.0, step: 0.05,
                            default: defaults.gyro_mult, log: true,
                        },
                    });
                    y += row_h;
                }
                // Digital-triggers checkbox (only when the pad has analog
                // triggers — otherwise it's forced-on & disabled).
                if d.kind.has_analog_triggers() {
                    nav_targets.push(LeftNavTarget {
                        rect: egui::Rect::from_min_size(
                            egui::pos2(inner.left(), y + 2.0),
                            egui::vec2(inner.width(), row_h)),
                        action: LeftNavAction::ToggleParam {
                            node: node_id, key: "digital_triggers".into() },
                    });
                }
            }
        }
    }

    body_resp.clicked()
}

/// Easy-mode Deadzone / Gyro × slider rows. Bigger label font (size
/// 12, regular weight — not the tiny weak label `slider_label` uses
/// in Advanced mode) and the slider expands to fill the row's middle
/// space so it ends just before the value box near the card's right
/// edge.
fn input_slider_rows(
    ui: &mut egui::Ui,
    params: &mut HashMap<String, Value>,
    device_id: &str,
    defaults: DeviceParamDefaults,
    enabled: bool,
) {
    use crate::canvas::viewer::{device_source_caps, slider_track_double_clicked};
    let (has_dz, has_gy, _) = device_source_caps(device_id, true);
    let label_w = 64.0_f32;
    let dv_w    = 52.0_f32;
    let row_h   = 20.0_f32;
    // Pixel-positioned row: place label, slider, and value box into
    // explicit screen-coord sub-rects via `new_child` rather than
    // relying on `ui.horizontal`'s cursor + item_spacing math. egui
    // adds invisible internal margins to interactive widgets (drag-
    // value frame inset, slider handle clearance) that make
    // cursor-based layout overshoot. With explicit rects the value
    // box's right edge is guaranteed to land at row_right.
    let row = |ui: &mut egui::Ui, label: &str, val: &mut f32,
               range: std::ops::RangeInclusive<f32>, log: bool,
               default_val: f32, step: f32, decimals: usize|
               -> bool
    {
        let initial = *val;
        let row_avail = ui.available_width();
        let (row_rect, _) = ui.allocate_exact_size(
            egui::vec2(row_avail, row_h), egui::Sense::hover());
        let gap = 6.0_f32;
        let slider_w = (row_rect.width() - label_w - dv_w - 2.0 * gap).max(40.0);

        let label_rect = egui::Rect::from_min_size(
            row_rect.min, egui::vec2(label_w, row_h));
        let slider_rect = egui::Rect::from_min_size(
            egui::pos2(label_rect.right() + gap, row_rect.top()),
            egui::vec2(slider_w, row_h));
        let dv_rect = egui::Rect::from_min_size(
            egui::pos2(row_rect.right() - dv_w, row_rect.top()),
            egui::vec2(dv_w, row_h));

        // Label (painted, no widget — keeps left edge exact).
        ui.painter().text(
            egui::pos2(label_rect.left(), label_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.0),
            ui.visuals().text_color(),
        );

        // Slider — override spacing.slider_width so the track fills
        // the full middle slot (egui's default ~100 px leaves a gap).
        let mut slider_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(slider_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        slider_ui.spacing_mut().slider_width = slider_w;
        let mut slider = egui::Slider::new(val, range.clone())
            .show_value(false)
            .clamping(egui::SliderClamping::Always);
        if log { slider = slider.logarithmic(true); }
        let resp = slider_ui.add_sized([slider_w, row_h], slider);
        if slider_track_double_clicked(&slider_ui, &resp) { *val = default_val; }

        // Value box, pinned to row's right edge.
        let mut dv_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(dv_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        dv_ui.add_sized(
            [dv_w, row_h],
            egui::DragValue::new(val)
                .speed(step)
                .range(range.clone())
                .fixed_decimals(decimals),
        );

        (*val - initial).abs() > f32::EPSILON
    };

    ui.add_enabled_ui(enabled, |ui| {
        if has_dz {
            let mut dz = params.get("deadzone").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
            if row(ui, "Deadzone", &mut dz, 0.0..=0.5, false,
                   defaults.stick_deadzone, 0.005, 2)
            {
                params.insert("deadzone".into(), Value::from(dz as f64));
            }
        }
        if has_gy {
            let mut gm = params.get("gyro_multiplier").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            if row(ui, "Gyro ×", &mut gm, 0.1..=50.0, true,
                   defaults.gyro_mult, 0.05, 2)
            {
                params.insert("gyro_multiplier".into(), Value::from(gm as f64));
            }
        }
    });
}

/// Digital-trigger override toggle. When on, the virtual pad's analog triggers
/// are driven by the physical pad's digital ZL/ZR (a pressed button forces the
/// analog trigger to full; otherwise the real analog value passes through).
///
/// Switch Pro has no analog triggers, so the option is forced ON and the
/// checkbox is disabled there — it's the only way to get triggers from it.
/// Pads with real analog triggers default OFF and can opt in.
fn digital_trigger_toggle(
    ui: &mut egui::Ui,
    params: &mut HashMap<String, Value>,
    kind: ControllerKind,
    enabled: bool,
) {
    // Only gamepads have triggers; skip MIDI ports entirely.
    if matches!(kind, ControllerKind::MidiIn | ControllerKind::MidiOut) { return; }

    let forced = !kind.has_analog_triggers(); // Switch Pro / digital-only → forced on.
    let stored = params.get("digital_triggers").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut checked = forced || stored;

    // Keep the stored param in sync with the forced state so the engine sees
    // `digital_triggers = true` for Switch Pro even if the user never clicked.
    if forced && !stored {
        params.insert("digital_triggers".into(), Value::Bool(true));
    }

    ui.add_space(2.0);
    ui.add_enabled_ui(enabled && !forced, |ui| {
        let label = if forced {
            "Digital triggers (only option)"
        } else {
            "Digital triggers \u{2192} analog"
        };
        let resp = ui.checkbox(&mut checked, egui::RichText::new(label).size(11.0));
        if resp.changed() {
            params.insert("digital_triggers".into(), Value::Bool(checked));
        }
        resp.on_hover_text(
            "Make the analog triggers act digital: each snaps to a full pull once it \
             crosses the digital threshold set in Calibration (default: half pull), \
             and 0 below it. The LT/RT digital buttons follow the same threshold. \
             Output stays analog-typed, so it still drives analog wires at full/zero.",
        );
    });
}

fn find_source_node_for(canvas: &Canvas, device_id: &str) -> Option<NodeId> {
    canvas.snarl.nodes_ids_data()
        .find(|(_, n)| n.value.module_id == "device.source"
            && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(device_id))
        .map(|(id, _)| id)
}

/// How many input devices this preset accepts — one per AutoMap inlet the
/// subpatch declares.
///
/// Read from the preset rather than fixed, so a single-inlet preset keeps
/// behaving exactly as it always has and a multi-inlet one just works.
pub(super) fn input_capacity(canvas: &Canvas) -> usize {
    let sp = canvas.snarl.nodes_ids_data()
        .find(|(_, n)| n.value.module_id == "subpatch");
    match sp {
        Some((id, _)) => {
            super::wiring::automap_input_indices(canvas.snarl.get_node(id)).len().max(1)
        }
        None => 1,
    }
}

/// Active sources with their AutoMap port, lowest port first.
fn active_sources(canvas: &Canvas) -> Vec<(NodeId, String, usize)> {
    let mut v: Vec<(NodeId, String, usize)> = canvas.snarl.nodes_ids_data()
        .filter(|(_, n)| n.value.module_id == "device.source")
        .filter_map(|(id, n)| {
            let dev = n.value.params.get("device_id")?.as_str()?.to_string();
            let port = n.value.params.get("automap_port")
                .and_then(|p| p.as_u64()).unwrap_or(0) as usize;
            Some((id, dev, port))
        })
        .collect();
    v.sort_by_key(|(_, _, port)| *port);
    v
}

/// Add, remove or replace a source in response to a card click.
///
/// Rules, chosen so a single-inlet preset behaves EXACTLY as before:
/// * already active, capacity 1 → nothing (clicking the active card was always
///   a no-op, and turning it into a deselect would let a user end up with no
///   input device and no obvious way back)
/// * already active, capacity > 1 → deselect it
/// * not active, room left → add on the lowest free port
/// * not active, full → replace the highest port, which for capacity 1 is the
///   old "clicking another device swaps to it" behaviour
fn toggle_source(
    canvas: &mut Canvas,
    device: &PhysicalDevice,
    default_collapsed: bool,
    defaults: DeviceParamDefaults,
) {
    let capacity = input_capacity(canvas);
    let active = active_sources(canvas);

    if let Some((id, _, _)) = active.iter().find(|(_, d, _)| d == &device.id) {
        if capacity > 1 {
            canvas.snarl.remove_node(*id);
            super::layout::reposition_io_nodes(canvas);
        }
        return;
    }

    let port = if active.len() < capacity {
        // Lowest free port, so removing a middle device and adding another
        // reuses the gap instead of pushing past the inlet count.
        (0..capacity).find(|p| !active.iter().any(|(_, _, q)| q == p)).unwrap_or(0)
    } else {
        let (id, _, port) = active.last().map(|(a, _, c)| (*a, (), *c)).unwrap();
        canvas.snarl.remove_node(id);
        port
    };

    canvas.add_device_source(device, default_collapsed, defaults);
    if let Some(node_id) = find_source_node_for(canvas, &device.id) {
        if let Some(n) = canvas.snarl.get_node_mut(node_id) {
            n.params.insert("automap_port".into(), serde_json::Value::from(port as u64));
        }
    }
    super::layout::reposition_io_nodes(canvas);
}

/// The `device.source` node driving a SPECIFIC device.
///
/// ⛔ Not the same as [`active_source`], and using that here was a bug. It
/// returns the FIRST source node on the canvas, which was fine when Easy mode
/// held exactly one — but multi-device support made every card's Calibrate
/// button open whichever node happened to be first. With a stale Joy-Con node
/// still on the canvas, clicking Calibrate on a Pro Controller opened a
/// "Joy-Con 2 (L) Calibration" window for a device that was not even connected.
///
/// A per-card control has to resolve the card's OWN device.
fn source_node_for(canvas: &Canvas, dev_id: &str) -> Option<NodeId> {
    canvas.snarl.nodes_ids_data()
        .find(|(_, n)| {
            n.value.module_id == "device.source"
                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(dev_id)
        })
        .map(|(id, _)| id)
}



// ── Output ──────────────────────────────────────────────────────────

fn show_output_section(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    shared_pool: &SharedDevicePool,
    default_collapsed: bool,
    defaults: DeviceParamDefaults,
    nav_targets: &mut Vec<crate::gamepad_nav::LeftNavTarget>,
) {
    use crate::gamepad_nav::{LeftNavAction, LeftNavTarget};
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new("Choose output devices").size(15.0).strong());
    });
    ui.add_space(CARD_GAP);

    let keymouse_on = has_sink_of_kind(canvas, KIND_KEYMOUSE);

    // Single gamepad output card: a model selector (Xbox 360 / DS4 / DualSense /
    // None) plus the Rumble-range control for whichever pad is active.
    let panel_w = ui.available_width();
    let inner_w = (panel_w - 2.0 * PANEL_PADDING).max(120.0);
    // HIDMaestro backs every gamepad output. When its driver is absent we keep the
    // card ENABLED — selecting a model installs the driver on demand via the
    // elevated helper (one UAC) through the normal create path — and show a hint.
    let gamepad_ok = flexinput_virtual::driver_availability::hidmaestro_available();
    ui.horizontal(|ui| {
        ui.add_space(PANEL_PADDING);
        let sel_rect = gamepad_selector_card(
            ui, canvas, shared_pool, default_collapsed, defaults, false, inner_w);
        // Nav target cycles the selector to the NEXT model (wrapping through
        // None) — a single actionable rect for the RS/gyro cursor.
        nav_targets.push(LeftNavTarget { rect: sel_rect,
            action: LeftNavAction::CycleGamepadOutput });
        ui.add_space(PANEL_PADDING);
    });
    if !gamepad_ok {
        ui.horizontal(|ui| {
            ui.add_space(PANEL_PADDING + 4.0);
            ui.add(egui::Label::new(
                egui::RichText::new("Selecting a gamepad installs the HIDMaestro driver (one admin prompt).")
                    .small()
                    .color(egui::Color32::from_rgb(220, 160, 40)),
            ));
        });
    }

    ui.add_space(CARD_GAP);

    // Keyboard and Mouse card with Mouse speed slider inline.
    ui.horizontal(|ui| {
        ui.add_space(PANEL_PADDING);
        let (km_click, km_rect, ms_target) =
            keymouse_card(ui, keymouse_on, canvas, defaults, inner_w);
        if km_click {
            if keymouse_on {
                remove_sinks_of_kind(canvas, KIND_KEYMOUSE);
            } else {
                ensure_sink_of_kind(canvas, KIND_KEYMOUSE, shared_pool, default_collapsed, defaults);
            }
            super::wiring::rewire(canvas);
        }
        // Toggle target: the LEFT icon column (so it doesn't cover the slider).
        let icon_col_w = OUTPUT_CARD_ICON_H + 16.0;
        let toggle_rect = egui::Rect::from_min_size(
            km_rect.min, egui::vec2(icon_col_w, km_rect.height()));
        nav_targets.push(LeftNavTarget { rect: toggle_rect,
            action: LeftNavAction::ToggleOutput { kind: KIND_KEYMOUSE.into() } });
        if let Some((km_node, ms_rect)) = ms_target {
            nav_targets.push(LeftNavTarget { rect: ms_rect,
                action: LeftNavAction::AdjustParam {
                    node: km_node, key: "mouse_sensitivity".into(),
                    lo: 0.0, hi: 3000.0, step: 0.5,
                    default: defaults.mouse_sensitivity, log: false,
                } });
        }
        ui.add_space(PANEL_PADDING);
    });
}

/// The single gamepad output card: an icon, a model selector (None / Xbox 360 /
/// DualShock 4 / DualSense), and — when a pad is active — the Rumble-range
/// control for that pad. Picking a model deploys it and removes any other
/// gamepad sink (Easy mode drives one pad). Returns the card rect (for the nav
/// target). All gamepad models are HIDMaestro-backed; when the driver is absent
/// the selector is disabled and a corner warning badge explains why.
/// Four XInput **player-slot** circles, right-aligned so their right edge meets
/// `right_edge_x`. Three visual states per circle:
///   * **free** — hollow (no XInput device on that slot),
///   * **occupied by another device** — dark green fill,
///   * **this card's slot** (`this_slot`) — bright green fill + an outer glow ring.
/// Live occupancy comes from the throttled XInput probe. Returns `Some(slot)` if a
/// circle was clicked (a request to move this card to that player slot).
#[must_use]
fn xinput_slot_circles(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    center_y: f32,
    right_edge_x: f32,
    id_salt: &str,
    this_slot: Option<usize>,
) -> Option<usize> {
    let slots = flexinput_devices::probe_xinput_slots_cached();
    let dot_d = 14.0_f32;
    let gap = 5.0_f32;
    let total_w = 4.0 * dot_d + 3.0 * gap;
    let start_x = right_edge_x - total_w;
    let mut clicked: Option<usize> = None;
    for i in 0..4usize {
        let center = egui::pos2(start_x + i as f32 * (dot_d + gap) + dot_d / 2.0, center_y);
        let rect = egui::Rect::from_center_size(center, egui::vec2(dot_d, dot_d));
        let resp = ui.interact(rect, ui.id().with((id_salt, i)), egui::Sense::click());
        let occupied = slots[i].connected;
        let is_mine = this_slot == Some(i);

        // Glow ring for this card's own slot (drawn first, under the dot).
        if is_mine {
            painter.circle_filled(center, dot_d / 2.0 + 3.0, egui::Color32::from_rgba_unmultiplied(90, 230, 150, 60));
            painter.circle_filled(center, dot_d / 2.0 + 1.5, egui::Color32::from_rgba_unmultiplied(90, 230, 150, 90));
        }

        let (fill, base_stroke, text_c) = if is_mine {
            (egui::Color32::from_rgb(80, 200, 130), egui::Color32::from_rgb(170, 255, 200), egui::Color32::WHITE)
        } else if occupied {
            // Occupied by some OTHER XInput device — dark green.
            (egui::Color32::from_rgb(34, 74, 50), egui::Color32::from_rgb(70, 130, 95), egui::Color32::from_gray(210))
        } else {
            // Free slot — hollow.
            (egui::Color32::from_gray(38), egui::Color32::from_gray(85), egui::Color32::from_gray(135))
        };
        let (stroke_w, stroke_c) = if resp.hovered() {
            (2.0, egui::Color32::from_white_alpha(220))
        } else if is_mine {
            (1.5, base_stroke)
        } else {
            (1.0, base_stroke)
        };
        painter.circle(center, dot_d / 2.0, fill, egui::Stroke::new(stroke_w, stroke_c));
        painter.text(center, egui::Align2::CENTER_CENTER, format!("{}", i + 1),
            egui::FontId::proportional(9.0), text_c);

        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let state = if is_mine { "this device" } else if occupied { "in use by another device" } else { "free" };
        let resp = resp.on_hover_text(format!("Player slot {} — {state}\nClick to assign this card here", i + 1));
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// True if `device_id` names an XInput (Xbox) card — a physical Xbox source
/// (`gilrs:xinput:*`) or one of our Virtual Xbox sinks (`virtual.xinput*` /
/// `virtual.hm.xinput*`). Only these carry an XInput player slot.
pub fn device_id_is_xinput(device_id: &str) -> bool {
    // Physical xinput pad from either backend (gilrs:xinput:* / sdl:xinput:*).
    if let Some(slug) = crate::canvas::remapper_icons::phys_pad_slug(device_id) {
        return slug == "xinput";
    }
    device_id.starts_with("virtual.xinput") || device_id.starts_with("virtual.hm.xinput")
}

/// Best-effort "which slot is this XInput device on" for the glow indicator.
/// Priority:
///   1. an explicit assignment the user made via the reorder engine (trusted only
///      while that slot is still occupied), then
///   2. the single-occupied fallback — when exactly one XInput slot is filled it
///      must be this device (the common case: one Virtual Xbox or one physical
///      Xbox against non-XInput peers).
/// With several XInput devices present and no recorded assignment we return `None`
/// (occupancy still shows, just no "mine" glow), since XInput exposes only
/// slot→state and we can't passively correlate instance→slot.
fn this_device_slot(device_id: &str) -> Option<usize> {
    let slots = flexinput_devices::probe_xinput_slots_cached();
    if let Some(s) = assigned_xinput_slot(device_id) {
        if s < 4 && slots[s].connected {
            return Some(s);
        }
    }
    let connected: Vec<usize> = (0..4).filter(|&i| slots[i].connected).collect();
    if connected.len() == 1 { Some(connected[0]) } else { None }
}

/// Inline XInput player-slot circles for a canvas device node (physical Xbox
/// `device.source` or a Virtual Xbox `device.sink`). Allocates a short row at the
/// cursor and draws the four circles left-aligned. A click queues a slot request
/// keyed by `device_id` — the app drains it and routes to the reorder engine
/// (virtual → re-arrive our companion; physical → displacement reorder). No-op
/// for any non-XInput device id.
pub fn canvas_node_xinput_slots(ui: &mut egui::Ui, device_id: &str) {
    if !device_id_is_xinput(device_id) { return; }
    let this_slot = this_device_slot(device_id);
    let dot_d = 14.0_f32;
    let gap = 5.0_f32;
    let total_w = 4.0 * dot_d + 3.0 * gap;
    let (row_rect, _) =
        ui.allocate_exact_size(egui::vec2(total_w, dot_d + 4.0), egui::Sense::hover());
    let painter = ui.painter().clone();
    let salt = format!("xi_node_{device_id}");
    if let Some(slot) =
        xinput_slot_circles(ui, &painter, row_rect.center().y, row_rect.right(), &salt, this_slot)
    {
        request_xinput_slot(device_id, slot);
    }
}

/// A `label` row with the four slot circles right-aligned to the dropdown width
/// below it. Returns the clicked slot, if any.
#[must_use]
fn xinput_slot_row(
    ui: &mut egui::Ui,
    label: &str,
    dropdown_w: f32,
    id_salt: &str,
    this_slot: Option<usize>,
) -> Option<usize> {
    let dot_d = 14.0_f32;
    let (row_rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), dot_d + 2.0), egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.text(
        egui::pos2(row_rect.left(), row_rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        ui.visuals().strong_text_color(),
    );
    xinput_slot_circles(
        ui,
        &painter,
        row_rect.center().y,
        row_rect.left() + dropdown_w,
        id_salt,
        this_slot,
    )
}

fn gamepad_selector_card(
    ui: &mut egui::Ui,
    canvas: &mut Canvas,
    shared_pool: &SharedDevicePool,
    default_collapsed: bool,
    defaults: DeviceParamDefaults,
    driver_missing: bool,
    width: f32,
) -> egui::Rect {
    let active = active_gamepad_kind(canvas);
    let icon_col_w = OUTPUT_CARD_ICON_H + 16.0;
    let card_h = OUTPUT_GAMEPAD_CARD_H;

    let (rect, _resp) = ui.allocate_exact_size(egui::vec2(width, card_h), egui::Sense::hover());

    // ── bg + (if a pad is active) LEFT-half accent fill ──
    let is_active = active.is_some();
    let stroke_col = if is_active { ui.visuals().selection.stroke.color } else { CARD_STROKE_INACTIVE };
    let stroke_w = if is_active { 1.5 } else { 1.0 };
    ui.painter().rect(rect, CARD_ROUND, CARD_FILL_INACTIVE,
        egui::Stroke::new(stroke_w, stroke_col), egui::StrokeKind::Inside);
    if is_active {
        let left_rect = egui::Rect::from_min_size(rect.min, egui::vec2(icon_col_w, rect.height()));
        let mut cr = egui::CornerRadius::ZERO;
        cr.nw = CARD_ROUND as u8;
        cr.sw = CARD_ROUND as u8;
        ui.painter().with_clip_rect(rect).rect_filled(left_rect, cr, active_accent_fill(ui));
    }

    // Left column: icon for the active model (or the Xbox icon as a neutral
    // placeholder when nothing is deployed), vertically centered.
    let icon_kind = active.unwrap_or(KIND_XINPUT);
    let left_rect = egui::Rect::from_min_size(rect.min, egui::vec2(icon_col_w, rect.height()));
    let mut left_ui = ui.new_child(egui::UiBuilder::new()
        .max_rect(left_rect.shrink(6.0))
        .layout(egui::Layout::centered_and_justified(egui::Direction::TopDown)));
    if driver_missing { left_ui.disable(); }
    render_device_icon(&mut left_ui, remapper_icons::virtual_device_card_svg(icon_kind), OUTPUT_CARD_ICON_H);

    // Right column: title + selector row, then (if active) the rumble row.
    let right_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + icon_col_w, rect.top()),
        egui::vec2(rect.width() - icon_col_w, rect.height()),
    );
    let mut right_ui = ui.new_child(egui::UiBuilder::new()
        .max_rect(right_rect.shrink2(egui::vec2(8.0, 6.0)))
        .layout(egui::Layout::top_down(egui::Align::Min)));
    right_ui.scope(|ui| {
        if driver_missing { ui.disable(); }
        ui.add_space(2.0);
        // XInput player-slot circles apply only to an XInput (Virtual Xbox) output;
        // DualShock/DualSense virtuals have no XInput slot, so show a plain label.
        if active == Some(KIND_XINPUT) {
            let dropdown_w = (ui.available_width() - 4.0).clamp(120.0, 240.0);
            // "This device's slot" glow: keyed by the active Virtual Xbox sink's
            // device_id so a recorded assignment (set when the user clicks a slot)
            // wins; otherwise the single-occupied fallback applies. Keeping the key
            // identical to the app's dispatch resolution keeps the glow consistent
            // across the Easy card and the canvas node for the same device.
            let dev_id = sink_node_of_kind(canvas, KIND_XINPUT)
                .and_then(|n| canvas.snarl.get_node(n))
                .and_then(|n| n.params.get("device_id").and_then(|v| v.as_str()).map(str::to_string));
            let this_slot = this_device_slot(dev_id.as_deref().unwrap_or(""));
            if let Some(slot) = xinput_slot_row(ui, "Gamepad output", dropdown_w, "xi_slot_out", this_slot) {
                request_xinput_slot(XINPUT_CARD_OUTPUT, slot);
            }
        } else {
            ui.label(egui::RichText::new("Gamepad output").size(13.0).strong());
        }
        ui.add_space(4.0);

        // Model selector. The current selection is the active kind, or "None".
        let cur_label = active.map(gamepad_label).unwrap_or("None");
        let mut pick: Option<Option<&'static str>> = None; // outer Some = changed; inner = kind/None
        egui::ComboBox::from_id_salt("easy_gamepad_output_select")
            .selected_text(cur_label)
            .width((ui.available_width() - 4.0).clamp(120.0, 240.0))
            .show_ui(ui, |ui| {
                if ui.selectable_label(active.is_none(), "None").clicked() {
                    pick = Some(None);
                }
                for (kind, label) in GAMEPAD_KINDS {
                    if ui.selectable_label(active == Some(*kind), *label).clicked() {
                        pick = Some(Some(*kind));
                    }
                }
            });
        if let Some(choice) = pick {
            // Deploying any model replaces whatever gamepad is active; "None"
            // tears all of them down.
            remove_all_gamepad_sinks(canvas);
            if let Some(kind) = choice {
                ensure_sink_of_kind(canvas, kind, shared_pool, default_collapsed, defaults);
            }
            super::wiring::rewire(canvas);
        }

        // Rumble range — only when a pad is active (it acts on that sink node).
        if let Some(kind) = active_gamepad_kind(canvas) {
            if let Some(node) = sink_node_of_kind(canvas, kind) {
                ui.add_space(6.0);
                if let Some(params) = canvas.snarl.get_node_mut(node).map(|n| &mut n.params) {
                    header_controls::render_rumble_feedback_controls(ui, params, defaults);
                }
            }
        }
    });

    if driver_missing {
        // Corner warning badge (painted above the disabled body) explaining the
        // selector is unavailable. HIDMaestro has no external download — it's
        // installed by the bundled helper on first deploy — so it doesn't link.
        let _ = warning_badge(ui, rect);
    }
    rect
}

/// The HIDMaestro gamepad kind currently deployed as a sink (Easy mode keeps at
/// most one), or `None`. Checked in `GAMEPAD_KINDS` order.
fn active_gamepad_kind(canvas: &Canvas) -> Option<&'static str> {
    GAMEPAD_KINDS.iter().map(|(k, _)| *k).find(|k| has_sink_of_kind(canvas, k))
}

/// Remove every HIDMaestro gamepad sink (all models). Used when switching the
/// selector or tearing the gamepad output down.
pub fn remove_all_gamepad_sinks(canvas: &mut Canvas) {
    for (kind, _) in GAMEPAD_KINDS {
        remove_sinks_of_kind(canvas, kind);
    }
}

/// Next model for the gamepad-nav cycle: Xbox 360 → DS4 → DualSense → None → …
/// `None` means "deploy nothing" (the off state). Wraps from the last model
/// through None back to the first.
pub fn next_gamepad_kind(canvas: &Canvas) -> Option<&'static str> {
    match active_gamepad_kind(canvas) {
        None => Some(GAMEPAD_KINDS[0].0),
        Some(cur) => {
            let idx = GAMEPAD_KINDS.iter().position(|(k, _)| *k == cur).unwrap_or(0);
            GAMEPAD_KINDS.get(idx + 1).map(|(k, _)| *k) // past the last → None (off)
        }
    }
}

/// Paint a small amber ⚠ badge in the top-right corner of `card_rect` and
/// return its response (hover → tooltip explaining the disabled state). Kept out
/// of the card's flow layout so the warning can never reflow the icon/label.
fn warning_badge(ui: &mut egui::Ui, card_rect: egui::Rect) -> egui::Response {
    const BADGE: f32 = 22.0;
    let center = egui::pos2(card_rect.right() - 4.0 - BADGE * 0.5,
                            card_rect.top()   + 4.0 + BADGE * 0.5);
    let hit = egui::Rect::from_center_size(center, egui::vec2(BADGE, BADGE));
    let resp = ui.interact(hit, ui.id().with(("hm_warn", card_rect.left() as i32)),
        egui::Sense::hover());
    let amber = egui::Color32::from_rgb(220, 160, 40);
    let glyph_color = if resp.hovered() { egui::Color32::from_rgb(255, 200, 80) } else { amber };
    ui.painter().text(
        center,
        egui::Align2::CENTER_CENTER,
        "⚠",
        egui::FontId::proportional(BADGE * 0.8),
        glyph_color,
    );
    resp.on_hover_text("HIDMaestro driver unavailable (bundled helper not found)")
}

fn keymouse_card(
    ui: &mut egui::Ui,
    is_active: bool,
    canvas: &mut Canvas,
    defaults: DeviceParamDefaults,
    width: f32,
) -> (bool, egui::Rect, Option<(NodeId, egui::Rect)>) {
    // Card geometry — derived from content rather than a constant so
    // the three stacked rows on the right always fit without spill.
    //
    //   ┌─────────────────────────────────────────┐
    //   │ ICON   Keyboard and Mouse               │
    //   │ ICON   Mouse speed:   [   300  ]        │
    //   │ ICON   [══════════slider══════════]     │
    //   └─────────────────────────────────────────┘
    //
    // ICON column is the LEFT-half accent surface; content stacks in
    // the right column.
    let icon_col_w = OUTPUT_CARD_ICON_H + 16.0;
    let card_h = OUTPUT_KBM_CARD_H;

    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, card_h),
        egui::Sense::click(),
    );

    // ── Pass 1: bg + (if active) LEFT-half accent fill ──
    let stroke_col = if is_active {
        ui.visuals().selection.stroke.color
    } else {
        CARD_STROKE_INACTIVE
    };
    let stroke_w = if is_active { 1.5 } else { 1.0 };
    ui.painter().rect(rect, CARD_ROUND, CARD_FILL_INACTIVE,
        egui::Stroke::new(stroke_w, stroke_col), egui::StrokeKind::Inside);
    if is_active {
        let left_rect = egui::Rect::from_min_size(
            rect.min, egui::vec2(icon_col_w, rect.height()),
        );
        let mut cr = egui::CornerRadius::ZERO;
        cr.nw = CARD_ROUND as u8;
        cr.sw = CARD_ROUND as u8;
        ui.painter()
            .with_clip_rect(rect)
            .rect_filled(left_rect, cr, active_accent_fill(ui));
    }

    // ── Pass 2: widgets ──
    let left_rect = egui::Rect::from_min_size(
        rect.min, egui::vec2(icon_col_w, rect.height()),
    );
    let right_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + icon_col_w, rect.top()),
        egui::vec2(rect.width() - icon_col_w, rect.height()),
    );
    // Left column: icon vertically centered.
    let mut left_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(left_rect.shrink(6.0))
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    left_ui.set_clip_rect(rect);
    let icon_top_pad = ((left_rect.height() - OUTPUT_CARD_ICON_H) * 0.5 - 6.0).max(0.0);
    left_ui.add_space(icon_top_pad);
    render_device_icon(&mut left_ui, remapper_icons::keymouse_svg(), OUTPUT_CARD_ICON_H);

    // Right column: title (row 1), Mouse speed label + value (row 2),
    // slider full width (row 3). Inner padding on the right matches
    // the card's CARD_HPAD-equivalent (10 px) so the slider track and
    // value box end flush with the same gutter as the input cards.
    const RIGHT_HPAD: f32 = 8.0;
    const RIGHT_TAIL_PAD: f32 = 10.0;
    let right_inset = right_rect.shrink2(egui::vec2(0.0, 6.0));
    let right_inset = egui::Rect::from_min_max(
        egui::pos2(right_inset.left() + RIGHT_HPAD, right_inset.top()),
        egui::pos2(right_inset.right() - RIGHT_TAIL_PAD, right_inset.bottom()),
    );
    let mut right_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(right_inset)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    right_ui.set_clip_rect(rect);
    // Nudge the title slightly down so it sits clear of the card
    // top edge but doesn't dominate the card's vertical space.
    right_ui.add_space(3.0);
    right_ui.label(egui::RichText::new("Keyboard and Mouse").size(13.0).strong());
    right_ui.add_space(2.0);
    // Explicit content width for the right column so the value box +
    // slider know exactly how much space they have.
    let right_content_w = right_inset.width().max(40.0);
    let mut mouse_target: Option<(NodeId, egui::Rect)> = None;
    if is_active {
        if let Some(km_node) = sink_node_of_kind(canvas, KIND_KEYMOUSE) {
            if let Some(params) = canvas.snarl.get_node_mut(km_node).map(|n| &mut n.params) {
                mouse_speed_stack(&mut right_ui, params, defaults, right_content_w);
            }
            // Mouse-speed nav target: the value-row + slider span (lower ~36 px
            // of the right column), mirroring `mouse_speed_stack`'s two rows.
            let ms_rect = egui::Rect::from_min_max(
                egui::pos2(right_inset.left(), right_inset.top() + 22.0),
                egui::pos2(right_inset.right(), right_inset.bottom()),
            );
            mouse_target = Some((km_node, ms_rect));
        }
    } else {
        right_ui.add_enabled_ui(false, |ui| {
            let mut preview: HashMap<String, Value> = HashMap::new();
            preview.insert("mouse_sensitivity".into(),
                Value::from(defaults.mouse_sensitivity as f64));
            mouse_speed_stack(ui, &mut preview, defaults, right_content_w);
        });
    }
    (resp.clicked(), rect, mouse_target)
}

/// Stacked Mouse-speed control for the keymouse card:
///   row 1: "Mouse speed:" label + DragValue box
///   row 2: full-width slider
///
/// Splitting label/value from the slider on separate rows keeps the
/// slider track wide enough to be usable at narrow card widths and
/// matches the mockup's vertical rhythm.
fn mouse_speed_stack(
    ui: &mut egui::Ui,
    params: &mut HashMap<String, Value>,
    defaults: DeviceParamDefaults,
    content_w: f32,
) {
    let mut ms = params.get("mouse_sensitivity")
        .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let initial = ms;
    let dv_w = 56.0_f32;
    let row1_h = 20.0_f32;
    let row2_h = 16.0_f32;

    // Row 1: "Mouse speed:" label on the left, DragValue pinned flush
    // to the row's right edge via an explicit screen-coord sub-rect
    // (same technique as input_slider_rows — avoids egui internal
    // widget margins drifting the right edge).
    let (row1_rect, _) = ui.allocate_exact_size(
        egui::vec2(content_w, row1_h), egui::Sense::hover());
    ui.painter().text(
        egui::pos2(row1_rect.left(), row1_rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Mouse speed:",
        egui::FontId::proportional(12.0),
        ui.visuals().text_color(),
    );
    let dv_rect = egui::Rect::from_min_size(
        egui::pos2(row1_rect.right() - dv_w, row1_rect.top()),
        egui::vec2(dv_w, row1_h));
    let mut dv_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(dv_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    dv_ui.add_sized(
        [dv_w, row1_h],
        egui::DragValue::new(&mut ms)
            .speed(0.5)
            .range(0.0_f32..=3000.0)
            .fixed_decimals(2),
    );

    // Tighter gap between the value-row and the slider so the slider
    // sits closer to the row above (and the card stays compact).
    ui.add_space(0.0);

    // Row 2: slider pinned to the same content_w, so its right edge
    // matches the value box's right edge on row 1. Override
    // `spacing.slider_width` for this child UI — that's the value
    // egui's Slider uses as its preferred width and it defaults to
    // ~100 px, which would leave the track stopping mid-row even
    // though `add_sized` reserves the full width.
    let (row2_rect, _) = ui.allocate_exact_size(
        egui::vec2(content_w, row2_h), egui::Sense::hover());
    let mut slider_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row2_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    slider_ui.spacing_mut().slider_width = content_w;
    let resp = slider_ui.add_sized(
        [content_w, row2_h],
        egui::Slider::new(&mut ms, 0.0_f32..=3000.0)
            .show_value(false)
            .clamping(egui::SliderClamping::Always),
    );
    if resp.double_clicked() { ms = defaults.mouse_sensitivity; }

    if (ms - initial).abs() > f32::EPSILON {
        params.insert("mouse_sensitivity".into(), Value::from(ms as f64));
    }
}

// ── Sink helpers ────────────────────────────────────────────────────

fn has_sink_of_kind(canvas: &Canvas, kind_prefix_str: &str) -> bool {
    canvas.snarl.nodes_ids_data().any(|(_, n)| {
        n.value.module_id == "device.sink"
            && n.value.params.get("device_id")
                .and_then(|v| v.as_str())
                .map(|did| kind_prefix(did) == kind_prefix_str)
                .unwrap_or(false)
    })
}

fn sink_node_of_kind(canvas: &Canvas, kind_prefix_str: &str) -> Option<NodeId> {
    canvas.snarl.nodes_ids_data()
        .find(|(_, n)| {
            n.value.module_id == "device.sink"
                && n.value.params.get("device_id")
                    .and_then(|v| v.as_str())
                    .map(|did| kind_prefix(did) == kind_prefix_str)
                    .unwrap_or(false)
        })
        .map(|(id, _)| id)
}

fn remove_sinks_of_kind(canvas: &mut Canvas, kind_prefix_str: &str) {
    let to_remove: Vec<NodeId> = canvas.snarl.nodes_ids_data()
        .filter(|(_, n)| {
            n.value.module_id == "device.sink"
                && n.value.params.get("device_id")
                    .and_then(|v| v.as_str())
                    .map(|did| kind_prefix(did) == kind_prefix_str)
                    .unwrap_or(false)
        })
        .map(|(id, _)| id)
        .collect();
    for id in to_remove { canvas.snarl.remove_node(id); }
}

fn ensure_sink_of_kind(
    canvas: &mut Canvas,
    kind_id: &str,
    pool: &SharedDevicePool,
    default_collapsed: bool,
    defaults: DeviceParamDefaults,
) {
    if has_sink_of_kind(canvas, kind_id) { return; }
    let existing: Option<String> = {
        let devs = pool.lock().unwrap();
        devs.iter()
            .find(|d| d.id().starts_with(kind_id))
            .map(|d| d.id().to_string())
    };
    if existing.is_some() {
        let devs = pool.lock().unwrap();
        if let Some(d) = devs.iter().find(|d| d.id().starts_with(kind_id)) {
            canvas.add_virtual_sink(d.as_ref(), default_collapsed, defaults);
        }
    } else {
        let dev = create_device(kind_id, 0);
        canvas.add_virtual_sink(dev.as_ref(), default_collapsed, defaults);
        let mut devs = pool.lock().unwrap();
        devs.push(dev);
    }
    super::layout::reposition_io_nodes(canvas);
}

/// Public entry for gamepad-nav: add (or reuse) a virtual sink of `kind_id`.
/// Mirrors the io_panel card-click path so output toggles behave identically.
pub fn nav_ensure_sink(
    canvas: &mut Canvas,
    kind_id: &str,
    pool: &SharedDevicePool,
    default_collapsed: bool,
    defaults: DeviceParamDefaults,
) {
    ensure_sink_of_kind(canvas, kind_id, pool, default_collapsed, defaults);
}
