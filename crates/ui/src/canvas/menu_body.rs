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
//! the menu is pointed at by whatever analog input the user maps to it as a
//! macro-style named target (plus wired Show/Select pins as alternates), and
//! it renders on its own screen overlay while open. The zone-live highlight
//! here comes from the node's own eval mirror (Open/Hover outputs), not from
//! touch pins.
//!
//! Menu-specific params (on top of the TZ set):
//!   menu_name, menu_icon, menu_icon_svg — identity (Macro pattern)
//!   activation_mode: "hold" | "toggle" | "touch"
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
fn menu_zone_live(node: &NodeData) -> HashMap<(usize, usize), (f32, f32, bool)> {
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
    let accent = egui::Color32::from_rgb(255, 196, 90); // menu accent: overlay amber
    let mapping = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");

    let zone_live = snarl.get_node(node_id).map(menu_zone_live).unwrap_or_default();

    let field_w = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_w").and_then(|v| v.as_f64())).unwrap_or(300.0) as f32;
    let field_h = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_h").and_then(|v| v.as_f64())).unwrap_or(180.0) as f32;

    ui.vertical(|ui| {
        // ── Identity row: icon + name (the menu's face in mapping pickers) ──
        show_menu_identity_row(node_id, ui, snarl);

        // ── Mode + menu options row (where TZ has its split-pads checkbox;
        //    the RADIAL toggle takes this slot later) ──
        ui.horizontal(|ui| {
            let mut want_mapping = mapping;
            ui.label("Mode:");
            if ui.selectable_label(!want_mapping, "Ports")
                .on_hover_text("Expose typed X / Y / Active outputs per zone + Select.").clicked() { want_mapping = false; }
            if ui.selectable_label(want_mapping, "Mapping")
                .on_hover_text("Map each zone to gamepad/key/stick outputs with internal cards.").clicked() { want_mapping = true; }
            if want_mapping != mapping {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("zone_mode".to_string(),
                        serde_json::json!(if want_mapping { "mapping" } else { "ports" }));
                }
                regenerate_menu_ports(node_id, snarl);
            }
            ui.separator();
            show_menu_options_row(node_id, ui, snarl);
        });

        // Re-read after the toggle so this frame renders consistently.
        let mapping = snarl.get_node(node_id)
            .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");

        // ── The zone field (single, TZ machinery) ──
        let field_area = render_touch_field(
            node_id, 0, true, true, mapping, ui, snarl,
            &zone_live, &visuals, accent, field_w, field_h,
        );
        register_exposable_element(ui, node_id, "field", field_area);

        if mapping { tz_render_merge_popup(ui, snarl, node_id); }

        // ── Mapping mode: zone-tab card list (TZ machinery, separately pinnable) ──
        if mapping {
            ui.add_space(6.0);
            let cards_area = ui.vertical(|ui| {
                render_touch_zone_cards(node_id, ui, snarl, &visuals, accent, live_signals, automap_parent);
            }).response.rect;
            register_exposable_element(ui, node_id, "cards", cards_area);
        }

        // Placeholder until the menu overlay + edit mode lands.
        ui.add_enabled(false, egui::Button::new("🖵 Position on screen…"))
            .on_hover_text("Place/size the menu on the screen overlay (coming up)");
    });
}

/// Icon + name row (Macro pattern: one identity per module — this is how the
/// menu appears as a named target in the mapping pickers).
fn show_menu_identity_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (mut name, icon, icon_svg) = {
        let Some(node) = snarl.get_node(node_id) else { return };
        (
            pstr(node, "menu_name", "").to_string(),
            pstr(node, "menu_icon", "").to_string(),
            pstr(node, "menu_icon_svg", "").to_string(),
        )
    };
    let mut new_icon: Option<(String, String)> = None; // (key, custom svg)
    let mut name_changed = false;
    ui.horizontal(|ui| {
        let icon_btn = if let Some(tex) = crate::macro_icons::macro_port_icon_texture(
            ui.ctx(), &icon, &icon_svg, 18.0)
        {
            egui::Button::image(egui::Image::new(&tex)
                .fit_to_exact_size(egui::vec2(18.0, 18.0))
                .tint(egui::Color32::WHITE))
        } else {
            egui::Button::new(egui::RichText::new("◌").size(14.0))
        };
        egui::containers::menu::MenuButton::from_button(icon_btn).ui(ui, |ui: &mut egui::Ui| {
            ui.horizontal(|ui| {
                if ui.button("Load custom SVG…")
                    .on_hover_text("Pick an .svg file — it's embedded into the patch")
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("SVG", &["svg"]).pick_file()
                    {
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
            ui.separator();
            const COLS: usize = 10;
            const CELL: f32 = 26.0;
            const GAP: f32 = 4.0;
            ui.set_min_width(COLS as f32 * (CELL + GAP));
            egui::ScrollArea::vertical().max_height(10.0 * (CELL + GAP)).show(ui, |ui| {
                egui::Grid::new((node_id, "menu_icon_grid")).spacing([GAP, GAP]).show(ui, |ui| {
                    for (idx, (key, label, _)) in crate::macro_icons::ALL_ICONS.iter().enumerate() {
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
                        if ui.add(btn).on_hover_text(*label).clicked() {
                            new_icon = Some((key.to_string(), String::new()));
                            ui.close();
                        }
                        if (idx + 1) % COLS == 0 { ui.end_row(); }
                    }
                });
            });
        });
        let resp = ui.add(egui::TextEdit::singleline(&mut name)
            .hint_text("Menu name")
            .desired_width(140.0));
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

/// Activation options: how the menu opens, commits, and behaves while open.
fn show_menu_options_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (act, sel_on, dz, suppress) = {
        let Some(node) = snarl.get_node(node_id) else { return };
        (
            pstr(node, "activation_mode", "hold").to_string(),
            pstr(node, "select_on", "release").to_string(),
            pf32(node, "pointer_deadzone", 0.25),
            pbool(node, "suppress_while_open", true),
        )
    };
    let mut set: Vec<(&str, serde_json::Value)> = Vec::new();
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
    let mut sup = suppress;
    if ui.checkbox(&mut sup, "Suppress")
        .on_hover_text("While the menu is open, the pointing input no longer reaches\nwhatever is wired after the AutoMap passthrough.")
        .changed()
    {
        set.push(("suppress_while_open", serde_json::json!(sup)));
    }
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
}
