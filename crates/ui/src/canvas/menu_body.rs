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
//! inlet, and its Show/Select controls are macro-style named targets
//! (menu:{menu_id}_show / _sel — assignable from Remapper / Touch Zones /
//! Lean via the Special picker, where the menu wears its name + icon) plus
//! the wired Show/Select pins as alternates. It renders on its own screen
//! overlay while open. The zone-live highlight here comes from the node's own
//! eval mirror (Open/Hover outputs), not from touch pins.
//!
//! Menu-specific params (on top of the TZ set):
//!   menu_id — stable identity behind the target pin ids (save-format)
//!   menu_name, menu_icon, menu_icon_svg — identity (Macro pattern)
//!   activation_mode: "hold" | "toggle" | "touch"
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
/// when the last entry goes away.
fn write_zone_meta_entry(node: &mut NodeData, zone: u32, meta: &ZoneMeta) {
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
    p.line_segment([origin, pos], egui::Stroke::new(2.0, accent.gamma_multiply(0.35)));
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

pub(crate) fn show_menu_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &LiveSignals,
    automap_parent: Option<&AutomapGlowParent<'_>>,
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

    let field_w = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_w").and_then(|v| v.as_f64())).unwrap_or(300.0) as f32;
    let field_h = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_h").and_then(|v| v.as_f64())).unwrap_or(180.0) as f32;

    ui.vertical(|ui| {
        // ── Menu options rows (identity + mode + Suppress/Radial live in the
        //    node HEADER; these two rows are pinnable to layouts / overlay) ──
        let opts_area = ui.vertical(|ui| {
            show_menu_options_row(node_id, ui, snarl);
        }).response.rect;
        register_exposable_element(ui, node_id, "options", opts_area);

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
                node_id, 0, true, true, mapping, ui, snarl,
                &zone_live, &visuals, accent, field_w, field_h,
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
                        f, egui::Color32::from_gray(210));
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

        // ── Per-zone icon + name override: the icon replaces the zone's
        // mapping icons everywhere it's painted (field, pinned widget, menu
        // overlay) — keyboard bindings rarely read as "what this zone does";
        // the name draws alongside whatever icons show. ──
        {
            let ids = super::viewer::tz_field_tree(snarl, node_id, 0).ids();
            let max_id = ids.iter().copied().max().unwrap_or(0);
            let sel = snarl.get_node(node_id)
                .and_then(|n| n.params.get("sel_zone").and_then(|v| v.as_u64()))
                .unwrap_or(0) as u32;
            let zone = if ids.contains(&sel) { sel } else { ids.first().copied().unwrap_or(0) };
            let meta = snarl.get_node(node_id)
                .map(|n| menu_zone_meta(n).get(&zone).cloned().unwrap_or_default())
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Zone").small().weak());
                let mut z_edit = zone as u64;
                if ui.add(egui::DragValue::new(&mut z_edit).range(0..=max_id as u64).speed(0.05))
                    .on_hover_text("Which zone the icon/name override applies to.\nClicking a zone in the field selects it too.")
                    .changed()
                {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("sel_field".into(), serde_json::json!(0u64));
                        node.params.insert("sel_zone".into(), serde_json::json!(z_edit));
                    }
                }
                ui.label(egui::RichText::new("icon").small().weak())
                    .on_hover_text("Override the icons shown on this zone with one picked icon\n(shared icon set, or a custom SVG embedded into the patch) —\nmapping destinations like keyboard keys rarely read clearly.");
                if let Some((k, s)) = icon_picker_button(
                    ui, egui::Id::new((node_id, "vm_zone_icon", zone)), &meta.icon, &meta.svg)
                {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        write_zone_meta_entry(node, zone, &ZoneMeta {
                            icon: k, svg: s, label: meta.label.clone(),
                        });
                    }
                }
                ui.label(egui::RichText::new("name").small().weak())
                    .on_hover_text("Zone name, drawn on the zone alongside its icons.");
                let mut label_edit = meta.label.clone();
                if ui.add(egui::TextEdit::singleline(&mut label_edit)
                    .desired_width(90.0)
                    .id_salt((node_id, "vm_zone_name", zone)))
                    .changed()
                {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        write_zone_meta_entry(node, zone, &ZoneMeta {
                            icon: meta.icon.clone(), svg: meta.svg.clone(), label: label_edit,
                        });
                    }
                }
                if (!meta.icon.is_empty() || !meta.svg.is_empty() || !meta.label.is_empty())
                    && ui.small_button("✕").on_hover_text("Remove this zone's icon + name override").clicked()
                {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        write_zone_meta_entry(node, zone, &ZoneMeta::default());
                    }
                }
            });
        }

        // ── Mapping mode: zone-tab card list (TZ machinery, separately pinnable) ──
        if mapping {
            ui.add_space(6.0);
            let cards_area = ui.vertical(|ui| {
                render_touch_zone_cards(node_id, ui, snarl, &visuals, accent, live_signals, automap_parent);
            }).response.rect;
            register_exposable_element(ui, node_id, "cards", cards_area);
        }

        // Summon the menu overlay in position-edit mode for THIS menu.
        let outer_uid = super::viewer::root_subpatch_id(automap_parent).map(|n| n.0);
        let editing = crate::menu_overlay::menu_edit_target(ui.ctx())
            == Some((outer_uid, node_id.0));
        if ui.add(egui::Button::selectable(editing, "🖵 Position on screen…"))
            .on_hover_text("Place/size this menu on the screen: it appears as an editable\noverlay widget — drag to move, corner to resize, Esc/Done saves.")
            .clicked()
        {
            crate::menu_overlay::request_menu_edit(ui.ctx(), outer_uid, node_id.0);
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
        let mut pts = outer;
        inner.reverse();
        pts.extend(inner);
        shapes.push(egui::Shape::closed_line(pts, stroke));
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
    p.circle_stroke(geom.center, geom.r_in, egui::Stroke::new(1.0, colors.main.gamma_multiply(0.7)));

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
        p.layout_no_wrap(s.to_string(), egui::FontId::proportional(size), egui::Color32::WHITE)
            .size().x
    };
    let w = measure(text, base);
    if w <= max_w || w <= 0.0 {
        return (egui::FontId::proportional(base), text.to_string());
    }
    let scaled = (base * max_w / w).max(FLOOR);
    if scaled > FLOOR || measure(text, scaled) <= max_w {
        return (egui::FontId::proportional(scaled), text.to_string());
    }
    // Even at the floor it overflows — clip to whatever fits, plus an ellipsis.
    let mut s = String::new();
    for ch in text.chars() {
        if measure(&format!("{s}{ch}…"), FLOOR) > max_w { break; }
        s.push(ch);
    }
    let out = if s.is_empty() { "…".to_string() } else { format!("{s}…") };
    (egui::FontId::proportional(FLOOR), out)
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
        let f = egui::FontId::proportional(s);
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
    let font = egui::FontId::proportional(size);

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
        egui::Stroke::new(2.0, colors.hi)
    } else if is_sel {
        egui::Stroke::new(2.0, colors.hi.gamma_multiply(0.6))
    } else {
        egui::Stroke::new(1.0, colors.main)
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
    let label_col = if hovered { egui::Color32::WHITE } else { egui::Color32::from_gray(200) };
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
                egui::FontId::proportional(want), egui::Color32::WHITE).size().x;
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
                egui::FontId::proportional(ic * 0.6), egui::Color32::from_gray(170));
        }
    } else if !has_label {
        // Bare zone: faint index at the centre.
        p.text(
            icon_center, egui::Align2::CENTER_CENTER, format!("{zid}"),
            egui::FontId::proportional((rad * 0.30).clamp(11.0, 22.0)),
            if hovered { egui::Color32::WHITE } else { egui::Color32::from_gray(150) },
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
        p.rect_stroke(zr, 5.0, egui::Stroke::new(2.0, colors.hi), egui::StrokeKind::Inside);
    } else {
        p.rect_stroke(zr, 5.0, egui::Stroke::new(1.0, colors.main), egui::StrokeKind::Inside);
    }

    let meta = zone_meta.get(&zid);
    let label = meta.map(|m| m.label.as_str()).unwrap_or("");
    let label_col = if hovered { egui::Color32::WHITE } else { egui::Color32::from_gray(200) };
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
                    egui::FontId::proportional((ic_h * 0.5).clamp(11.0, 26.0)),
                    if hovered { egui::Color32::WHITE } else { egui::Color32::from_gray(150) },
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
                    egui::FontId::proportional(ic * 0.6), egui::Color32::from_gray(170));
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
    use flexinput_core::touchzones as tz;
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
    let geom = RingGeom::of(rect, deadzone, origin);
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

    // ── Border editor ─────────────────────────────────────────────────────
    let col_edges = super::viewer::tz_read_field_edges(snarl, node_id, 0, "col_edges");
    let row_edges = super::viewer::tz_read_field_edges(snarl, node_id, 0, "row_edges");
    // The origin seam (tree-u 0) is always present as a draggable border —
    // dragging it rotates the whole ring. It's drawn thicker to mark the
    // menu's angular origin. `key = [SEAM_KEY]` distinguishes it from real
    // dividers (no real divider path/edge index ever reaches 254).
    const SEAM_KEY: u8 = 254;
    let seam = RingBorder {
        angular: true, pos: 0.0, span_lo: 0.0, span_hi: 1.0,
        lo: 0.0, hi: 1.0, key: vec![SEAM_KEY], seam: true,
    };
    let mut borders: Vec<RingBorder> = vec![seam];
    if mapping {
        borders.extend(tree.dividers().iter().map(|d| RingBorder {
            angular: matches!(d.axis, tz::Axis::V),
            pos: d.pos,
            span_lo: d.span_lo,
            span_hi: d.span_hi,
            lo: d.lo,
            hi: d.hi,
            key: d.path.clone(),
            seam: false,
        }));
    } else {
        borders.extend(col_edges.iter().enumerate().map(|(i, &e)| RingBorder {
            angular: true,
            pos: e,
            span_lo: 0.0,
            span_hi: 1.0,
            lo: if i == 0 { 0.02 } else { col_edges[i - 1] + 0.04 },
            hi: if i + 1 == col_edges.len() { 0.98 } else { col_edges[i + 1] - 0.04 },
            key: vec![i as u8],
            seam: false,
        }));
        borders.extend(row_edges.iter().enumerate().map(|(i, &e)| RingBorder {
            angular: false,
            pos: e,
            span_lo: 0.0,
            span_hi: 1.0,
            lo: if i == 0 { 0.05 } else { row_edges[i - 1] + 0.06 },
            hi: if i + 1 == row_edges.len() { 0.95 } else { row_edges[i + 1] - 0.06 },
            key: vec![i as u8],
            seam: false,
        }));
    }

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
            let (lo, hi) = if mapping { (b.lo + 0.03, b.hi - 0.03) } else { (b.lo, b.hi) };
            let t = if lo <= hi { want.clamp(lo, hi) } else { (b.lo + b.hi) * 0.5 };
            if mapping {
                let mut nt = tree.clone();
                if nt.set_divider_t(&b.key, t) {
                    super::viewer::tz_set_field_tree(snarl, node_id, 0, &nt);
                }
            } else if let Some(node) = snarl.get_node_mut(node_id) {
                let (key, mut edges) = if b.angular {
                    ("col_edges", col_edges.clone())
                } else {
                    ("row_edges", row_edges.clone())
                };
                if let Some(e) = edges.get_mut(b.key[0] as usize) { *e = t; }
                node.params.insert(key.to_string(), serde_json::json!(edges));
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
            if mapping {
                // Recentre between the divider's IMMEDIATE neighbours (mirrors
                // the rectangular editor; the naïve parent-span midpoint
                // overshoots and squashes the next zone in a 3+-zone tree).
                let target = tree.centered_divider_pos(&b.key)
                    .unwrap_or((b.lo + b.hi) * 0.5);
                let mut nt = tree.clone();
                if nt.set_divider_t(&b.key, target) {
                    super::viewer::tz_set_field_tree(snarl, node_id, 0, &nt);
                }
            } else {
                // Ports mode: recenter between the neighbouring edges (or the
                // hub/rim at the ends), mirroring the flat editor.
                let i = b.key[0] as usize;
                let (key, src) = if b.angular {
                    ("col_edges", &col_edges)
                } else {
                    ("row_edges", &row_edges)
                };
                let lo = if i == 0 { 0.0 } else { src[i - 1] };
                let hi = if i + 1 == src.len() { 1.0 } else { src[i + 1] };
                let mut edges = src.clone();
                if let Some(e) = edges.get_mut(i) {
                    *e = (lo + hi) * 0.5;
                }
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert(key.to_string(), serde_json::json!(edges));
                }
            }
        }
    }

    // Gamepad-nav focus: the shared TZ line editor (`nav_drive_touch_zones`)
    // drives radial borders by the SAME (axis, line) index model as the flat
    // field — angular borders are the "column"/V axis (0), radial rings the
    // "row"/H axis (1), each counted per axis in border order (which matches the
    // driver's `dividers()` walk in mapping mode and the edge-array index in
    // ports mode). Read the published focus to highlight the divider, and expose
    // per-border hit-rects for the RS cursor: a small box at each border's
    // midpoint (an arc has no useful bounding rect, so a point target replaces
    // the flat field's full-span strip). The origin seam is never a nav divider.
    let nav_tz: Option<(u64, u64, u64, u64, bool)> =
        ui.ctx().data(|d| d.get_temp(egui::Id::new(("gp_nav_tz", node_id.0))));
    let cur_pass = ui.ctx().cumulative_pass_nr();
    let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    let mut nav_line_rects: Vec<(u8, u32, egui::Rect)> = Vec::new();
    let (mut vcount, mut hcount) = (0u32, 0u32);

    // Border strokes (hot = hovered or mid-drag). The origin seam is drawn
    // thicker so it reads as the ring's reference edge.
    for (i, b) in borders.iter().enumerate() {
        let hot = dragged_border == Some(i)
            || (dragged_border.is_none()
                && ptr.and_then(hit_at) == Some(i));
        // Per-axis nav index + midpoint cursor hit-box for non-seam borders.
        let nav_ax_line = if b.seam {
            None
        } else {
            let axis: u8 = if b.angular { 0 } else { 1 };
            let line = if b.angular { let n = vcount; vcount += 1; n }
                       else         { let n = hcount; hcount += 1; n };
            let mid = (b.span_lo + b.span_hi) * 0.5;
            let c = if b.angular { geom.point(b.pos, mid) } else { geom.point(mid, b.pos) };
            nav_line_rects.push((axis, line, to_global * egui::Rect::from_center_size(c, egui::vec2(18.0, 18.0))));
            Some((axis as u64, line as u64))
        };
        let nav_grab = match (nav_tz, nav_ax_line) {
            (Some((pass, f, a, l, g)), Some((ax, ln)))
                if cur_pass.saturating_sub(pass) <= 2 && f == 0 && a == ax && l == ln => Some(g),
            _ => None,
        };
        let stroke = if let Some(grabbed) = nav_grab {
            egui::Stroke::new(3.0,
                if grabbed { egui::Color32::from_rgb(90, 220, 120) } else { accent })
        } else if hot {
            egui::Stroke::new(if b.seam { 4.0 } else { 2.0 }, accent)
        } else if b.seam {
            egui::Stroke::new(3.0, visuals.weak_text_color().gamma_multiply(1.4))
        } else {
            egui::Stroke::new(1.0, visuals.weak_text_color())
        };
        paint_ring_border(&painter, &geom, b, stroke);
    }
    ui.ctx().data_mut(|d| d.insert_temp(
        egui::Id::new(("gp_nav_tz_lines", node_id.0, 0usize)),
        (cur_pass, nav_line_rects)));
    if dragged_border.is_some() || ptr.and_then(hit_at).is_some() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grab);
    }

    // "−" / "+" mini buttons.
    let mut grid_op: Option<tz::GridOp> = None;
    let mut tree_sub: Option<(f32, f32, tz::Axis, bool)> = None;
    let mut tree_rem: Option<Vec<u8>> = None;
    if mapping {
        // Both affordances are dynamic: hovering a divider offers "−" to remove
        // it; hovering a zone away from dividers offers "+" to split near the
        // closest edge. The origin seam is never removable — it just rotates.
        if let Some(bi) = ptr.and_then(hit_at).filter(|&bi| !borders[bi].seam) {
            let b = &borders[bi];
            let mid = (b.span_lo + b.span_hi) * 0.5;
            let c = if b.angular { geom.point(b.pos, mid) } else { geom.point(mid, b.pos) };
            if super::viewer::tz_mini_button(ui, &painter,
                ui.id().with((node_id, "vmrm", bi)), c, "−", accent, &visuals)
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
                            ui.id().with((node_id, "vmrp")), pos, "+", accent, &visuals)
                        {
                            tree_sub = Some((cx, cy, axis, new_low));
                        }
                    }
                }
            }
        }
    } else if let Some(p) = ptr {
        // Ports mode: full cuts only. The −/+ cluster appears AT the pointer
        // when it hovers near a border (a fixed spot makes no sense on a
        // circle); the rim, the hub edge, and the 12-o'clock seam grow "+"
        // for the first ring / sector cut.
        let (u, v) = geom.unit(p);
        let cols = col_edges.len() + 1;
        let rows = row_edges.len() + 1;
        // The seam only rotates — skip its −/+ cluster (fall through to the
        // rim/hub/seam add affordances below).
        if let Some(bi) = hit_at(p).filter(|&bi| !borders[bi].seam) {
            let b = &borders[bi];
            let line = b.key[0] as usize + 1; // grid line index (1-based band boundary)
            let (c_minus, c_plus_a, c_plus_b) = if b.angular {
                let vv = v.clamp(0.1, 0.9);
                let off = 20.0 / (std::f32::consts::TAU * geom.radius(vv)).max(1e-3);
                (geom.point(b.pos, vv),
                 geom.point(b.pos - off, vv),
                 geom.point(b.pos + off, vv))
            } else {
                let uu = u.min(0.9999);
                let off = 20.0 / (geom.r_out - geom.r_in).max(1e-3);
                (geom.point(uu, b.pos),
                 geom.point(uu, (b.pos - off).max(0.02)),
                 geom.point(uu, (b.pos + off).min(0.98)))
            };
            let (ins_lo, ins_hi, rem) = if b.angular {
                (tz::GridOp::InsertCol(line - 1), tz::GridOp::InsertCol(line),
                 tz::GridOp::RemoveCol(line - 1))
            } else {
                (tz::GridOp::InsertRow(line - 1), tz::GridOp::InsertRow(line),
                 tz::GridOp::RemoveRow(line - 1))
            };
            if super::viewer::tz_mini_button(ui, &painter,
                ui.id().with((node_id, "vmpa", bi)), c_plus_a, "+", accent, &visuals)
            { grid_op = Some(ins_lo); }
            if super::viewer::tz_mini_button(ui, &painter,
                ui.id().with((node_id, "vmpb", bi)), c_plus_b, "+", accent, &visuals)
            { grid_op = Some(ins_hi); }
            if super::viewer::tz_mini_button(ui, &painter,
                ui.id().with((node_id, "vm-", bi)), c_minus, "−", accent, &visuals)
            { grid_op = Some(rem); }
        } else if (0.0..=1.05).contains(&v) {
            let mag = (p - geom.center).length();
            let uc = u.min(0.9999);
            if geom.r_out - mag < 22.0 && mag < geom.r_out + 8.0 {
                // Near the rim → add an outer ring.
                if super::viewer::tz_mini_button(ui, &painter,
                    ui.id().with((node_id, "vmbr")),
                    geom.point(uc, 1.0 - 14.0 / (geom.r_out - geom.r_in).max(1e-3)),
                    "+", accent, &visuals)
                { grid_op = Some(tz::GridOp::InsertRow(rows - 1)); }
            } else if mag - geom.r_in < 16.0 && mag > geom.r_in - 8.0 {
                // Near the hub edge → add an inner ring.
                if super::viewer::tz_mini_button(ui, &painter,
                    ui.id().with((node_id, "vmbh")),
                    geom.point(uc, 14.0 / (geom.r_out - geom.r_in).max(1e-3)),
                    "+", accent, &visuals)
                { grid_op = Some(tz::GridOp::InsertRow(0)); }
            } else {
                // Near the 12-o'clock seam → first/next sector cut.
                let du = uc.min(1.0 - uc);
                let vv = v.clamp(0.1, 0.9);
                if du * std::f32::consts::TAU * geom.radius(vv) < 20.0 {
                    let side = 16.0 / (std::f32::consts::TAU * geom.radius(vv)).max(1e-3);
                    if super::viewer::tz_mini_button(ui, &painter,
                        ui.id().with((node_id, "vmbs")),
                        geom.point(if uc < 0.5 { side } else { 1.0 - side }, vv),
                        "+", accent, &visuals)
                    {
                        grid_op = Some(tz::GridOp::InsertCol(
                            if uc < 0.5 { 0 } else { cols - 1 }));
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
    if let Some(op) = grid_op {
        super::viewer::tz_restructure(node_id, 0, op, snarl);
        regenerate_menu_ports(node_id, snarl);
    }

    // Zone click-select (mapping mode) — only when not grabbing a border.
    if mapping && resp.clicked() && drag.is_none()
        && resp.interact_pointer_pos().map(|p| hit_at(p).is_none()).unwrap_or(false)
    {
        if let Some(z) = picked {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("sel_field".into(), serde_json::json!(0u64));
                node.params.insert("sel_zone".into(), serde_json::json!(z as u64));
            }
        }
    }

    // ── Resize grip (bottom-right corner) — same affordance as the flat
    // field, writing the shared field size (the ring diameter follows the
    // smaller side). Registered last so it wins the drag over the border
    // editor; the corner sits outside the ring, so no border is in reach. ──
    {
        let hs = 14.0;
        let handle = egui::Rect::from_min_max(
            egui::pos2(rect.right() - hs, rect.bottom() - hs),
            egui::pos2(rect.right(), rect.bottom()),
        );
        let hr = ui.interact(handle, ui.id().with((node_id, "vmresize")), egui::Sense::drag());
        if hr.hovered() || hr.dragged() {
            hr.clone().on_hover_cursor(egui::CursorIcon::ResizeNwSe);
        }
        let grip = if hr.hovered() || hr.dragged() { accent } else { visuals.weak_text_color() };
        for k in 1..=3 {
            let o = k as f32 * 3.5;
            painter.line_segment(
                [egui::pos2(rect.right() - o, rect.bottom()), egui::pos2(rect.right(), rect.bottom() - o)],
                egui::Stroke::new(1.0, grip),
            );
        }
        if hr.dragged() {
            let d = hr.drag_delta();
            let nw = (field_w + d.x).clamp(200.0, 900.0);
            let nh = (field_h + d.y).clamp(120.0, 600.0);
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("field_w".to_string(), serde_json::json!(nw));
                node.params.insert("field_h".to_string(), serde_json::json!(nh));
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
        egui::Button::new(egui::RichText::new("◌").size(14.0))
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
                            .fit_to_exact_size(egui::vec2(CELL - 6.0, CELL - 6.0))
                            .tint(egui::Color32::WHITE))
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
                if shown == 0 {
                    ui.label(egui::RichText::new("No matching icons").weak());
                }
            });
        });
    });
    new_icon
}

/// Rendered in the node HEADER (viewer.rs dispatches here for module.menu):
/// icon + editable name (the menu's face in mapping pickers) + the
/// Ports/Mapping mode switch.
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
            .desired_width(110.0));
        if resp.changed() { name_changed = true; }

        // Ports/Mapping mode switch.
        let mapping = snarl.get_node(node_id)
            .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");
        let mut want_mapping = mapping;
        if ui.selectable_label(!want_mapping, "Ports")
            .on_hover_text("Expose typed X / Y / Active outputs per zone + Select.")
            .clicked() { want_mapping = false; }
        if ui.selectable_label(want_mapping, "Mapping")
            .on_hover_text("Map each zone to gamepad/key/stick outputs with internal cards.")
            .clicked() { want_mapping = true; }
        if want_mapping != mapping {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("zone_mode".to_string(),
                    serde_json::json!(if want_mapping { "mapping" } else { "ports" }));
            }
            regenerate_menu_ports(node_id, snarl);
        }

        // Suppression + Radial live in the header with the mode switch — they
        // shape what the module IS, not how a session behaves. Suppression is
        // one 3-way choice stored across `suppress_while_open` (off vs on) and
        // `suppress_mode` (partial vs full), so older patches load unchanged.
        let (suppress, sup_mode, radial) = snarl.get_node(node_id)
            .map(|n| (pbool(n, "suppress_while_open", true),
                      pstr(n, "suppress_mode", "partial").to_string(),
                      pbool(n, "menu_radial", false)))
            .unwrap_or((true, "partial".to_string(), false));
        let cur = if !suppress { "off" }
            else if sup_mode == "full" { "full" }
            else if sup_mode == "latch" { "latch" }
            else { "partial" };
        let mut sel = cur.to_string();
        egui::ComboBox::from_id_salt((node_id, "menu_suppress_mode"))
            .selected_text(match sel.as_str() {
                "off" => "Passthrough",
                "full" => "Full suppress",
                "latch" => "Latch suppress",
                _ => "Active suppress",
            })
            .width(122.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut sel, "off".to_string(), "Passthrough")
                    .on_hover_text("Menu drivers keep flowing to the rest of the patch\nwhile the menu is open.");
                ui.selectable_value(&mut sel, "partial".to_string(), "Active suppress")
                    .on_hover_text("Block a driver AT THE SOURCE only while it's actively\nsteering the menu (stick past deadzone / finger down /\ngyro cursor out of the deadzone), so it reaches only the\nmenu's navigation. Idle enabled drivers still pass; a\nblocked one returns the moment it's back at rest.");
                ui.selectable_value(&mut sel, "latch".to_string(), "Latch suppress")
                    .on_hover_text("The FIRST driver to engage owns the menu: it alone\nsteers and is blocked at the source. Every other driver\nis ignored by the menu and keeps flowing to the game,\nuntil the owner disengages (stick back to deadzone /\nfinger up / gyro cursor re-centred) — then the next\nengaged driver can take over.");
                ui.selectable_value(&mut sel, "full".to_string(), "Full suppress")
                    .on_hover_text("Block every enabled driver AT THE SOURCE for as long\nas the menu is open — none of them reach a mouse\nmapping, another module, or the virtual pad.");
            });
        if sel != cur {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("suppress_while_open".into(), serde_json::json!(sel != "off"));
                if sel != "off" {
                    node.params.insert("suppress_mode".into(), serde_json::json!(sel));
                }
            }
        }
        let mut want_radial = radial;
        if ui.checkbox(&mut want_radial, "Radial")
            .on_hover_text("Show the same zones as a sector ring around a dead center\ninstead of a rectangular grid: columns become sectors\n(clockwise from 12 o'clock), rows become concentric rings.\nThe pointer deadzone doubles as the dead-center radius.")
            .changed()
        {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("menu_radial".into(), serde_json::json!(want_radial));
                // Legacy leftovers from when radial forced a synthetic strip.
                node.params.remove("zone_grid_stash");
                node.params.remove("radial_count");
            }
        }

        // Theme swatches live in the header (compact, self-describing on hover)
        // so their picker pops away from the zone field they recolour.
        ui.separator();
        show_zone_color_row(node_id, ui, snarl);
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
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Driven by").small().weak())
            .on_hover_text("Which analog inputs point at zones while the menu is open —\nevery checked source ADDS UP, so a stick and the gyro can steer\ntogether. The wired Pointer inlet overrides them all.");
        let mut b = src_ls;
        if ui.checkbox(&mut b, "LS").changed() { set.push(("ptr_ls", serde_json::json!(b))); }
        let mut b = src_rs;
        if ui.checkbox(&mut b, "RS").changed() { set.push(("ptr_rs", serde_json::json!(b))); }
        let mut b = src_touch;
        if ui.checkbox(&mut b, "Touch").changed() { set.push(("ptr_touch", serde_json::json!(b))); }
        if src_touch {
            egui::ComboBox::from_id_salt((node_id, "menu_touch_which"))
                .selected_text(if touch_which == "touch2" { "Touch 2" } else { "Touch 1" })
                .width(74.0)
                .show_ui(ui, |ui| {
                    for (val, label) in [("touch1", "Touch 1"), ("touch2", "Touch 2")] {
                        if ui.selectable_label(touch_which == val, label).clicked() {
                            set.push(("ptr_touch_which", serde_json::json!(val)));
                        }
                    }
                });
        }
        let mut b = src_gyro;
        if ui.checkbox(&mut b, "Gyro")
            .on_hover_text("Tilt to point: rotation accumulates while the menu is open and\nresets on every open, so the pointer always starts centered.")
            .changed() { set.push(("ptr_gyro", serde_json::json!(b))); }
        if src_gyro {
            egui::ComboBox::from_id_salt((node_id, "menu_gyro_axes"))
                .selected_text(if gyro_axes == "pitch_roll" { "Pitch+Roll" } else { "Pitch+Yaw" })
                .width(92.0)
                .show_ui(ui, |ui| {
                    for (val, label) in [("pitch_yaw", "Pitch+Yaw"), ("pitch_roll", "Pitch+Roll")] {
                        if ui.selectable_label(gyro_axes == val, label).clicked() {
                            set.push(("ptr_gyro_axes", serde_json::json!(val)));
                        }
                    }
                });
            ui.label(egui::RichText::new("×").small().weak());
            let mut sens = gyro_sens;
            if ui.add(egui::DragValue::new(&mut sens).range(0.5..=8.0).speed(0.05))
                .on_hover_text("Gyro pointer sensitivity. 1 ≈ 10× the raw rotation rate;\nraise it if the gyro feels too weak to reach the zones.")
                .changed()
            {
                set.push(("ptr_gyro_sens", serde_json::json!(sens)));
            }
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
            .on_hover_text("How the Show input opens the menu.\nHold: open while held. Toggle: press opens, press closes.\nTouch: open while a finger rests on the touchpad.");
        egui::ComboBox::from_id_salt((node_id, "menu_act"))
            .selected_text(match act.as_str() { "toggle" => "Toggle", "touch" => "Touch", _ => "Hold" })
            .width(70.0)
            .show_ui(ui, |ui| {
                for (val, label) in [("hold", "Hold"), ("toggle", "Toggle"), ("touch", "Touch")] {
                    if ui.selectable_label(act == val, label).clicked() {
                        set.push(("activation_mode", serde_json::json!(val)));
                    }
                }
            });
        ui.label(egui::RichText::new("Select").small().weak())
            .on_hover_text("What commits the highlighted zone.\nRelease: letting go of Show/touch. Press: the Select input.\nClick: the touchpad click.");
        egui::ComboBox::from_id_salt((node_id, "menu_sel_on"))
            .selected_text(match sel_on.as_str() { "press" => "Press", "click" => "Click", _ => "Release" })
            .width(76.0)
            .show_ui(ui, |ui| {
                for (val, label) in [("release", "Release"), ("press", "Press"), ("click", "Click")] {
                    if ui.selectable_label(sel_on == val, label).clicked() {
                        set.push(("select_on", serde_json::json!(val)));
                    }
                }
            });
        ui.label(egui::RichText::new("Deadzone").small().weak());
        let mut dz_val = dz;
        if ui.add(egui::DragValue::new(&mut dz_val).range(0.0..=0.9).speed(0.01))
            .on_hover_text("Pointer deflection below this doesn't move the highlight.")
            .changed()
        {
            set.push(("pointer_deadzone", serde_json::json!(dz_val)));
        }
        ui.label(egui::RichText::new("Linger").small().weak());
        let mut lin = linger;
        if ui.add(egui::DragValue::new(&mut lin).range(0.0..=10.0).speed(0.05).suffix(" s"))
            .on_hover_text("After a selection is accepted the menu hides immediately, but the\nchosen zone cell stays on screen this long, then fades out.\n0 = the whole menu disappears at once.")
            .changed()
        {
            set.push(("select_linger", serde_json::json!(lin)));
        }
        let mut stk = sticky;
        if ui.checkbox(&mut stk, "Sticky")
            .on_hover_text("Keep the last highlighted zone when the pointer returns to the\ndeadzone, so flick-and-release selection works. Off: the highlight\nclears, and releasing inside the deadzone selects nothing.")
            .changed()
        {
            set.push(("hover_sticky", serde_json::json!(stk)));
        }
    });
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
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
