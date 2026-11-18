//! Virtual Menu module body — Touch Zones, re-based for a screen menu.
//!
//! Per user direction this is the Touch Zones module reused wholesale: the
//! menu node carries the SAME params (`zone_mode`, `col_edges`/`row_edges`,
//! `zone_tree`, `zone_maps`, `hold_zones`, `sel_field`/`sel_zone`,
//! `field_w`/`field_h`, `_tz_*` learn state) and renders through the same
//! node-generic functions (`render_touch_field`, `render_touch_zone_cards`,
//! `tz_render_merge_popup`) — so dividers, partial dividers, mapping cards,
//! curves, and hold zones behave identically. Only single-field (no split
//! pads; that slot holds the menu options and, later, the RADIAL toggle).
//!
//! The difference is the DRIVER: instead of the upstream device's touchpad,
//! the menu's pointer comes from a chosen analog input of the upstream device
//! (`pointer_source`: LS / RS / touch 1 / touch 2) or the wired Pointer Vec2
//! inlet, and its Show/Select/Next/Previous controls are macro-style named
//! targets (menu:{menu_id}_show / _sel / _next / _prev — assignable from
//! Remapper / Touch Zones / Lean via the Special picker, where the menu wears
//! its name + icon, badged by role) plus the wired Show/Select pins as
//! alternates. It renders on its own screen
//! overlay while open. The zone-live highlight here comes from the node's own
//! eval mirror (Open/Hover outputs), not from touch pins.
//!
//! Menu-specific params (on top of the TZ set):
//!   menu_id — stable identity behind the target pin ids (save-format)
//!   menu_name, menu_icon, menu_icon_svg — identity (Macro pattern)
//!   activation_mode: "hold" | "toggle" | "touch" | "never"
//!   pointer_source: "left_stick" | "right_stick" | "touch1" | "touch2"
//!   select_on: "release" | "press" | "click"
//!   pointer_deadzone: f32
//!   suppress_while_open: bool
//!   menu_rect: [x, y, w, h] — monitor-fraction placement (edit mode, later)

use std::collections::HashMap;

use egui_snarl::{NodeId, Snarl};
use flexinput_core::menu as fm;
use flexinput_core::touchzones as tz;
use flexinput_core::{PinDescriptor, Signal, SignalType};

use super::viewer::{
    register_exposable_element, render_touch_field, render_touch_zone_cards,
    tz_render_merge_popup, tz_zone_live, AutomapGlowParent,
};
use super::NodeData;

type LiveSignals = HashMap<(String, String), Signal>;

fn pstr<'a>(node: &'a NodeData, key: &str, default: &'a str) -> &'a str {
    node.params.get(key).and_then(|v| v.as_str()).unwrap_or(default)
}

fn pf32(node: &NodeData, key: &str, default: f32) -> f32 {
    node.params.get(key).and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(default)
}

fn pbool(node: &NodeData, key: &str, default: bool) -> bool {
    node.params.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// Menu / Touch-Zones theme defaults, each RGB + a repurposed alpha byte:
/// `main`'s alpha is the whole menu's OPACITY (its RGB tints the borders bright
/// and the plate dark, together); `highlight`'s alpha is its BLEND — 255 fully
/// overrides the hovered/selected zone, lower fades it toward the main colour.
pub(crate) const MENU_MAIN_DEFAULT: [u8; 4] = [96, 120, 156, 217];
pub(crate) const MENU_HIGHLIGHT_DEFAULT: [u8; 4] = [255, 196, 90, 255];

/// Read an RGBA colour param (3 or 4 components), falling back to `default`.
/// Raw `[r, g, b, a]` colour param parse — straight (unmultiplied) alpha,
/// exactly as stored. THE form to use whenever the bytes feed a colour swatch
/// or `ZoneColors::build`: a `Color32` is PREMULTIPLIED internally, so
/// unpacking one via `.r()/.g()/.b()/.a()` hands back `rgb × alpha` — the
/// picker then sees pre-darkened bytes, brightness clamps to the alpha value,
/// and every hue/sat edit bakes the darkening into the param.
pub(crate) fn pcolor_bytes(node: &NodeData, key: &str, default: [u8; 4]) -> [u8; 4] {
    node.params.get(key).and_then(|v| v.as_array())
        .and_then(|a| {
            if a.len() >= 3 {
                Some([
                    a[0].as_u64()? as u8,
                    a[1].as_u64()? as u8,
                    a[2].as_u64()? as u8,
                    a.get(3).and_then(|v| v.as_u64()).unwrap_or(255) as u8,
                ])
            } else { None }
        })
        .unwrap_or(default)
}

/// [`pcolor_bytes`] as a ready-to-PAINT `Color32`. Paint-only: do not unpack
/// its components back out (see `pcolor_bytes` on why).
pub(crate) fn pcolor(node: &NodeData, key: &str, default: [u8; 4]) -> egui::Color32 {
    let c = pcolor_bytes(node, key, default);
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

/// Colours driving the zone painters, derived from `main_color` (RGB+opacity)
/// and `highlight_color` (RGB+blend):
///   `main`   — idle structure (borders/hub/plate) at the menu's opacity.
///   `hi`     — hover/select colour: the highlight RGB blended toward `main` by
///              (1 − blend), at the menu's opacity, so it fades with the menu.
///   `accent` — the pure highlight RGB, fully opaque, for editor affordances
///              (border grips, ± buttons) that must stay crisp regardless.
#[derive(Clone, Copy)]
pub(crate) struct ZoneColors {
    pub main: egui::Color32,
    pub hi: egui::Color32,
    pub accent: egui::Color32,
}

impl ZoneColors {
    pub(crate) fn build(main_c: [u8; 4], hi_c: [u8; 4]) -> Self {
        let opacity = main_c[3];
        let blend = hi_c[3] as f32 / 255.0;
        let lerp8 = |a: u8, b: u8| {
            (a as f32 + (b as f32 - a as f32) * blend).round().clamp(0.0, 255.0) as u8
        };
        Self {
            main: egui::Color32::from_rgba_unmultiplied(main_c[0], main_c[1], main_c[2], opacity),
            hi: egui::Color32::from_rgba_unmultiplied(
                lerp8(main_c[0], hi_c[0]), lerp8(main_c[1], hi_c[1]), lerp8(main_c[2], hi_c[2]),
                opacity),
            accent: egui::Color32::from_rgb(hi_c[0], hi_c[1], hi_c[2]),
        }
    }
    /// Read the module's `main_color` / `highlight_color` params. Raw bytes —
    /// routing through a `Color32` premultiplied the rgb by alpha before
    /// `build` applied the opacity AGAIN, rendering the menu darker than the
    /// picked colour whenever opacity < 100%.
    pub(crate) fn read(node: &NodeData) -> Self {
        Self::build(
            pcolor_bytes(node, "main_color", MENU_MAIN_DEFAULT),
            pcolor_bytes(node, "highlight_color", MENU_HIGHLIGHT_DEFAULT),
        )
    }
    pub(crate) fn fallback() -> Self {
        Self::build(MENU_MAIN_DEFAULT, MENU_HIGHLIGHT_DEFAULT)
    }
}

/// Two colour swatches (Main additive + Highlight) shared by the Virtual Menu
/// and Touch Zones HEADERS. Compact and label-free — each swatch names itself
/// on hover — so the picker opens away from the zone field it recolours.
/// Adds itself to the CURRENT row; callers wrap it in their header layout.
/// Writes `main_color` / `highlight_color`.
pub(crate) fn show_zone_color_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    // RAW stored bytes — extracting them from a `Color32` (`pcolor(..).r()` …)
    // premultiplies rgb by alpha, so the picker read back a pre-darkened colour
    // every frame: brightness clamped to the alpha value, the marker snapped
    // dark on release, and hue/sat edits baked the darkening into the param.
    let (mut main, mut hi) = {
        let Some(node) = snarl.get_node(node_id) else { return };
        (
            pcolor_bytes(node, "main_color", MENU_MAIN_DEFAULT),
            pcolor_bytes(node, "highlight_color", MENU_HIGHLIGHT_DEFAULT),
        )
    };
    let mut changed: Vec<(&str, [u8; 4])> = Vec::new();

    // Full colour pickers (alpha bar included). Each swatch's alpha carries
    // meaning: MAIN's alpha = whole-menu opacity; HIGHLIGHT's alpha = blend
    // (opaque overrides the zone, lower blends toward main).
    if super::viewer::fxi_color_swatch(ui, &mut main,
        "Main colour — tints the zone borders (bright) and plate (dark) together.\nIts ALPHA = whole-menu opacity (see-through over a game).", true)
    {
        changed.push(("main_color", main));
    }
    if super::viewer::fxi_color_swatch(ui, &mut hi,
        "Highlight colour — the hovered / selected zone.\nIts ALPHA = blend: opaque fully overrides the zone, lower blends toward the main colour.", true)
    {
        changed.push(("highlight_color", hi));
    }
    if !changed.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, c) in changed {
                node.params.insert(k.into(), serde_json::json!([c[0], c[1], c[2], c[3]]));
            }
        }
    }
}

/// Dark zone plate tinted by the main colour, at the menu's opacity (`main`'s
/// alpha). The main RGB tints a near-black base a fixed amount (borders read
/// brighter because they use the main RGB directly); the alpha carries opacity,
/// so a translucent main colour makes the whole menu see-through over a game.
pub(crate) fn plate_fill(main: egui::Color32) -> egui::Color32 {
    let mix = |base: f32, m: u8| (base + m as f32 * 0.22).min(255.0) as u8;
    egui::Color32::from_rgba_unmultiplied(
        mix(16.0, main.r()), mix(16.0, main.g()), mix(20.0, main.b()), main.a())
}

// ── Ports ─────────────────────────────────────────────────────────────────────

/// Derive `output_pin_ids` + dynamic `node.outputs` from the zone tree.
/// Slots 0–2 are the fixed descriptor outputs (AutoMap pass, Open, Hover).
/// Ports mode appends the SAME per-zone vocabulary as Touch Zones — X / Y
/// (local pointer coords) and Active (pointer in zone while open) per zone,
/// plus one Select pin (the TZ "click" slot) — so `touchzones::parse_pin`
/// serves both modules in eval. Idempotent.
pub(crate) fn regenerate_menu_ports(node_id: NodeId, snarl: &mut Snarl<NodeData>) {
    let tree = super::viewer::tz_field_tree(snarl, node_id, 0);
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let mut want: Vec<String> = vec![
        fm::PASS_PIN.to_string(), fm::OPEN_PIN.to_string(), fm::HOVER_PIN.to_string(),
    ];
    let ports_mode = pstr(node, "zone_mode", "mapping") == "ports";
    if ports_mode {
        for id in tree.ids() {
            for comp in [tz::ZoneComp::X, tz::ZoneComp::Y, tz::ZoneComp::Active] {
                want.push(tz::zone_pin_id(0, id as usize, comp));
            }
        }
        want.push(tz::click_pin_id(0));
    }
    let cur: Vec<String> = node.params.get("output_pin_ids").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    if cur == want { return; }

    node.outputs.truncate(3);
    if ports_mode {
        for id in tree.ids() {
            for comp in [tz::ZoneComp::X, tz::ZoneComp::Y, tz::ZoneComp::Active] {
                let ty = match comp {
                    tz::ZoneComp::Active => SignalType::Bool,
                    _ => SignalType::Float,
                };
                node.outputs.push(PinDescriptor::new(
                    tz::zone_pin_label(0, id as usize, comp, true), ty));
            }
        }
        node.outputs.push(PinDescriptor::new("Select", SignalType::Bool));
    }
    node.params.insert(
        "output_pin_ids".into(),
        serde_json::Value::Array(want.into_iter().map(serde_json::Value::String).collect()),
    );
}

// ── Live state (from the node's own eval mirror) ─────────────────────────────

/// Zone-live map for the menu: ports mode reconstructs from the node's own
/// zone outputs exactly like Touch Zones; mapping mode (no zone ports) marks
/// the hovered zone from the fixed Open/Hover outputs.
/// The menu's live pointer (unit-rect 0..1, y down) from the eval mirror —
/// the engine appends it as one extra trailing slot beyond the real ports,
/// `None` while the menu is closed or the pointer rests in the deadzone.
pub(crate) fn menu_pointer(node: &NodeData) -> Option<egui::Vec2> {
    match node.extra.last_out.last()? {
        Some(flexinput_core::Signal::Vec2(v)) => Some(egui::vec2(v.x, v.y)),
        _ => None,
    }
}

/// Per-zone display overrides from the `zone_meta` param. An icon override
/// REPLACES the zone's mapping-destination icons everywhere the zone is
/// painted; the label draws alongside whatever icons show.
#[derive(Clone, Default)]
pub(crate) struct ZoneMeta {
    /// Shared icon-set key ("" = none).
    pub icon: String,
    /// Custom SVG embedded into the patch ("" = none).
    pub svg: String,
    /// Free-text zone name ("" = none).
    pub label: String,
}

pub(crate) fn menu_zone_meta(node: &NodeData) -> HashMap<u32, ZoneMeta> {
    let mut out = HashMap::new();
    if let Some(obj) = node.params.get("zone_meta").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            let Ok(id) = k.parse::<u32>() else { continue };
            let s = |key: &str| v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let m = ZoneMeta { icon: s("icon"), svg: s("svg"), label: s("label") };
            if !m.icon.is_empty() || !m.svg.is_empty() || !m.label.is_empty() {
                out.insert(id, m);
            }
        }
    }
    out
}

/// Write (or clear) one zone's `zone_meta` entry, dropping the param entirely
/// when the last entry goes away. Shared with the Touch Zones per-zone icon row.
pub(crate) fn write_zone_meta_entry(node: &mut NodeData, zone: u32, meta: &ZoneMeta) {
    let mut obj = node.params.get("zone_meta")
        .and_then(|v| v.as_object()).cloned()
        .unwrap_or_default();
    if meta.icon.is_empty() && meta.svg.is_empty() && meta.label.is_empty() {
        obj.remove(&zone.to_string());
    } else {
        obj.insert(zone.to_string(), serde_json::json!({
            "icon": meta.icon, "svg": meta.svg, "label": meta.label,
        }));
    }
    if obj.is_empty() {
        node.params.remove("zone_meta");
    } else {
        node.params.insert("zone_meta".into(), serde_json::Value::Object(obj));
    }
}

/// The menu's last accepted selection from the eval mirror — `(zone id,
/// selection seq)` at the SECOND-to-last slot (the pointer sits last; see
/// `eval_menu_node`'s trailing-slot comment). `None` before any selection.
/// The seq increments on each selection, so the overlay's linger animation
/// triggers on change rather than on a short pulse it could miss.
pub(crate) fn menu_sel_info(node: &NodeData) -> Option<(u32, u32)> {
    let lo = &node.extra.last_out;
    if lo.len() < 2 { return None; }
    match lo.get(lo.len() - 2)? {
        Some(flexinput_core::Signal::Vec2(v)) if v.x >= 0.0 && v.y > 0.0 => {
            Some((v.x as u32, v.y as u32))
        }
        _ => None,
    }
}

/// Cursor-deflection indicator: a soft line from the field's origin to the
/// pointer position with a dot at the tip. Deliberately translucent so zone
/// icons stay readable when the cursor crosses them.
pub(crate) fn paint_menu_cursor(
    p: &egui::Painter,
    origin: egui::Pos2,
    pos: egui::Pos2,
    accent: egui::Color32,
) {
    p.line_segment([origin, pos], egui::Stroke::new(2.0_f32, accent.gamma_multiply(0.35)));
    p.circle_filled(pos, 6.0, accent.gamma_multiply(0.45));
    p.circle_filled(pos, 2.5, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200));
}

pub(crate) fn menu_zone_live(node: &NodeData) -> HashMap<(usize, usize), (f32, f32, bool)> {
    let ports = node.params.get("output_pin_ids").and_then(|v| v.as_array())
        .map(|a| a.len() > 3).unwrap_or(false);
    if ports {
        return tz_zone_live(node);
    }
    let mut out = HashMap::new();
    let open = node.extra.last_out.get(1).and_then(|s| *s).map(|s| s.as_bool()).unwrap_or(false);
    let hover = node.extra.last_out.get(2).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(-1.0);
    if open && hover >= 0.0 {
        out.insert((0usize, hover as usize), (0.5f32, 0.5f32, true));
    }
    out
}

// ── Body ──────────────────────────────────────────────────────────────────────

/// The node body: the field is what the module's corner grip sizes (`kit::Surface`) - the
/// frame it was dragged to, less the rows around the field. It had a grip of its own in its
/// corner and a size of its own in the patch.
/// The narrowest the body's rows read at: the widest of their groups.
const MENU_ROWS_MIN_W: f32 = 300.0;

pub(crate) fn show_menu_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &LiveSignals,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    body_w: f32,
    body_h: f32,
) {
    let mut surface = super::viewer::kit::Surface::begin(ui, node_id, body_w, body_h, egui::vec2(160.0, 100.0));
    surface.need(MENU_ROWS_MIN_W * super::viewer::simple_bodies::control_width_factor());
    menu_body(node_id, ui, snarl, live_signals, automap_parent, surface.size);
    surface.end(ui);
}

fn menu_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &LiveSignals,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    field: egui::Vec2,
) {
    // Lazy default init: single-field 2×2 grid, MAPPING mode (the menu's
    // headline use — zones firing outputs without wiring).
    if let Some(node) = snarl.get_node_mut(node_id) {
        // Stable identity behind the macro-style target pins
        // (menu:{id}_show/_sel) — generated once, survives rename.
        if !node.params.contains_key("menu_id") {
            node.params.insert(
                "menu_id".to_string(),
                serde_json::json!(flexinput_core::macros::new_port_id()),
            );
        }
        if !node.params.contains_key("col_edges") {
            node.params.insert("zone_mode".to_string(), serde_json::json!("mapping"));
            node.params.insert("field_mode".to_string(), serde_json::json!("single"));
            node.params.insert("col_edges".to_string(), serde_json::json!([0.5]));
            node.params.insert("row_edges".to_string(), serde_json::json!([0.5]));
            // Menu pads default smaller than a touch surface.
            node.params.insert("field_w".to_string(), serde_json::json!(300.0));
            node.params.insert("field_h".to_string(), serde_json::json!(180.0));
        }
    }
    regenerate_menu_ports(node_id, snarl);

    let visuals = ui.visuals().clone();
    // Configurable theme: the zone painters take the full `colors`; the grid
    // field editor + cards take the crisp opaque `accent` for their affordances.
    let colors = snarl.get_node(node_id).map(ZoneColors::read).unwrap_or_else(ZoneColors::fallback);
    let accent = colors.accent;

    let zone_live = snarl.get_node(node_id).map(menu_zone_live).unwrap_or_default();

    let (field_w, field_h) = (field.x, field.y);

    let rows_w = field_w.max(MENU_ROWS_MIN_W * super::viewer::simple_bodies::control_width_factor());
    ui.vertical(|ui| {
        // Bounded to the frame: a heading takes the width it is offered, and a body is
        // offered last frame's - a frame dragged wide would never come back.
        ui.set_max_width(rows_w);
        ui.spacing_mut().item_spacing.y = 2.0;
        // ── What the menu is (its identity is in the header), then how it is pointed at
        //    and picked from - the pinnable "options" element - in categories that fold,
        //    as the other modules' bodies have their settings ──
        if super::viewer::curve_rows::section(ui, snarl, node_id, "menu", "Menu", true) {
            menu_mode_rows(node_id, ui, snarl, automap_parent, rows_w);
        }
        if super::viewer::curve_rows::section(ui, snarl, node_id, "pointer", "Pointer and selection", true) {
            let opts_area = ui.vertical(|ui| {
                menu_options_pills(node_id, ui, snarl, rows_w);
            }).response.rect;
            register_exposable_element(ui, node_id, "options", opts_area);
        }
        ui.add_space(4.0);

        // Re-read after header toggles so this frame renders consistently.
        let mapping = snarl.get_node(node_id)
            .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");

        // ── The zone field: TZ machinery for the grid, sector ring for radial ──
        let radial = snarl.get_node(node_id).map(|n| pbool(n, "menu_radial", false)).unwrap_or(false);
        let field_area = if radial {
            render_radial_field(
                node_id, mapping, ui, snarl, &zone_live, colors, field_w, field_h,
            )
        } else {
            render_touch_field(
                node_id, 0, true, false, mapping, ui, snarl,
                &zone_live, &visuals, accent, field_w, field_h,
                super::viewer::remapper_out_skin(snarl, node_id, automap_parent),
            )
        };
        register_exposable_element(ui, node_id, "field", field_area);
        // Zone-name labels over the grid field (the radial ring paints its
        // own; the grid field's TZ painter predates `zone_meta`). Drawn
        // before the cursor so the indicator stays on top.
        if !radial {
            let metas = snarl.get_node(node_id).map(menu_zone_meta).unwrap_or_default();
            if !metas.is_empty() {
                let tree = super::viewer::tz_field_tree(snarl, node_id, 0);
                let p = ui.painter_at(field_area);
                for (zid, [x0, _y0, x1, y1]) in tree.zones() {
                    let Some(m) = metas.get(&zid) else { continue };
                    if m.label.is_empty() { continue; }
                    let cx = field_area.left() + (x0 + x1) * 0.5 * field_area.width();
                    let by = field_area.top() + y1 * field_area.height();
                    let (f, txt) = fit_zone_label(
                        &p, &m.label, 11.0, (x1 - x0) * field_area.width() - 6.0);
                    p.text(egui::pos2(cx, by - 3.0), egui::Align2::CENTER_BOTTOM, txt,
                        f, crate::settings::tinted_gray(210));
                }
            }
        }
        // Cursor-deflection indicator over the grid field (the radial field
        // paints its own inside paint_radial_ring).
        if !radial {
            if let Some(c) = snarl.get_node(node_id).and_then(menu_pointer) {
                let pos = egui::pos2(
                    field_area.left() + c.x.clamp(0.0, 1.0) * field_area.width(),
                    field_area.top() + c.y.clamp(0.0, 1.0) * field_area.height(),
                );
                paint_menu_cursor(&ui.painter_at(field_area), field_area.center(), pos, accent);
            }
        }

        if mapping && !radial { tz_render_merge_popup(ui, snarl, node_id); }

        // ── Per-zone icon + name override: the icon replaces the zone's mapping icons
        // everywhere it's painted (field, pinned widget, menu overlay) — keyboard bindings
        // rarely read as "what this zone does"; the name draws alongside whatever icons
        // show. The Touch Zones row: the menu's one field is its field 0. ──
        super::viewer::tz_zone_icon_row(node_id, ui, snarl, rows_w);

        // ── Mapping mode: zone-tab card list (TZ machinery, separately pinnable) ──
        if mapping {
            ui.add_space(6.0);
            // The zone tabs and Learn row stay put, the cards scroll in a window under them
            // (`card_window`), as a Remapper's do.
            let band = super::viewer::curve_rows::band_h(ui);
            let cards_area = super::viewer::card_window(
                ui, snarl, node_id, "menu_cards", rows_w, "cards_h", band,
                live_signals, &crate::app::PanicShortcut::default(), automap_parent,
                |id, _ins, ui, sn, sigs, _panic, am| {
                    let visuals = ui.visuals().clone();
                    let accent = visuals.selection.bg_fill;
                    ui.vertical(|ui| {
                        render_touch_zone_cards(id, ui, sn, &visuals, accent, sigs, am);
                    });
                },
                super::viewer::tz_cards_hash_map,
                super::viewer::tz_cards_hash_draft,
            );
            register_exposable_element(ui, node_id, "cards", cards_area);
        }

    });
}

// ── Radial field ──────────────────────────────────────────────────────────────

/// Tessellated donut-sector shapes (fill mesh + closed outline). `a0`/`a1` are
/// clockwise-from-top radians (`core::menu::radial_sector_arc`); direction of
/// angle θ on screen is `(sin θ, −cos θ)`. Shared by the node body and the
/// menu overlay so both paint identical geometry.
pub(crate) fn radial_sector_shapes(
    center: egui::Pos2,
    r_in: f32,
    r_out: f32,
    a0: f32,
    a1: f32,
    fill: egui::Color32,
    stroke: egui::Stroke,
) -> Vec<egui::Shape> {
    let dir = |a: f32| egui::vec2(a.sin(), -a.cos());
    let steps = (((a1 - a0).abs() / 0.15).ceil() as usize).max(2);
    let mut outer = Vec::with_capacity(steps + 1);
    let mut inner = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let a = a0 + (a1 - a0) * (i as f32 / steps as f32);
        outer.push(center + dir(a) * r_out);
        inner.push(center + dir(a) * r_in);
    }
    let mut shapes = Vec::new();
    if fill.a() > 0 {
        let mut mesh = egui::Mesh::default();
        for i in 0..=steps {
            mesh.colored_vertex(inner[i], fill);
            mesh.colored_vertex(outer[i], fill);
        }
        for i in 0..steps {
            let b = (i * 2) as u32;
            mesh.add_triangle(b, b + 1, b + 2);
            mesh.add_triangle(b + 1, b + 3, b + 2);
        }
        shapes.push(egui::Shape::mesh(mesh));
    }
    if stroke.width > 0.0 {
        if (a1 - a0).abs() >= std::f32::consts::TAU - 1e-3 {
            // A whole ring has no sides: its two circles alone, no seam.
            outer.pop();
            shapes.push(egui::Shape::closed_line(outer, stroke));
            if r_in > 2.5 {
                inner.pop();
                shapes.push(egui::Shape::closed_line(inner, stroke));
            }
        } else {
            let mut pts = outer;
            inner.reverse();
            pts.extend(inner);
            shapes.push(egui::Shape::closed_line(pts, stroke));
        }
    }
    shapes
}

/// Screen-space unit direction for an angle fraction `u` (0 = 12 o'clock,
/// clockwise).
pub(crate) fn polar_dir(u: f32) -> egui::Vec2 {
    let a = u * std::f32::consts::TAU;
    egui::vec2(a.sin(), -a.cos())
}

/// Screen geometry of a sector ring: center + hub/rim radii. Maps the zone
/// tree's unit space (x = angle fraction clockwise from 12 o'clock, y = radius
/// fraction hub→rim) to screen points and back — the single source of truth
/// shared by the painter, the border editor, and hit-testing.
#[derive(Clone, Copy)]
pub(crate) struct RingGeom {
    pub center: egui::Pos2,
    pub r_in: f32,
    pub r_out: f32,
    /// Angular origin offset (fraction, clockwise). Tree-u 0 maps to this
    /// screen angle instead of 12 o'clock, so the whole ring can be rotated
    /// by dragging the origin seam.
    pub origin: f32,
}

impl RingGeom {
    pub fn of(rect: egui::Rect, deadzone: f32, origin: f32) -> Self {
        let r_out = (rect.width().min(rect.height()) * 0.5 - 4.0).max(12.0);
        let r_in = (r_out * deadzone.clamp(0.0, 0.9)).max(8.0);
        Self { center: rect.center(), r_in, r_out, origin: origin.rem_euclid(1.0) }
    }
    /// Screen radius of tree-y fraction `v`.
    pub fn radius(&self, v: f32) -> f32 {
        self.r_in + v.clamp(0.0, 1.0) * (self.r_out - self.r_in)
    }
    /// Screen point of tree-space `(u, v)`.
    pub fn point(&self, u: f32, v: f32) -> egui::Pos2 {
        self.center + polar_dir(u + self.origin) * self.radius(v)
    }
    /// Tree-space `(u, v)` of a screen point. `v` < 0 inside the hub and > 1
    /// outside the rim — callers gate the band they care about.
    pub fn unit(&self, p: egui::Pos2) -> (f32, f32) {
        let rel = p - self.center;
        let (u, mag) = flexinput_core::menu::radial_unit(rel.x, rel.y);
        (
            (u - self.origin).rem_euclid(1.0),
            (mag - self.r_in) / (self.r_out - self.r_in).max(1e-3),
        )
    }
    /// Raw screen-angle fraction of a point, IGNORING the origin offset — used
    /// when dragging the origin seam itself (the seam should follow the cursor).
    pub fn raw_angle(&self, p: egui::Pos2) -> f32 {
        let rel = p - self.center;
        flexinput_core::menu::radial_unit(rel.x, rel.y).0
    }
}

/// Paint one sector ring into `rect` from the zone tree's polar projection:
/// plate sectors/ring bands, hover highlight, selection ring, dead-center
/// hub, and per-zone destination icons (or a faint index when unmapped).
/// Returns the zone the pointer position `pos` falls in, if any. Shared by
/// the node body, the pinned widget, and the menu overlay.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_radial_ring(
    ui: &egui::Ui,
    rect: egui::Rect,
    zones: &[(u32, [f32; 4])],
    deadzone: f32,
    origin: f32,
    hover: i32,
    selected: Option<usize>,
    zone_maps: &[serde_json::Value],
    zone_meta: &HashMap<u32, ZoneMeta>,
    colors: ZoneColors,
    pos: Option<egui::Pos2>,
    cursor: Option<egui::Vec2>,
) -> Option<usize> {
    let p = ui.painter();
    let geom = RingGeom::of(rect, deadzone, origin);

    let mut picked = None;
    if let Some(pos) = pos {
        let (u, v) = geom.unit(pos);
        // Allow ~8 px of slack past the rim so edge clicks still land.
        let slack = 8.0 / (geom.r_out - geom.r_in).max(1e-3);
        if v > 0.0 && v <= 1.0 + slack {
            let (uc, vc) = (u.min(0.9999), v.clamp(0.0, 0.9999));
            picked = zones.iter()
                .find(|(_, [x0, y0, x1, y1])|
                    uc >= *x0 && uc < *x1 && vc >= *y0 && vc < *y1)
                .map(|(zid, _)| *zid as usize);
        }
    }

    for &(zid, band) in zones {
        let hovered = hover == zid as i32;
        let is_sel = selected == Some(zid as usize);
        paint_radial_zone(
            ui, &geom, zid, band, hovered, is_sel, zone_maps, zone_meta, colors,
        );
    }

    // Dead-center hub: the region where the pointer hovers nothing.
    p.circle_stroke(geom.center, geom.r_in, egui::Stroke::new(1.0_f32, colors.main.gamma_multiply(0.7)));

    // Live cursor: the pointer's deflection from the origin (unit-rect coords
    // → centered vector; full deflection = rim). Drawn last, translucent, so
    // it blends over zone icons instead of hiding them.
    if let Some(c) = cursor {
        let v = egui::vec2((c.x - 0.5) * 2.0, (c.y - 0.5) * 2.0);
        let end = geom.center + v.clamp(egui::vec2(-1.0, -1.0), egui::vec2(1.0, 1.0)) * geom.r_out;
        paint_menu_cursor(p, geom.center, end, colors.hi);
    }

    picked
}

/// Fit `text` into `max_w` px for a rectangular zone: shrink the font from
/// `base` toward a 6 px floor, then — only if it STILL won't fit at the floor
/// (a very long name in a narrow cell) — truncate with an ellipsis. Returns
/// the font and the possibly-truncated string; together they never exceed
/// `max_w`, so a zone name can't spill past its borders.
pub(crate) fn fit_zone_label(
    p: &egui::Painter,
    text: &str,
    base: f32,
    max_w: f32,
) -> (egui::FontId, String) {
    const FLOOR: f32 = 6.0;
    let measure = |s: &str, size: f32| -> f32 {
        p.layout_no_wrap(s.to_string(), crate::settings::ui_font(size), egui::Color32::WHITE)
            .size().x
    };
    let w = measure(text, base);
    if w <= max_w || w <= 0.0 {
        return (crate::settings::ui_font(base), text.to_string());
    }
    let scaled = (base * max_w / w).max(FLOOR);
    if scaled > FLOOR || measure(text, scaled) <= max_w {
        return (crate::settings::ui_font(scaled), text.to_string());
    }
    // Even at the floor it overflows — clip to whatever fits, plus an ellipsis.
    let mut s = String::new();
    for ch in text.chars() {
        if measure(&format!("{s}{ch}…"), FLOOR) > max_w { break; }
        s.push(ch);
    }
    let out = if s.is_empty() { "…".to_string() } else { format!("{s}…") };
    (crate::settings::ui_font(FLOOR), out)
}

/// Zone-name text bent along a ring band: glyphs are laid out one by one with
/// their centres riding the `r_center` circle and rotated so their tops face
/// outward; on the bottom half of the ring the run flips (tops face the hub,
/// reading order reversed) so it never reads upside-down. Auto-shrinks to the
/// arc length available at that radius.
fn paint_curved_label(
    p: &egui::Painter,
    center: egui::Pos2,
    r_center: f32,
    a0: f32,
    a1: f32,
    text: &str,
    base_size: f32,
    color: egui::Color32,
) {
    if a1 <= a0 || text.is_empty() {
        return;
    }
    let tau = std::f32::consts::TAU;
    // Bottom half = the sector's mid angle points more down than up.
    let mid = ((a0 + a1) * 0.5).rem_euclid(tau);
    let flipped = mid > tau * 0.25 && mid < tau * 0.75;

    let widths = |s: f32| -> (Vec<f32>, f32) {
        let f = crate::settings::ui_font(s);
        let mut ws = Vec::new();
        let mut total = 0.0;
        for ch in text.chars() {
            // Whitespace still needs advance; layout gives it zero width.
            let w = p
                .layout_no_wrap(ch.to_string(), f.clone(), color)
                .size()
                .x
                .max(s * 0.28);
            ws.push(w);
            total += w;
        }
        (ws, total)
    };
    // Fit: measure at base size, scale down to the arc length available at the
    // glyph-centre radius.
    let r_anchor = r_center.max(8.0);
    let avail = ((a1 - a0) * r_anchor - 4.0).max(8.0);
    let (_, total0) = widths(base_size);
    let size = if total0 <= avail || total0 <= 0.0 {
        base_size
    } else {
        (base_size * avail / total0).max(7.0)
    };
    let (ws, total) = widths(size);
    let arc_total = total / r_anchor;
    let font = crate::settings::ui_font(size);

    // Reading order: clockwise (increasing angle) on top, counter-clockwise
    // on the bottom so the flipped run still reads left-to-right.
    let mut a = if flipped {
        (a0 + a1) * 0.5 + arc_total * 0.5
    } else {
        (a0 + a1) * 0.5 - arc_total * 0.5
    };
    for (ch, w) in text.chars().zip(ws) {
        let da = w / r_anchor;
        let ac = if flipped { a - da * 0.5 } else { a + da * 0.5 };
        let dir = egui::vec2(ac.sin(), -ac.cos());
        let pos_c = center + dir * r_anchor;
        let galley = p.layout_no_wrap(ch.to_string(), font.clone(), color);
        let gs = galley.size();
        // Tops outward on top, tops inward (rotated a half turn) on bottom.
        let rot = if flipped { ac + std::f32::consts::PI } else { ac };
        // TextShape rotates clockwise around `pos` (the galley's top-left);
        // place it so the glyph's CENTER lands on the anchor circle.
        let (s, c) = (rot.sin(), rot.cos());
        let local = egui::vec2(gs.x * 0.5, gs.y * 0.5);
        let rotated = egui::vec2(local.x * c - local.y * s, local.x * s + local.y * c);
        let mut shape = egui::epaint::TextShape::new(pos_c - rotated, galley, color);
        shape.angle = rot;
        p.add(egui::Shape::Text(shape));
        a = if flipped { a - da } else { a + da };
    }
}

/// Every destination pin across one zone's mapping cards, deduped in card
/// order — what the zone painters show when no icon override is set.
pub(crate) fn zone_out_pins(zone_maps: &[serde_json::Value], zid: u32) -> Vec<String> {
    let mut pins: Vec<String> = Vec::new();
    for c in zone_maps.iter().filter(|c|
        c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == zid as u64)
    {
        for pin in c.get("out").and_then(|v| v.as_array()).into_iter().flatten()
            .filter_map(|v| v.as_str())
        {
            if !pins.iter().any(|x| x == pin) { pins.push(pin.to_string()); }
        }
    }
    pins
}

/// Screen geometry of one zone band of the ring: inner/outer radii and
/// start/end angles, with the same insets and per-sector angular gap the
/// painter uses. `None` when the band degenerates. Shared by the zone
/// painter and the menu overlay's select-glow stroke so they trace the same
/// outline.
pub(crate) fn radial_band_geom(geom: &RingGeom, band: [f32; 4]) -> Option<(f32, f32, f32, f32)> {
    let tau = std::f32::consts::TAU;
    let [x0, y0, x1, y1] = band;
    let full_circle = x0 <= 1e-4 && x1 >= 1.0 - 1e-4;
    let r0 = geom.radius(y0) + if y0 <= 1e-4 { 2.0 } else { 1.5 };
    let r1 = (geom.radius(y1) - 1.5).max(r0 + 1.0);
    let r_mid = (r0 + r1) * 0.5;
    // ~2 px angular gap between sectors at the band's mid radius.
    let gap = if full_circle { 0.0 } else { 2.0 / r_mid.max(1.0) };
    // Tree-x → screen angle, rotated by the ring's origin offset.
    let o = geom.origin * tau;
    let (a0, a1) = (x0 * tau + o + gap, x1 * tau + o - gap);
    if a1 <= a0 { return None; }
    Some((r0, r1, a0, a1))
}

/// Paint ONE zone of the sector ring: plate sector, destination icons (or the
/// `zone_meta` icon override / faint index), and the zone-name label. Split
/// out of `paint_radial_ring` so the menu overlay can draw just the selected
/// cell during the select-linger fade.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_radial_zone(
    ui: &egui::Ui,
    geom: &RingGeom,
    zid: u32,
    band: [f32; 4],
    hovered: bool,
    is_sel: bool,
    zone_maps: &[serde_json::Value],
    zone_meta: &HashMap<u32, ZoneMeta>,
    colors: ZoneColors,
) {
    let p = ui.painter();
    let Some((r0, r1, a0, a1)) = radial_band_geom(geom, band) else { return };

    // Idle plate: dark base tinted by the (additive) main colour so the ring
    // reads over a game; hover/select use the highlight colour.
    let fill = if hovered {
        colors.hi.gamma_multiply(0.30)
    } else {
        plate_fill(colors.main)
    };
    let stroke = if hovered {
        egui::Stroke::new(2.0_f32, colors.hi)
    } else if is_sel {
        egui::Stroke::new(2.0_f32, colors.hi.gamma_multiply(0.6))
    } else {
        egui::Stroke::new(1.0_f32, colors.main)
    };
    for s in radial_sector_shapes(geom.center, r0, r1, a0, a1, fill, stroke) {
        p.add(s);
    }

    // Icon + name placement. The name curves in an OUTER band near the rim; the
    // icon stacks just BELOW it, kept toward the wide outer part of the wedge
    // (the sector pinches toward the hub, so hugging the text keeps the icon
    // roomy). Only a sector too shallow to stack both bands falls back to
    // placing the icon BESIDE the name. Each item is centred and sized in its
    // own sub-cell, kept off the borders.
    let mid = (a0 + a1) * 0.5;
    let r_mid = (r0 + r1) * 0.5;
    let meta = zone_meta.get(&zid);
    let label = meta.map(|m| m.label.as_str()).unwrap_or("");
    let label_col = if hovered { egui::Color32::WHITE } else { crate::settings::tinted_gray(200) };
    let dir_at = |a: f32| egui::vec2(a.sin(), -a.cos());

    let has_override = meta.map(|m| !m.icon.is_empty() || !m.svg.is_empty()).unwrap_or(false);
    let pins = if has_override { Vec::new() } else { zone_out_pins(zone_maps, zid) };
    let has_icon = has_override || !pins.is_empty();
    let has_label = !label.is_empty();

    let rad = r1 - r0;
    let arc_at = |r: f32| (a1 - a0) * r.max(1.0);
    let arc_mid = arc_at(r_mid);
    // Keep icons a touch inside their cell so they never graze the borders.
    const MARGIN: f32 = 0.82;

    // (icon centre, icon square, chip-row span, text arc, text-centre radius,
    //  text base size). The base is generous — `paint_curved_label` shrinks it
    // to the arc, so wide zones read large and narrow ones stay tidy.
    let (icon_center, icon_sz, chip_span, text_a0, text_a1, text_r, text_base) =
        if has_icon && has_label {
            // Choose BESIDE vs STACK by whether the name actually FITS beside a
            // radial-height icon at a comfortable size — so a short name in a
            // wide wedge sits beside a big icon (both larger), while a long name
            // takes the full rim arc with the icon stacked below. A ring too
            // shallow to stack a separate icon band also goes beside.
            let text_h = (rad * 0.4).clamp(13.0, 28.0).min((rad - 10.0).max(0.0));
            let inner_h = (r1 - text_h - 4.0 - r0).max(0.0);
            let icon_side = (rad * MARGIN).clamp(10.0, 44.0);
            let want = (rad * 0.6).clamp(9.0, 22.0);
            let name_w = p.layout_no_wrap(label.to_string(),
                crate::settings::ui_font(want), egui::Color32::WHITE).size().x;
            if arc_mid >= icon_side + name_w + 14.0 || inner_h < 14.0 {
                // BESIDE: icon square on the start side, name centred across the
                // remaining arc, both riding the mid radius.
                let icon_ang = (icon_side + 6.0) / r_mid;
                (geom.center + dir_at(a0 + icon_ang * 0.5) * r_mid, icon_side, icon_side,
                 a0 + icon_ang, a1, r_mid, want)
            } else {
                // STACK: name curves in the outer band, icon centred just below,
                // hugging the text so it rides the wide outer part of the wedge.
                let tr = r1 - text_h * 0.5;
                let icon_top = r1 - text_h - 4.0;
                let sz = ((icon_top - r0).min(arc_at(icon_top)) * MARGIN).clamp(10.0, 44.0);
                let icon_r = icon_top - sz * 0.5;
                (geom.center + dir_at(mid) * icon_r, sz, arc_at(icon_r),
                 a0, a1, tr, (text_h * 0.72).clamp(9.0, 26.0))
            }
        } else if has_icon {
            // Icon only: centred, budgeted by the smaller axis.
            let sz = (rad.min(arc_mid) * MARGIN).clamp(10.0, 34.0);
            (geom.center + dir_at(mid) * r_mid, sz, arc_mid, a0, a1, r_mid, 9.0)
        } else {
            // Name only (or empty): the name centres in the whole band.
            (geom.center + dir_at(mid) * r_mid, 0.0, arc_mid, a0, a1, r_mid,
             (rad * 0.55).clamp(9.0, 24.0))
        };

    if has_override {
        if let Some(m) = meta {
            if let Some(tex) = crate::macro_icons::macro_port_icon_texture(ui.ctx(), &m.icon, &m.svg, icon_sz) {
                p.image(
                    tex.id(),
                    egui::Rect::from_center_size(icon_center, egui::vec2(icon_sz, icon_sz)),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }
    } else if !pins.is_empty() {
        let ic = icon_sz.clamp(10.0, 26.0);
        let gap_px = 3.0;
        let fit = (((chip_span + gap_px) / (ic + gap_px)).floor() as usize).clamp(1, pins.len());
        let truncated = fit < pins.len();
        let total_w = fit as f32 * ic + (fit.saturating_sub(1)) as f32 * gap_px;
        let mut x = icon_center.x - total_w * 0.5;
        let y = icon_center.y - ic * 0.5;
        for pin in pins.iter().take(fit) {
            let w = super::viewer::paint_chord_chip_to_rect(
                p, ui.ctx(), egui::pos2(x, y), ic, pin,
                super::remapper_icons::Skin::Kbm,
            );
            x += w + gap_px;
        }
        if truncated {
            p.text(egui::pos2(x, icon_center.y), egui::Align2::LEFT_CENTER, "…",
                crate::settings::ui_font(ic * 0.6), crate::settings::tinted_gray(170));
        }
    } else if !has_label {
        // Bare zone: faint index at the centre.
        p.text(
            icon_center, egui::Align2::CENTER_CENTER, format!("{zid}"),
            crate::settings::ui_font((rad * 0.30).clamp(11.0, 22.0)),
            if hovered { egui::Color32::WHITE } else { crate::settings::tinted_gray(150) },
        );
    }

    // Zone name bent along its arc, centred in its band and flipped on the
    // bottom half so it never reads upside-down. A straight label at a radial
    // sector spills across the wedge, so the ring always curves its names.
    if has_label {
        paint_curved_label(p, geom.center, text_r, text_a0, text_a1, label, text_base, label_col);
    }
}

/// Paint ONE rectangular zone cell (overlay grid style): plate, hover/select
/// styling, destination icons / icon override / faint index, and the
/// zone-name label. Shared by the menu overlay's grid painter and its
/// select-linger fade.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_grid_zone_cell(
    ui: &egui::Ui,
    zr: egui::Rect,
    zid: u32,
    hovered: bool,
    zone_maps: &[serde_json::Value],
    zone_meta: &HashMap<u32, ZoneMeta>,
    colors: ZoneColors,
) {
    let p = ui.painter();
    if hovered {
        p.rect_filled(zr, 5.0, colors.hi.gamma_multiply(0.30));
        p.rect_stroke(zr, 5.0, egui::Stroke::new(2.0_f32, colors.hi), egui::StrokeKind::Inside);
    } else {
        p.rect_stroke(zr, 5.0, egui::Stroke::new(1.0_f32, colors.main), egui::StrokeKind::Inside);
    }

    let meta = zone_meta.get(&zid);
    let label = meta.map(|m| m.label.as_str()).unwrap_or("");
    let label_col = if hovered { egui::Color32::WHITE } else { crate::settings::tinted_gray(200) };
    let mut drew_content = false;

    // A named zone reserves a bottom band for its label; icons centre in the
    // region ABOVE it and shrink to fit, so icon and text never collide.
    let base_lbl = (zr.height() * 0.20).clamp(10.0, 15.0);
    let label_band = if label.is_empty() { 0.0 } else { base_lbl + 4.0 };
    let icon_area = egui::Rect::from_min_max(zr.min, egui::pos2(zr.max.x, zr.max.y - label_band));
    let ic_h = icon_area.height().max(4.0);

    // Destination icons: every out pin across the zone's mapping cards,
    // deduped in card order. KBM as the base skin — destinations are
    // typically keys/mouse/macro targets; a virtual-pad pin still renders
    // via the chip painter's any-skin fallback (dimmed). A per-zone icon
    // OVERRIDE from `zone_meta` replaces them entirely.
    let override_tex = meta.and_then(|m| {
        let ic = (ic_h * 0.6).clamp(14.0, 34.0).min(icon_area.width() - 6.0).max(10.0);
        crate::macro_icons::macro_port_icon_texture(ui.ctx(), &m.icon, &m.svg, ic)
            .map(|tex| (tex, ic))
    });
    if let Some((tex, ic)) = override_tex {
        p.image(
            tex.id(),
            egui::Rect::from_center_size(icon_area.center(), egui::vec2(ic, ic)),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        drew_content = true;
    } else {
        let pins = zone_out_pins(zone_maps, zid);
        if pins.is_empty() {
            if label.is_empty() {
                // Unmapped zone: faint index so it's still identifiable as a target.
                p.text(
                    icon_area.center(), egui::Align2::CENTER_CENTER, format!("{zid}"),
                    crate::settings::ui_font((ic_h * 0.5).clamp(11.0, 26.0)),
                    if hovered { egui::Color32::WHITE } else { crate::settings::tinted_gray(150) },
                );
            }
        } else {
            let ic = (ic_h * 0.62).clamp(14.0, 30.0);
            let gap = 4.0;
            // Show as many icons as fit; a trailing "…" marks the overflow.
            let fit = (((icon_area.width() - 8.0 + gap) / (ic + gap)).floor() as usize)
                .clamp(1, pins.len());
            let truncated = fit < pins.len();
            let total_w = fit as f32 * ic + (fit.saturating_sub(1)) as f32 * gap;
            let mut x = icon_area.center().x - total_w * 0.5;
            let y = icon_area.center().y - ic * 0.5;
            for pin in pins.iter().take(fit) {
                let w = super::viewer::paint_chord_chip_to_rect(
                    p, ui.ctx(), egui::pos2(x, y), ic, pin,
                    super::remapper_icons::Skin::Kbm,
                );
                x += w + gap;
            }
            if truncated {
                p.text(egui::pos2(x, icon_area.center().y), egui::Align2::LEFT_CENTER, "…",
                    crate::settings::ui_font(ic * 0.6), crate::settings::tinted_gray(170));
            }
            drew_content = true;
        }
    }
    if !label.is_empty() {
        // Auto-shrink (then ellipsize) so the name never spills across the cell.
        let (f, txt) = fit_zone_label(p, label, base_lbl, zr.width() - 8.0);
        if drew_content {
            p.text(egui::pos2(zr.center().x, zr.bottom() - 3.0),
                egui::Align2::CENTER_BOTTOM, txt, f, label_col);
        } else {
            p.text(zr.center(), egui::Align2::CENTER_CENTER, txt, f, label_col);
        }
    }
}

/// A border of the ring materialized for drawing / hit-testing, either from
/// the mapping-mode tree (carries a divider `path`) or a ports-mode flat edge
/// (carries the edge index). `angular` borders are straight radius lines at
/// angle `pos`; ring borders are arcs at radius `pos`.
struct RingBorder {
    angular: bool,
    pos: f32,
    span_lo: f32,
    span_hi: f32,
    lo: f32,
    hi: f32,
    /// Tree divider path (mapping mode) or `vec![edge index]` (ports mode).
    key: Vec<u8>,
    /// The origin seam (tree-u 0): not a real divider — dragging it rotates the
    /// whole ring (the `menu_radial_origin` param) rather than editing zones.
    seam: bool,
}

/// Distance in px from a screen point to a border, or None when the point is
/// outside the border's span (with a little slack).
fn ring_border_dist(geom: &RingGeom, b: &RingBorder, p: egui::Pos2) -> Option<f32> {
    let tau = std::f32::consts::TAU;
    let (u, v) = geom.unit(p);
    if b.angular {
        let slack = 6.0 / (geom.r_out - geom.r_in).max(1e-3);
        if v < b.span_lo - slack || v > b.span_hi + slack { return None; }
        // Angular px distance at the pointer's radius (wrap-aware for the seam).
        let mut du = (u - b.pos).abs();
        du = du.min(1.0 - du);
        Some(du * tau * geom.radius(v.clamp(0.0, 1.0)))
    } else {
        let r_line = geom.radius(b.pos);
        let slack = 6.0 / (tau * r_line).max(1e-3);
        if u < b.span_lo - slack || u > b.span_hi + slack { return None; }
        let mag = (p - geom.center).length();
        Some((mag - r_line).abs())
    }
}

/// Paint a border: radius line for angular, arc polyline for rings.
fn paint_ring_border(p: &egui::Painter, geom: &RingGeom, b: &RingBorder, stroke: egui::Stroke) {
    if b.angular {
        p.line_segment([geom.point(b.pos, b.span_lo), geom.point(b.pos, b.span_hi)], stroke);
    } else {
        let steps = (((b.span_hi - b.span_lo) * std::f32::consts::TAU
            / 0.15).ceil() as usize).max(2);
        let pts: Vec<egui::Pos2> = (0..=steps)
            .map(|i| geom.point(
                b.span_lo + (b.span_hi - b.span_lo) * (i as f32 / steps as f32),
                b.pos))
            .collect();
        p.add(egui::Shape::line(pts, stroke));
    }
}

/// Materialize the ring's borders — the origin seam first, then the zone-tree
/// dividers (partial, per-zone). Both mapping AND ports mode use the tree now (a
/// pure-grid ports menu migrates losslessly), so the border editor is identical
/// and radial-ports edits correctly. Shared by `render_radial_field`, the pinned
/// field, and the gamepad-nav focus highlight so they index borders identically.
fn radial_nav_borders(snarl: &Snarl<NodeData>, node_id: NodeId) -> Vec<RingBorder> {
    use flexinput_core::touchzones as tz;
    const SEAM_KEY: u8 = 254;
    let mut borders: Vec<RingBorder> = vec![RingBorder {
        angular: true, pos: 0.0, span_lo: 0.0, span_hi: 1.0,
        lo: 0.0, hi: 1.0, key: vec![SEAM_KEY], seam: true,
    }];
    let tree = super::viewer::tz_field_tree(snarl, node_id, 0);
    borders.extend(tree.dividers().iter().map(|d| RingBorder {
        angular: matches!(d.axis, tz::Axis::V),
        pos: d.pos, span_lo: d.span_lo, span_hi: d.span_hi,
        lo: d.lo, hi: d.hi, key: d.path.clone(), seam: false,
    }));
    borders
}

/// Draw the gamepad-nav focused-border highlight on a radial menu field and
/// publish per-border midpoint hit-rects for the RS cursor (keyed by `node_id`,
/// field 0). Angular borders are the "column"/V axis, radial rings the "row"/H
/// axis, counted per axis to match the TZ nav driver's (axis, line) index model.
/// Shared by the node body AND the pinned field renderer so both show which
/// divider is being edited over the game. Gates against the viewport-agnostic
/// `nav_pass` so it shows in the config overlay's own viewport.
pub(crate) fn paint_radial_nav_focus(
    ui: &egui::Ui,
    node_id: NodeId,
    snarl: &Snarl<NodeData>,
    rect: egui::Rect,
    deadzone: f32,
    origin: f32,
) {
    let nav_tz: Option<(u64, u64, u64, u64, bool)> =
        ui.ctx().data(|d| d.get_temp(egui::Id::new(("gp_nav_tz", node_id.0))));
    let nav_focus_pass = crate::widgets::nav_pass(ui.ctx());
    let geom = RingGeom::of(rect, deadzone, origin);
    let borders = radial_nav_borders(snarl, node_id);
    let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    let painter = ui.painter_at(rect.expand(4.0));
    let style = crate::widgets::NavHighlightStyle::of(ui.ctx());

    // Focused-zone highlight: a translucent accent sector over the focused zone,
    // drawn BEFORE the borders so the divider highlights sit on top. Keyed by the
    // zone id the nav publishes (usize::MAX = none).
    let nav_zone: Option<(u64, u64, u64)> =
        ui.ctx().data(|d| d.get_temp(egui::Id::new(("gp_nav_tz_zone", node_id.0))));
    if let Some((zpass, zfield, zid)) = nav_zone {
        if zfield == 0 && zid != u64::MAX && crate::widgets::nav_pass_matches(ui.ctx(), zpass) {
            let zones = super::viewer::tz_field_tree(snarl, node_id, 0).zones();
            if let Some((_, band)) = zones.into_iter().find(|(id, _)| *id as u64 == zid) {
                if let Some((r0, r1, a0, a1)) = radial_band_geom(&geom, band) {
                    let fill = style.accent.gamma_multiply(0.22);
                    let stroke = egui::Stroke::new(2.5_f32, style.accent);
                    for s in radial_sector_shapes(geom.center, r0, r1, a0, a1, fill, stroke) {
                        painter.add(s);
                    }
                }
            }
        }
    }

    let mut nav_line_rects: Vec<(u8, u32, egui::Rect)> = Vec::new();
    // Nav targets for the gamepad spatial walk (global space): kind 0 = zone,
    // 1 = border, 2 = seam. Zone centroids come from the same polar projection
    // the ring painter uses.
    let mut nav_targets: Vec<(u8, u32, u32, egui::Pos2)> =
        super::viewer::tz_field_tree(snarl, node_id, 0).zones().into_iter()
            .map(|(id, [x0, y0, x1, y1])| (0u8, id, 0u32,
                to_global * geom.point((x0 + x1) * 0.5, (y0 + y1) * 0.5)))
            .collect();
    // The seam sits at tree-u 0, mid-radius (the ring's 12-o'clock reference edge).
    nav_targets.push((2, 0, 0, to_global * geom.point(0.0, 0.5)));
    // Seam focus highlight (the "root border"): the nav driver publishes it as a
    // simple bool so the ring's reference edge lights up when walked onto.
    let seam_focus: Option<(u64, bool)> =
        ui.ctx().data(|d| d.get_temp(egui::Id::new(("gp_nav_tz_seam", node_id.0))));
    let seam_hot = matches!(seam_focus,
        Some((p, true)) if nav_focus_pass.saturating_sub(p) <= 2);
    let (mut vcount, mut hcount) = (0u32, 0u32);
    for b in &borders {
        if b.seam {
            if seam_hot {
                paint_ring_border(&painter, &geom, b, egui::Stroke::new(4.0_f32, style.accent));
            }
            continue;
        }
        let axis: u8 = if b.angular { 0 } else { 1 };
        let line = if b.angular { let n = vcount; vcount += 1; n }
                   else         { let n = hcount; hcount += 1; n };
        let mid = (b.span_lo + b.span_hi) * 0.5;
        let c = if b.angular { geom.point(b.pos, mid) } else { geom.point(mid, b.pos) };
        let gc = to_global * c;
        nav_line_rects.push((axis, line, egui::Rect::from_center_size(gc, egui::vec2(18.0, 18.0))));
        nav_targets.push((1, axis as u32, line, gc));
        let grab = match nav_tz {
            Some((pass, f, a, l, g))
                if nav_focus_pass.saturating_sub(pass) <= 2
                    && f == 0 && a == axis as u64 && l == line as u64 => Some(g),
            _ => None,
        };
        if let Some(g) = grab {
            let col = if g { style.grabbed } else { style.accent };
            paint_ring_border(&painter, &geom, b, egui::Stroke::new(3.0_f32, col));
        }
    }
    let pass_nr = ui.ctx().cumulative_pass_nr();
    let nav_pass_now = crate::widgets::nav_pass(ui.ctx());
    ui.ctx().data_mut(|d| {
        d.insert_temp(egui::Id::new(("gp_nav_tz_lines", node_id.0, 0usize)),
            (pass_nr, nav_line_rects));
        d.insert_temp(egui::Id::new(("gp_nav_tz_targets", node_id.0, 0usize)),
            (nav_pass_now, nav_targets));
    });
}

/// The radial menu field's MOUSE border editor: drag dividers to move them, drag
/// the origin seam to rotate the whole ring, double-click to recenter, and the
/// hover "−"/"+" buttons to remove/add. Extracted so the node body AND the pinned
/// / config-overlay field share ONE editor — the pinned radial field previously
/// only click-selected a zone (no divider move / seam rotate over the game), which
/// is why the mouse "could only select a zone" there. `resp` is the field's
/// `click_and_drag` response. Returns whether a zone click-select should be
/// SUPPRESSED (a border sits under the click, or a border drag is in progress).
#[allow(clippy::too_many_arguments)]
pub(crate) fn radial_border_editor(
    ui: &mut egui::Ui,
    node_id: NodeId,
    snarl: &mut Snarl<NodeData>,
    rect: egui::Rect,
    deadzone: f32,
    origin: f32,
    resp: &egui::Response,
    accent: egui::Color32,
    visuals: &egui::Visuals,
) -> bool {
    use flexinput_core::touchzones as tz;
    let tree = super::viewer::tz_field_tree(snarl, node_id, 0);
    let geom = RingGeom::of(rect, deadzone, origin);
    let zones = tree.zones();
    // Border list (seam first, then tree dividers) from the shared builder so the
    // pinned field indexes borders identically. Both modes drive the tree now.
    let borders = radial_nav_borders(snarl, node_id);

    let painter = ui.painter_at(rect.expand(4.0));
    let from_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY)
        .inverse();
    let ptr = ui.input(|i| i.pointer.hover_pos()).map(|p| from_global * p);
    let hit_at = |p: egui::Pos2| -> Option<usize> {
        borders.iter().enumerate()
            .filter_map(|(i, b)| ring_border_dist(&geom, b, p).map(|d| (i, d)))
            .filter(|(_, d)| *d <= 7.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };

    // Drag state survives across frames so a fast pointer can't lose the
    // border it grabbed. (angular flag, key) identifies the border.
    let drag_key = egui::Id::new(("menu_rdrag", node_id));
    let mut drag: Option<(bool, Vec<u8>)> = ui.ctx().data(|d| d.get_temp(drag_key));
    if resp.drag_started() {
        drag = resp.interact_pointer_pos()
            .and_then(hit_at)
            .map(|i| (borders[i].angular, borders[i].key.clone()));
        ui.ctx().data_mut(|d| match &drag {
            Some(v) => d.insert_temp(drag_key, v.clone()),
            None => d.remove::<(bool, Vec<u8>)>(drag_key),
        });
    }
    if resp.drag_stopped() {
        ui.ctx().data_mut(|d| d.remove::<(bool, Vec<u8>)>(drag_key));
    }
    let dragged_border = drag.as_ref().and_then(|(ang, key)|
        borders.iter().position(|b| b.angular == *ang && b.key == *key));
    if let (true, Some(bi), Some(p)) = (resp.dragged(), dragged_border, resp.interact_pointer_pos()) {
        let b = &borders[bi];
        if b.seam {
            // Rotate the whole ring: the seam follows the cursor's raw angle.
            let new_origin = geom.raw_angle(p);
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("menu_radial_origin".into(), serde_json::json!(new_origin));
            }
        } else {
            let (u, v) = geom.unit(p);
            let want = if b.angular { u } else { v };
            let (lo, hi) = (b.lo + 0.03, b.hi - 0.03);
            let t = if lo <= hi { want.clamp(lo, hi) } else { (b.lo + b.hi) * 0.5 };
            let mut nt = tree.clone();
            if nt.set_divider_t(&b.key, t) {
                super::viewer::tz_set_field_tree(snarl, node_id, 0, &nt);
            }
        }
    }

    // Double-click a border to recenter it between its two neighbours — the
    // same gesture the rectangular editor uses (viewer.rs `tz_draw_field`). The
    // origin seam only rotates, so it is skipped. `interact_pointer_pos` is
    // already in local (layer) space, matching the drag path above.
    if resp.double_clicked() {
        if let Some(bi) = resp.interact_pointer_pos()
            .and_then(hit_at)
            .filter(|&bi| !borders[bi].seam)
        {
            let b = &borders[bi];
            // Recentre between the divider's IMMEDIATE neighbours (mirrors the
            // rectangular editor; the naïve parent-span midpoint overshoots and
            // squashes the next zone in a 3+-zone tree).
            let target = tree.centered_divider_pos(&b.key)
                .unwrap_or((b.lo + b.hi) * 0.5);
            let mut nt = tree.clone();
            if nt.set_divider_t(&b.key, target) {
                super::viewer::tz_set_field_tree(snarl, node_id, 0, &nt);
            }
        }
    }

    // Border strokes (hot = hovered or mid-drag). The origin seam is drawn
    // thicker so it reads as the ring's reference edge.
    for (i, b) in borders.iter().enumerate() {
        let hot = dragged_border == Some(i)
            || (dragged_border.is_none()
                && ptr.and_then(hit_at) == Some(i));
        let stroke = if hot {
            egui::Stroke::new(if b.seam { 4.0 } else { 2.0_f32 }, accent)
        } else if b.seam {
            egui::Stroke::new(3.0_f32, visuals.weak_text_color().gamma_multiply(1.4))
        } else {
            egui::Stroke::new(1.0_f32, visuals.weak_text_color())
        };
        paint_ring_border(&painter, &geom, b, stroke);
    }
    if dragged_border.is_some() || ptr.and_then(hit_at).is_some() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grab);
    }

    // Gamepad-nav focused-border highlight + RS-cursor hit-rects, shared with the
    // pinned field renderer so both surfaces show which divider is being edited.
    paint_radial_nav_focus(ui, node_id, snarl, rect, deadzone, origin);

    // "−" / "+" mini buttons — per-zone tree ops in BOTH modes. Hovering a divider
    // offers "−" to remove/merge it; hovering a zone away from dividers offers "+"
    // to split near the closest edge. The origin seam is never removable — it just
    // rotates.
    let mut tree_sub: Option<(f32, f32, tz::Axis, bool)> = None;
    let mut tree_rem: Option<Vec<u8>> = None;
    if let Some(bi) = ptr.and_then(hit_at).filter(|&bi| !borders[bi].seam) {
        let b = &borders[bi];
        let mid = (b.span_lo + b.span_hi) * 0.5;
        let c = if b.angular { geom.point(b.pos, mid) } else { geom.point(mid, b.pos) };
        if super::viewer::tz_mini_button(ui, &painter,
            ui.id().with((node_id, "vmrm", bi)), c, "−", accent, visuals)
        {
            tree_rem = Some(b.key.clone());
        }
    } else if let Some(p) = ptr {
        let (u, v) = geom.unit(p);
        if (0.0..1.0).contains(&v) {
            let uc = u.min(0.9999);
            if let Some(&(_, [x0, y0, x1, y1])) = zones.iter().find(
                |(_, [x0, y0, x1, y1])| uc >= *x0 && uc < *x1 && v >= *y0 && v < *y1)
            {
                let r_here = geom.radius(v);
                let tau = std::f32::consts::TAU;
                // px distances to the four polar edges of the zone
                let d_a0 = (uc - x0) * tau * r_here;
                let d_a1 = (x1 - uc) * tau * r_here;
                let d_r0 = (v - y0) * (geom.r_out - geom.r_in);
                let d_r1 = (y1 - v) * (geom.r_out - geom.r_in);
                let m = d_a0.min(d_a1).min(d_r0).min(d_r1);
                if m <= 24.0 {
                    let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
                    let inset_u = 14.0 / (tau * r_here).max(1e-3);
                    let inset_v = 14.0 / (geom.r_out - geom.r_in).max(1e-3);
                    let (pos, axis, new_low) = if m == d_a0 {
                        (geom.point(x0 + inset_u, (y0 + y1) * 0.5), tz::Axis::V, true)
                    } else if m == d_a1 {
                        (geom.point(x1 - inset_u, (y0 + y1) * 0.5), tz::Axis::V, false)
                    } else if m == d_r0 {
                        (geom.point((x0 + x1) * 0.5, y0 + inset_v), tz::Axis::H, true)
                    } else {
                        (geom.point((x0 + x1) * 0.5, y1 - inset_v), tz::Axis::H, false)
                    };
                    if super::viewer::tz_mini_button(ui, &painter,
                        ui.id().with((node_id, "vmrp")), pos, "+", accent, visuals)
                    {
                        tree_sub = Some((cx, cy, axis, new_low));
                    }
                }
            }
        }
    }

    if let Some((cx, cy, axis, new_low)) = tree_sub {
        super::viewer::tz_subdivide_at(snarl, node_id, 0, cx, cy, axis, new_low);
    }
    if let Some(path) = tree_rem {
        super::viewer::tz_request_or_apply_merge(snarl, node_id, 0, &tree, &path);
    }
    // Port sync + wiring preservation is handled by `tz_set_field_tree` (called
    // inside the subdivide/merge above), so no explicit `regenerate_menu_ports`
    // here.

    drag.is_some()
        || (resp.clicked()
            && resp.interact_pointer_pos().and_then(|p| hit_at(p)).is_some())
}

/// The radial zone field on the node body: paints the polar projection of the
/// zone tree with the live hover (from the eval mirror), click-selects a zone
/// in mapping mode, and hosts the border editor — drag borders to move them,
/// "−" removes one, "+" adds a sector cut or ring next to the hovered border
/// (ports mode) or splits the hovered zone from its nearest edge (mapping
/// mode), matching the flat field's editing model.
fn render_radial_field(
    node_id: NodeId,
    mapping: bool,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    zone_live: &HashMap<(usize, usize), (f32, f32, bool)>,
    colors: ZoneColors,
    field_w: f32,
    field_h: f32,
) -> egui::Rect {
    let visuals = ui.visuals().clone();
    // Editor affordances (border grips, +/- buttons) use the crisp opaque
    // accent; the ring itself paints from the full `colors` (opacity/blend).
    let accent = colors.accent;
    let tree = super::viewer::tz_field_tree(snarl, node_id, 0);
    let (deadzone, origin, sel_zone, zone_maps) = {
        let Some(node) = snarl.get_node(node_id) else {
            return ui.allocate_exact_size(egui::vec2(field_w, field_h), egui::Sense::hover()).0;
        };
        (
            pf32(node, "pointer_deadzone", 0.25),
            pf32(node, "menu_radial_origin", 0.0),
            node.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            node.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
        )
    };
    let (zone_meta, ptr) = snarl.get_node(node_id)
        .map(|n| (menu_zone_meta(n), menu_pointer(n)))
        .unwrap_or_default();
    let size = field_w.min(field_h.max(160.0)).max(160.0);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(field_w.max(size), size),
        egui::Sense::click_and_drag(),
    );
    let hover = zone_live.iter()
        .find(|((_, _), (_, _, act))| *act)
        .map(|((_, z), _)| *z as i32)
        .unwrap_or(-1);
    let zones = tree.zones();
    let picked = paint_radial_ring(
        ui, rect, &zones, deadzone, origin, hover,
        if mapping { Some(sel_zone) } else { None },
        &zone_maps, &zone_meta, colors, resp.interact_pointer_pos(), ptr,
    );

    // Border editor (drag dividers / rotate the origin seam / recenter / hover
    // ± add-remove) — shared with the pinned & config-overlay field so every
    // surface edits identically (the pinned radial field used to only zone-select).
    let suppress = radial_border_editor(
        ui, node_id, snarl, rect, deadzone, origin, &resp, accent, &visuals);
    // Zone click-select (mapping mode) — only when the click missed all borders
    // and no border drag is in progress.
    if mapping && resp.clicked() && !suppress {
        if let Some(z) = picked {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("sel_field".into(), serde_json::json!(0u64));
                node.params.insert("sel_zone".into(), serde_json::json!(z as u64));
            }
        }
    }

    rect
}

/// Icon + name row (Macro pattern: one identity per module — this is how the
/// menu appears as a named target in the mapping pickers).
/// Icon-picker menu button: the shared macro icon set in a grid, plus "Load
/// custom SVG…" (embedded into the patch) and "No icon". Returns
/// `Some((icon key, custom svg))` when the user picked something — both
/// empty = cleared. Reused by the menu header and the per-zone icon override.
/// Synthetic icon-picker category name for dynamic gamepad-control glyphs.
pub(crate) const GP_ICON_CATEGORY: &str = "Gamepad inputs";

pub(crate) fn icon_picker_button(
    ui: &mut egui::Ui,
    grid_id: egui::Id,
    icon: &str,
    icon_svg: &str,
) -> Option<(String, String)> {
    let mut new_icon: Option<(String, String)> = None;
    let icon_btn = if let Some(tex) = crate::macro_icons::macro_port_icon_texture(
        ui.ctx(), icon, icon_svg, 18.0)
    {
        egui::Button::image(egui::Image::new(&tex)
            .fit_to_exact_size(egui::vec2(18.0, 18.0))
            .tint(egui::Color32::WHITE))
    } else {
        egui::Button::new(egui::RichText::new("◌").size(crate::settings::ui_size(14.0)))
    };
    use egui::containers::menu::{MenuButton, MenuConfig, SubMenuButton};
    // `CloseOnClickOutside` so typing in the search box or using the category
    // dropdown (a nested popup) doesn't dismiss the picker mid-edit — only
    // picking an icon (explicit `ui.close()`) or clicking away closes it.
    MenuButton::from_button(icon_btn)
        .config(MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside))
        .ui(ui, |ui: &mut egui::Ui| {
        ui.horizontal(|ui| {
            if ui.button("Load custom SVG…")
                .on_hover_text("Pick an .svg file — it's embedded into the patch")
                .clicked()
            {
                if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                    rfd::FileDialog::new().add_filter("SVG", &["svg"]).pick_file()
                }) {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        new_icon = Some((String::new(), text));
                    }
                }
                ui.close();
            }
            if (!icon.is_empty() || !icon_svg.is_empty()) && ui.button("No icon").clicked() {
                new_icon = Some((String::new(), String::new()));
                ui.close();
            }
        });

        // Category dropdown (default "All") + name search. Both persist in temp
        // memory keyed off the grid id so they survive the popup's frames. The
        // category list is compile-time (sub-folders under `general/`).
        //
        // The category selector is a SUBMENU flyout, not a `ComboBox`: a combo
        // opens a plain popup that egui's menu doesn't track as a child, so
        // clicking an option would read as an outside-click and dismiss the
        // whole picker. A `SubMenuButton` registers with the menu state and
        // keeps the picker open (same pattern as the Switch node's submenus).
        let cat_key = grid_id.with("icon_cat");
        let search_key = grid_id.with("icon_search");
        let mut sel_cat: String = ui.data(|d| d.get_temp::<String>(cat_key)).unwrap_or_default();
        let mut search: String = ui.data(|d| d.get_temp::<String>(search_key)).unwrap_or_default();
        ui.horizontal(|ui| {
            let cur = if sel_cat.is_empty() { "All" } else { sel_cat.as_str() };
            let cat_label = format!("Category: {cur}");
            SubMenuButton::new(cat_label.as_str())
                .config(MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside))
                .ui(ui, |ui| {
                    // Selecting just sets the filter; we deliberately do NOT
                    // `ui.close()` (that could bubble up and shut the picker) —
                    // the flyout dismisses when the pointer leaves it.
                    if ui.selectable_label(sel_cat.is_empty(), "All").clicked() {
                        sel_cat.clear();
                    }
                    for cat in crate::macro_icons::ICON_CATEGORIES {
                        if ui.selectable_label(sel_cat == *cat, *cat).clicked() {
                            sel_cat = (*cat).to_string();
                        }
                    }
                    // Synthetic category: gamepad-control glyphs rendered in the
                    // CURRENT pad's style (stored as `gp:<pin>`, restyle on pad change).
                    if ui.selectable_label(sel_cat == GP_ICON_CATEGORY, GP_ICON_CATEGORY).clicked() {
                        sel_cat = GP_ICON_CATEGORY.to_string();
                    }
                });
            ui.add(egui::TextEdit::singleline(&mut search)
                .hint_text("Search…")
                .desired_width(90.0));
            if !search.is_empty() && ui.small_button("✕").on_hover_text("Clear").clicked() {
                search.clear();
            }
        });
        ui.data_mut(|d| {
            d.insert_temp(cat_key, sel_cat.clone());
            d.insert_temp(search_key, search.clone());
        });

        ui.separator();
        const COLS: usize = 10;
        const CELL: f32 = 26.0;
        const GAP: f32 = 4.0;
        let q = search.trim().to_ascii_lowercase();
        ui.set_min_width(COLS as f32 * (CELL + GAP));
        egui::ScrollArea::vertical().max_height(10.0 * (CELL + GAP)).show(ui, |ui| {
            egui::Grid::new(grid_id).spacing([GAP, GAP]).show(ui, |ui| {
                // Filtered index drives row wrapping — NOT the global index —
                // so a partial category/search still forms a full 10-wide grid.
                let mut shown = 0usize;
                let cell = egui::vec2(CELL - 6.0, CELL - 6.0);
                if sel_cat == GP_ICON_CATEGORY {
                    // Gamepad-control glyphs: rendered per the CURRENT pad style
                    // (macro_port_icon_texture resolves `gp:<pin>` dynamically) and
                    // stored as that key, so a pad swap restyles every placed icon.
                    for (pin, label) in crate::canvas::remapper_icons::GAMEPAD_INPUT_PINS {
                        if !q.is_empty()
                            && !pin.to_ascii_lowercase().contains(&q)
                            && !label.to_ascii_lowercase().contains(&q)
                        {
                            continue;
                        }
                        let key = format!("gp:{pin}");
                        let selected = icon_svg.is_empty() && icon == key;
                        let btn = if let Some(tex) = crate::macro_icons::macro_port_icon_texture(
                            ui.ctx(), &key, "", CELL - 6.0)
                        {
                            egui::Button::image(egui::Image::new(&tex)
                                .fit_to_exact_size(cell).tint(egui::Color32::WHITE))
                                .min_size(egui::vec2(CELL, CELL))
                                .selected(selected)
                        } else {
                            egui::Button::new(*label).selected(selected)
                        };
                        if ui.add(btn).on_hover_text(format!("{label}\n{GP_ICON_CATEGORY}")).clicked() {
                            new_icon = Some((key, String::new()));
                            ui.close();
                        }
                        shown += 1;
                        if shown % COLS == 0 { ui.end_row(); }
                    }
                } else {
                    for (key, label, cat, _) in crate::macro_icons::ALL_ICONS.iter() {
                        if !sel_cat.is_empty() && *cat != sel_cat { continue; }
                        if !q.is_empty()
                            && !key.to_ascii_lowercase().contains(&q)
                            && !label.to_ascii_lowercase().contains(&q)
                        {
                            continue;
                        }
                        let selected = icon_svg.is_empty() && icon == *key;
                        let btn = if let Some(tex) =
                            crate::macro_icons::macro_icon_texture(ui.ctx(), key, CELL - 6.0)
                        {
                            egui::Button::image(egui::Image::new(&tex)
                                .fit_to_exact_size(cell).tint(egui::Color32::WHITE))
                                .min_size(egui::vec2(CELL, CELL))
                                .selected(selected)
                        } else {
                            egui::Button::new(*label).selected(selected)
                        };
                        if ui.add(btn).on_hover_text(format!("{label}\n{cat}")).clicked() {
                            new_icon = Some((key.to_string(), String::new()));
                            ui.close();
                        }
                        shown += 1;
                        if shown % COLS == 0 { ui.end_row(); }
                    }
                }
                if shown == 0 {
                    ui.label(egui::RichText::new("No matching icons").weak());
                }
            });
        });
    });
    new_icon
}

/// Rendered in the node HEADER (viewer.rs dispatches here for module.menu): icon +
/// editable name, the menu's face in mapping pickers. What the menu is - Ports / Mapping,
/// suppression, radial, its colours - is the body's first band ([`menu_mode_rows`]).
pub(crate) fn show_menu_header(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (mut name, icon, icon_svg) = {
        let Some(node) = snarl.get_node(node_id) else { return };
        (
            pstr(node, "menu_name", "").to_string(),
            pstr(node, "menu_icon", "").to_string(),
            pstr(node, "menu_icon_svg", "").to_string(),
        )
    };
    let mut name_changed = false;
    let mut new_icon: Option<(String, String)> = None;
    ui.horizontal(|ui| {
        new_icon = icon_picker_button(ui, egui::Id::new((node_id, "menu_icon_grid")), &icon, &icon_svg);
        let resp = ui.add(egui::TextEdit::singleline(&mut name)
            .hint_text("Menu name")
            .desired_width(160.0));
        if resp.changed() { name_changed = true; }

    });
    if name_changed || new_icon.is_some() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if name_changed {
                node.params.insert("menu_name".into(), serde_json::Value::String(name));
            }
            if let Some((key, svg)) = new_icon {
                node.params.insert("menu_icon".into(), serde_json::Value::String(key));
                node.params.insert("menu_icon_svg".into(), serde_json::Value::String(svg));
            }
        }
    }
}

/// The body's first band: Ports / Mapping, how the menu's drivers are suppressed while it is
/// open, grid or radial, its two colours, and placing it on the screen. They were controls
/// of every size crammed into the header beside the name.
fn menu_mode_rows(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    w: f32,
) {
    use super::viewer::curve_rows::{pill_bands, pill_combo, segmented, word_toggle, word_w};
    let Some(node) = snarl.get_node(node_id) else { return };
    let mapping = pstr(node, "zone_mode", "ports") == "mapping";
    let suppress = pbool(node, "suppress_while_open", true);
    let sup_mode = pstr(node, "suppress_mode", "partial").to_string();
    let mut radial = pbool(node, "menu_radial", false);
    let cur = if !suppress {
        "off"
    } else if sup_mode == "full" {
        "full"
    } else if sup_mode == "latch" {
        "latch"
    } else {
        "partial"
    }
    .to_string();
    const SWATCHES_W: f32 = 76.0;
    let cells = pill_bands(ui, w, &[150.0, 200.0, word_w(ui, "radial"), SWATCHES_W, word_w(ui, "🖵 position on screen…")]);

    const MODE_TIP: &str = "Ports: typed X / Y / Active outputs per zone + Select. Mapping: each zone mapped to gamepad / key / stick outputs with internal cards.";
    if let Some(want_mapping) = segmented(ui, cells[0], &mapping, &[(false, "ports", MODE_TIP), (true, "mapping", MODE_TIP)], ("vm_mode_seg", node_id.0)) {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("zone_mode".to_string(), serde_json::json!(if want_mapping { "mapping" } else { "ports" }));
        }
        regenerate_menu_ports(node_id, snarl);
    }
    // Suppression is one 3-way choice stored across `suppress_while_open` (off vs on) and
    // `suppress_mode` (partial / latch / full), so older patches load unchanged.
    let modes: [(String, &str); 4] = [
        ("off".to_string(), "passthrough"),
        ("partial".to_string(), "active"),
        ("latch".to_string(), "latch"),
        ("full".to_string(), "full"),
    ];
    if let Some(sel) = pill_combo(ui, cells[1], "suppress|suppress drivers", &cur, &modes,
        "What the menu's drivers do while it is open.\nPassthrough: they keep flowing to the rest of the patch.\nActive: a driver is blocked at the source only while it is actively steering the menu (stick past deadzone / finger down / gyro cursor out of the deadzone); idle ones pass, and a blocked one returns the moment it is back at rest.\nLatch: the first driver to engage owns the menu and alone is blocked; the others keep flowing to the game until it disengages.\nFull: every enabled driver is blocked at the source for as long as the menu is open.",
        ("vm_suppress_pill", node_id.0))
    {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("suppress_while_open".into(), serde_json::json!(sel != "off"));
            if sel != "off" {
                node.params.insert("suppress_mode".into(), serde_json::json!(sel));
            }
        }
    }
    if word_toggle(ui, cells[2], &mut radial, "radial",
        "Show the same zones as a sector ring around a dead center instead of a rectangular grid: columns become sectors (clockwise from 12 o'clock), rows become concentric rings. The pointer deadzone doubles as the dead-center radius.",
        ("vm_radial", node_id.0))
    {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("menu_radial".into(), serde_json::json!(radial));
            // Legacy leftovers from when radial forced a synthetic strip.
            node.params.remove("zone_grid_stash");
            node.params.remove("radial_count");
        }
    }
    {
        let mut swatches = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(cells[3])
                .layout(egui::Layout::left_to_right(egui::Align::Center))
                .id_salt(("vm_colours", node_id.0)),
        );
        show_zone_color_row(node_id, &mut swatches, snarl);
    }
    // Summon the menu overlay in position-edit mode for THIS menu.
    let outer_uid = super::viewer::root_subpatch_id(automap_parent).map(|n| n.0);
    let editing = crate::menu_overlay::menu_edit_target(ui.ctx()) == Some((outer_uid, node_id.0));
    if ui
        .put(cells[4], egui::Button::selectable(editing, super::viewer::simple_bodies::control_text("🖵 position on screen…")).frame_when_inactive(true))
        .on_hover_text("Place / size this menu on the screen: it appears as an editable overlay widget — drag to move, corner to resize, Esc / Done saves.")
        .clicked()
    {
        crate::menu_overlay::request_menu_edit(ui.ctx(), outer_uid, node_id.0);
    }
}

/// The pinnable "options" element in the body: what points at the zones (additive sources,
/// the gyro's axes and sensitivity), then how the menu shows and picks. The fields publish in
/// `show_menu_options_row`'s order - the nav's field list for the element mirrors it - with
/// the same touch / gyro gating.
fn menu_options_pills(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, w: f32) {
    use super::viewer::curve_rows::{pill_bands, pill_combo, real_pill, segmented, split, word_toggle, word_w};
    const GAP: f32 = super::viewer::kit::GAP;
    let Some(node) = snarl.get_node(node_id) else { return };
    let legacy = pstr(node, "pointer_source", "left_stick").to_string();
    let mut src_ls = node.params.get("ptr_ls").and_then(|v| v.as_bool()).unwrap_or(legacy == "left_stick");
    let mut src_rs = node.params.get("ptr_rs").and_then(|v| v.as_bool()).unwrap_or(legacy == "right_stick");
    let mut src_touch = node.params.get("ptr_touch").and_then(|v| v.as_bool()).unwrap_or(legacy == "touch1" || legacy == "touch2");
    let touch_which = pstr(node, "ptr_touch_which", if legacy == "touch2" { "touch2" } else { "touch1" }).to_string();
    let mut src_gyro = node.params.get("ptr_gyro").and_then(|v| v.as_bool()).unwrap_or(false);
    let gyro_axes = pstr(node, "ptr_gyro_axes", "pitch_yaw").to_string();
    let mut gyro_sens = pf32(node, "ptr_gyro_sens", 4.0);
    let act = pstr(node, "activation_mode", "hold").to_string();
    let sel_on = pstr(node, "select_on", "release").to_string();
    let mut dz = pf32(node, "pointer_deadzone", 0.25);
    let mut linger = pf32(node, "select_linger", 0.5);
    let mut sticky = pbool(node, "hover_sticky", true);
    let mut set: Vec<(&str, serde_json::Value)> = Vec::new();
    let mut rects: Vec<egui::Rect> = Vec::new();

    // What points: every lit source ADDS UP, so a stick and the gyro can steer together.
    let words = ["LS", "RS", "touch", "gyro"];
    let ww: Vec<f32> = words.iter().map(|t| word_w(ui, t)).collect();
    // Laid out for the sources as they were when the frame began: a switch flipped below
    // changes the bands next frame.
    let (touch_shown, gyro_shown) = (src_touch, src_gyro);
    let mut mins = vec![ww.iter().sum::<f32>() + GAP * 3.0];
    if touch_shown {
        mins.push(130.0);
    }
    if gyro_shown {
        mins.extend([180.0, 90.0]);
    }
    let cells = pill_bands(ui, w, &mins);
    const SRC_TIP: &str = "Which analog inputs point at zones while the menu is open — every lit source ADDS UP, so a stick and the gyro can steer together. The wired Pointer inlet overrides them all.";
    let s = split(cells[0], &ww);
    if word_toggle(ui, s[0], &mut src_ls, "LS", SRC_TIP, ("vm_ls", node_id.0)) {
        set.push(("ptr_ls", serde_json::json!(src_ls)));
    }
    rects.push(s[0]);
    if word_toggle(ui, s[1], &mut src_rs, "RS", SRC_TIP, ("vm_rs", node_id.0)) {
        set.push(("ptr_rs", serde_json::json!(src_rs)));
    }
    rects.push(s[1]);
    if word_toggle(ui, s[2], &mut src_touch, "touch", SRC_TIP, ("vm_touch", node_id.0)) {
        set.push(("ptr_touch", serde_json::json!(src_touch)));
    }
    rects.push(s[2]);
    let mut k = 1;
    if touch_shown {
        const WHICH_TIP: &str = "Which touchpad points.";
        if let Some(v) = segmented(ui, cells[k], &touch_which.as_str(), &[("touch1", "touch 1", WHICH_TIP), ("touch2", "touch 2", WHICH_TIP)], ("vm_touch_which", node_id.0)) {
            set.push(("ptr_touch_which", serde_json::json!(v)));
        }
        rects.push(cells[k]);
        k += 1;
    }
    if word_toggle(ui, s[3], &mut src_gyro, "gyro",
        "Tilt to point: rotation accumulates while the menu is open and resets on every open, so the pointer always starts centered.",
        ("vm_gyro", node_id.0))
    {
        set.push(("ptr_gyro", serde_json::json!(src_gyro)));
    }
    rects.push(s[3]);
    if gyro_shown {
        const AXES_TIP: &str = "Which two of the pad's rotations move the pointer.";
        if let Some(v) = segmented(ui, cells[k], &gyro_axes.as_str(), &[("pitch_yaw", "pitch + yaw", AXES_TIP), ("pitch_roll", "pitch + roll", AXES_TIP)], ("vm_gyro_axes", node_id.0)) {
            set.push(("ptr_gyro_axes", serde_json::json!(v)));
        }
        rects.push(cells[k]);
        let shown = format!("{gyro_sens:.2}");
        if real_pill(ui, node_id, "vm_gyro_sens", cells[k + 1], "×|gyro ×", &mut gyro_sens, &shown, 0.5..=8.0, 0.02,
            "Gyro pointer sensitivity. 1 ≈ 10× the raw rotation rate; raise it if the gyro feels too weak to reach the zones.")
        {
            set.push(("ptr_gyro_sens", serde_json::json!(gyro_sens)));
        }
        rects.push(cells[k + 1]);
    }

    // How the menu shows and picks.
    let cells = pill_bands(ui, w, &[130.0, 140.0, 120.0, 110.0, word_w(ui, "sticky")]);
    let shows: [(String, &str); 4] = [
        ("hold".to_string(), "hold"),
        ("toggle".to_string(), "toggle"),
        ("touch".to_string(), "touch"),
        ("never".to_string(), "never"),
    ];
    if let Some(v) = pill_combo(ui, cells[0], "show", &act, &shows,
        "How the Show input opens the menu.\nHold: open while held. Toggle: press opens, press closes.\nTouch: open while a finger rests on the touchpad.\nNever: the menu never appears — Next / Previous pick the cells without showing them.",
        ("vm_show_pill", node_id.0))
    {
        set.push(("activation_mode", serde_json::json!(v)));
    }
    rects.push(cells[0]);
    let picks: [(String, &str); 3] = [("release".to_string(), "release"), ("press".to_string(), "press"), ("click".to_string(), "click")];
    if let Some(v) = pill_combo(ui, cells[1], "select", &sel_on, &picks,
        "What commits the highlighted zone.\nRelease: letting go of Show / touch. Press: the Select input.\nClick: the touchpad click.",
        ("vm_select_pill", node_id.0))
    {
        set.push(("select_on", serde_json::json!(v)));
    }
    rects.push(cells[1]);
    let shown = format!("{dz:.2}");
    if real_pill(ui, node_id, "vm_dz", cells[2], "dz|deadzone", &mut dz, &shown, 0.0..=0.9, 0.004,
        "Pointer deflection below this doesn't move the highlight.")
    {
        set.push(("pointer_deadzone", serde_json::json!(dz)));
    }
    rects.push(cells[2]);
    let shown = format!("{linger:.2} s");
    if real_pill(ui, node_id, "vm_linger", cells[3], "linger", &mut linger, &shown, 0.0..=10.0, 0.02,
        "After a selection is accepted the menu hides immediately, but the chosen zone cell stays on screen this long, then fades out. 0 = the whole menu disappears at once.")
    {
        set.push(("select_linger", serde_json::json!(linger)));
    }
    rects.push(cells[3]);
    if word_toggle(ui, cells[4], &mut sticky, "sticky",
        "Keep the last highlighted zone when the pointer returns to the deadzone, so flick-and-release selection works. Off: the highlight clears, and releasing inside the deadzone selects nothing.",
        ("vm_sticky", node_id.0))
    {
        set.push(("hover_sticky", serde_json::json!(sticky)));
    }
    rects.push(cells[4]);
    super::viewer::publish_nav_field_rects(ui, node_id, &rects);
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set {
                node.params.insert(k.to_string(), v);
            }
        }
    }
}

/// Menu options — TWO rows, both inside the single pinnable "options"
/// element: pointer sources (additive checkboxes, gyro sensitivity) on top,
/// session behaviour (open/select/deadzone/linger/sticky) below. Rendered in
/// the node body AND as a pinnable layout element (viewer.rs "options" arm),
/// so it must stay self-contained.
pub(crate) fn show_menu_options_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    // ── Row 1: pointer sources. ADDITIVE — every checked input sums into
    // one deflection vector (the wired Pointer inlet overrides them all).
    // Defaults derive from the legacy single-choice `pointer_source`, so
    // patches from before the checkboxes keep pointing the same way. ──
    let (src_ls, src_rs, src_touch, touch_which, src_gyro, gyro_axes, gyro_sens) = {
        let Some(node) = snarl.get_node(node_id) else { return };
        let legacy = pstr(node, "pointer_source", "left_stick").to_string();
        (
            node.params.get("ptr_ls").and_then(|v| v.as_bool()).unwrap_or(legacy == "left_stick"),
            node.params.get("ptr_rs").and_then(|v| v.as_bool()).unwrap_or(legacy == "right_stick"),
            node.params.get("ptr_touch").and_then(|v| v.as_bool())
                .unwrap_or(legacy == "touch1" || legacy == "touch2"),
            pstr(node, "ptr_touch_which", if legacy == "touch2" { "touch2" } else { "touch1" })
                .to_string(),
            node.params.get("ptr_gyro").and_then(|v| v.as_bool()).unwrap_or(false),
            pstr(node, "ptr_gyro_axes", "pitch_yaw").to_string(),
            pf32(node, "ptr_gyro_sens", 4.0),
        )
    };
    let mut set: Vec<(&str, serde_json::Value)> = Vec::new();
    // Per-field interactive rects for the gamepad-nav focus glow, captured in the
    // SAME order (and with the SAME conditional touch/gyro gating) as this
    // element's `nav_element_fields` arm — see that arm's lockstep note.
    let mut field_rects: Vec<egui::Rect> = Vec::new();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Driven by").small().weak())
            .on_hover_text("Which analog inputs point at zones while the menu is open —\nevery checked source ADDS UP, so a stick and the gyro can steer\ntogether. The wired Pointer inlet overrides them all.");
        let mut b = src_ls;
        let r = ui.checkbox(&mut b, "LS");
        if r.changed() { set.push(("ptr_ls", serde_json::json!(b))); }
        field_rects.push(r.rect);
        let mut b = src_rs;
        let r = ui.checkbox(&mut b, "RS");
        if r.changed() { set.push(("ptr_rs", serde_json::json!(b))); }
        field_rects.push(r.rect);
        let mut b = src_touch;
        let r = ui.checkbox(&mut b, "Touch");
        if r.changed() { set.push(("ptr_touch", serde_json::json!(b))); }
        field_rects.push(r.rect);
        if src_touch {
            let r = egui::ComboBox::from_id_salt((node_id, "menu_touch_which"))
                .selected_text(if touch_which == "touch2" { "Touch 2" } else { "Touch 1" })
                .width(74.0)
                .show_ui(ui, |ui| {
                    for (val, label) in [("touch1", "Touch 1"), ("touch2", "Touch 2")] {
                        if ui.selectable_label(touch_which == val, label).clicked() {
                            set.push(("ptr_touch_which", serde_json::json!(val)));
                        }
                    }
                });
            field_rects.push(r.response.rect);
        }
        let mut b = src_gyro;
        let r = ui.checkbox(&mut b, "Gyro")
            .on_hover_text("Tilt to point: rotation accumulates while the menu is open and\nresets on every open, so the pointer always starts centered.");
        if r.changed() { set.push(("ptr_gyro", serde_json::json!(b))); }
        field_rects.push(r.rect);
        if src_gyro {
            let r = egui::ComboBox::from_id_salt((node_id, "menu_gyro_axes"))
                .selected_text(if gyro_axes == "pitch_roll" { "Pitch+Roll" } else { "Pitch+Yaw" })
                .width(92.0)
                .show_ui(ui, |ui| {
                    for (val, label) in [("pitch_yaw", "Pitch+Yaw"), ("pitch_roll", "Pitch+Roll")] {
                        if ui.selectable_label(gyro_axes == val, label).clicked() {
                            set.push(("ptr_gyro_axes", serde_json::json!(val)));
                        }
                    }
                });
            field_rects.push(r.response.rect);
            ui.label(egui::RichText::new("×").small().weak());
            let mut sens = gyro_sens;
            let r = ui.add(egui::DragValue::new(&mut sens).range(0.5..=8.0).speed(0.05))
                .on_hover_text("Gyro pointer sensitivity. 1 ≈ 10× the raw rotation rate;\nraise it if the gyro feels too weak to reach the zones.");
            if r.changed() {
                set.push(("ptr_gyro_sens", serde_json::json!(sens)));
            }
            field_rects.push(r.rect);
        }
    });

    // ── Row 2: session behaviour ──
    let (act, sel_on, dz, linger, sticky) = {
        let Some(node) = snarl.get_node(node_id) else { return };
        (
            pstr(node, "activation_mode", "hold").to_string(),
            pstr(node, "select_on", "release").to_string(),
            pf32(node, "pointer_deadzone", 0.25),
            pf32(node, "select_linger", 0.5),
            pbool(node, "hover_sticky", true),
        )
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Show").small().weak())
            .on_hover_text("How the Show input opens the menu.\nHold: open while held. Toggle: press opens, press closes.\nTouch: open while a finger rests on the touchpad.\nNever: the menu never appears — Next / Previous pick the cells\nwithout showing them.");
        let r = egui::ComboBox::from_id_salt((node_id, "menu_act"))
            .selected_text(match act.as_str() {
                "toggle" => "Toggle", "touch" => "Touch", "never" => "Never", _ => "Hold",
            })
            .width(70.0)
            .show_ui(ui, |ui| {
                for (val, label) in [("hold", "Hold"), ("toggle", "Toggle"), ("touch", "Touch"), ("never", "Never")] {
                    if ui.selectable_label(act == val, label).clicked() {
                        set.push(("activation_mode", serde_json::json!(val)));
                    }
                }
            });
        field_rects.push(r.response.rect);
        ui.label(egui::RichText::new("Select").small().weak())
            .on_hover_text("What commits the highlighted zone.\nRelease: letting go of Show/touch. Press: the Select input.\nClick: the touchpad click.");
        let r = egui::ComboBox::from_id_salt((node_id, "menu_sel_on"))
            .selected_text(match sel_on.as_str() { "press" => "Press", "click" => "Click", _ => "Release" })
            .width(76.0)
            .show_ui(ui, |ui| {
                for (val, label) in [("release", "Release"), ("press", "Press"), ("click", "Click")] {
                    if ui.selectable_label(sel_on == val, label).clicked() {
                        set.push(("select_on", serde_json::json!(val)));
                    }
                }
            });
        field_rects.push(r.response.rect);
        ui.label(egui::RichText::new("Deadzone").small().weak());
        let mut dz_val = dz;
        let r = ui.add(egui::DragValue::new(&mut dz_val).range(0.0..=0.9).speed(0.01))
            .on_hover_text("Pointer deflection below this doesn't move the highlight.");
        if r.changed() {
            set.push(("pointer_deadzone", serde_json::json!(dz_val)));
        }
        field_rects.push(r.rect);
        ui.label(egui::RichText::new("Linger").small().weak());
        let mut lin = linger;
        let r = ui.add(egui::DragValue::new(&mut lin).range(0.0..=10.0).speed(0.05).suffix(" s"))
            .on_hover_text("After a selection is accepted the menu hides immediately, but the\nchosen zone cell stays on screen this long, then fades out.\n0 = the whole menu disappears at once.");
        if r.changed() {
            set.push(("select_linger", serde_json::json!(lin)));
        }
        field_rects.push(r.rect);
        let mut stk = sticky;
        let r = ui.checkbox(&mut stk, "Sticky")
            .on_hover_text("Keep the last highlighted zone when the pointer returns to the\ndeadzone, so flick-and-release selection works. Off: the highlight\nclears, and releasing inside the deadzone selects nothing.");
        if r.changed() {
            set.push(("hover_sticky", serde_json::json!(stk)));
        }
        field_rects.push(r.rect);
    });
    super::viewer::publish_nav_field_rects(ui, node_id, &field_rects);
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
}

/// The options' Style 2 look, pinned: the body's two bands, drawn inside `pin_style2::fit`.
pub(crate) fn render_menu_options_style2(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let w = ui.available_width();
    menu_options_pills(inner_id, ui, snarl, w);
}

/// Pinned renderer for the menu options ("options" element, now TWO rows) —
/// standard row scaling per memory/pinned_widget_scaling.md: height drives
/// text scale, no flexible element (the combos follow the scaled style).
pub(crate) fn render_menu_options_pinned(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    super::viewer::apply_widget_scale(ui, container, egui::vec2(560.0, 48.0));
    ui.vertical(|ui| {
        show_menu_options_row(inner_id, ui, snarl);
    });
}
