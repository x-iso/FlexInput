//! Virtual Menu module body — name/icon, activation options, and a lean zone
//! editor over the shared BSP tree from `flexinput_core::touchzones`.
//!
//! Deliberately NOT a reuse of Touch Zones' `tz_draw_field`: that painter's
//! middle is coupled to touch fingers, split fields, and the TZ card icons.
//! The menu paints its own zones from the same `ZoneNode` (so dividers,
//! partial dividers, and stable leaf ids behave identically) and will render
//! live hover/selection state from the eval mirror once the eval arm lands.
//!
//! Param schema documented on `VirtualMenuModule` (crates/modules/src/menu.rs).

use std::collections::HashMap;

use egui_snarl::{NodeId, Snarl};
use flexinput_core::menu as fm;
use flexinput_core::touchzones::{Axis, ZoneNode};
use flexinput_core::{PinDescriptor, Signal, SignalType};

use super::NodeData;

type LiveSignals = HashMap<(String, String), Signal>;

// ── Param helpers ─────────────────────────────────────────────────────────────

fn pstr<'a>(node: &'a NodeData, key: &str, default: &'a str) -> &'a str {
    node.params.get(key).and_then(|v| v.as_str()).unwrap_or(default)
}

fn pf32(node: &NodeData, key: &str, default: f32) -> f32 {
    node.params.get(key).and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(default)
}

fn pbool(node: &NodeData, key: &str, default: bool) -> bool {
    node.params.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

fn edges(node: &NodeData, key: &str) -> Vec<f32> {
    node.params.get(key).and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
        .unwrap_or_default()
}

/// The menu's zone tree: explicit `zone_tree` param when present, else derived
/// from the legacy grid edges (matching `tz_field_tree` semantics).
pub(crate) fn menu_tree(node: &NodeData) -> ZoneNode {
    if let Some(v) = node.params.get("zone_tree") {
        if let Some(t) = ZoneNode::from_value(v) {
            return t;
        }
    }
    ZoneNode::from_grid(&edges(node, "col_edges"), &edges(node, "row_edges"))
}

fn set_tree(node: &mut NodeData, tree: &ZoneNode) {
    node.params.insert("zone_tree".into(), tree.to_value());
}

/// Per-zone display label: `zone_meta` entry when set, else `Z{id}`.
pub(crate) fn zone_label(node: &NodeData, id: u32) -> String {
    node.params.get("zone_meta").and_then(|v| v.as_array())
        .and_then(|a| a.iter().find(|m| m.get("id").and_then(|x| x.as_u64()) == Some(id as u64)))
        .and_then(|m| m.get("label").and_then(|x| x.as_str()))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("Z{id}"))
}

fn set_zone_label(node: &mut NodeData, id: u32, label: &str) {
    let mut arr = node.params.get("zone_meta").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if let Some(m) = arr.iter_mut().find(|m| m.get("id").and_then(|x| x.as_u64()) == Some(id as u64)) {
        m["label"] = serde_json::Value::String(label.to_string());
    } else {
        arr.push(serde_json::json!({ "id": id, "label": label }));
    }
    node.params.insert("zone_meta".into(), serde_json::Value::Array(arr));
}

// ── Ports ─────────────────────────────────────────────────────────────────────

/// Derive `output_pin_ids` + dynamic `node.outputs` from the zone tree.
/// Slots 0–2 are the fixed descriptor outputs (AutoMap pass, Open, Hover);
/// ports mode appends per-zone Active/Selected pins. Idempotent.
pub(crate) fn regenerate_menu_ports(node: &mut NodeData) {
    let mut want: Vec<String> = vec![
        fm::PASS_PIN.to_string(), fm::OPEN_PIN.to_string(), fm::HOVER_PIN.to_string(),
    ];
    let ports_mode = pstr(node, "zone_mode", "ports") == "ports";
    let ids = menu_tree(node).ids();
    if ports_mode {
        for id in &ids {
            want.push(fm::zone_pin_id(*id, fm::MenuComp::Active));
            want.push(fm::zone_pin_id(*id, fm::MenuComp::Selected));
        }
    }
    let cur: Vec<String> = node.params.get("output_pin_ids").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    if cur == want { return; }

    node.outputs.truncate(3);
    if ports_mode {
        for id in &ids {
            node.outputs.push(PinDescriptor::new(
                fm::zone_pin_label(*id, fm::MenuComp::Active), SignalType::Bool));
            node.outputs.push(PinDescriptor::new(
                fm::zone_pin_label(*id, fm::MenuComp::Selected), SignalType::Bool));
        }
    }
    node.params.insert(
        "output_pin_ids".into(),
        serde_json::Value::Array(want.into_iter().map(serde_json::Value::String).collect()),
    );
}

// ── Body ──────────────────────────────────────────────────────────────────────

pub(crate) fn show_menu_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    _live_signals: &LiveSignals,
) {
    // Defaults on first render (2×2 grid, ports mode).
    {
        let Some(node) = snarl.get_node_mut(node_id) else { return };
        if !node.params.contains_key("col_edges") {
            node.params.insert("col_edges".into(), serde_json::json!([0.5]));
            node.params.insert("row_edges".into(), serde_json::json!([0.5]));
        }
        regenerate_menu_ports(node);
    }

    ui.set_min_width(250.0);

    // ── Name + icon row (Macro pattern: one name/icon per module) ──
    {
        let (mut name, icon, icon_svg) = {
            let node = snarl.get_node(node_id).unwrap();
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
                .desired_width(120.0));
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

    // ── Activation options ──
    {
        let (act, ptr, sel_on, dz, suppress) = {
            let node = snarl.get_node(node_id).unwrap();
            (
                pstr(node, "activation_mode", "hold").to_string(),
                pstr(node, "pointer_source", "left_stick").to_string(),
                pstr(node, "select_on", "release").to_string(),
                pf32(node, "pointer_deadzone", 0.25),
                pbool(node, "suppress_while_open", true),
            )
        };
        let mut set: Vec<(&str, serde_json::Value)> = Vec::new();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Show").small().weak())
                .on_hover_text("How the wired Show input opens the menu.\nHold: open while held. Toggle: press opens, press closes.\nTouch: open while a finger rests on the touchpad.");
            egui::ComboBox::from_id_salt((node_id, "menu_act"))
                .selected_text(match act.as_str() {
                    "toggle" => "Toggle", "touch" => "Touch", _ => "Hold" })
                .width(70.0)
                .show_ui(ui, |ui| {
                    for (val, label) in [("hold", "Hold"), ("toggle", "Toggle"), ("touch", "Touch")] {
                        if ui.selectable_label(act == val, label).clicked() {
                            set.push(("activation_mode", serde_json::json!(val)));
                        }
                    }
                });
            ui.label(egui::RichText::new("Point").small().weak())
                .on_hover_text("What highlights zones while the menu is open.");
            egui::ComboBox::from_id_salt((node_id, "menu_ptr"))
                .selected_text(match ptr.as_str() {
                    "right_stick" => "R.Stick", "touch1" => "Touch", _ => "L.Stick" })
                .width(76.0)
                .show_ui(ui, |ui| {
                    for (val, label) in [
                        ("left_stick", "L.Stick"), ("right_stick", "R.Stick"), ("touch1", "Touch"),
                    ] {
                        if ui.selectable_label(ptr == val, label).clicked() {
                            set.push(("pointer_source", serde_json::json!(val)));
                        }
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Select").small().weak())
                .on_hover_text("What commits the highlighted zone.\nRelease: letting go of Show/touch. Press: the wired Select input.\nClick: the touchpad click.");
            egui::ComboBox::from_id_salt((node_id, "menu_sel_on"))
                .selected_text(match sel_on.as_str() {
                    "press" => "Press", "click" => "Click", _ => "Release" })
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
                .on_hover_text("Stick deflection below this doesn't move the highlight.")
                .changed()
            {
                set.push(("pointer_deadzone", serde_json::json!(dz_val)));
            }
        });
        let mut sup = suppress;
        if ui.checkbox(&mut sup, "Suppress pointer input while open")
            .on_hover_text("While the menu is open, the pointing stick / touch no longer\nreaches whatever is wired after the AutoMap passthrough.")
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

    // ── Mode toggle (Ports ⇄ Mapping); the future radial toggle sits here ──
    {
        let mode = {
            let node = snarl.get_node(node_id).unwrap();
            pstr(node, "zone_mode", "ports").to_string()
        };
        let mut new_mode: Option<&str> = None;
        ui.horizontal(|ui| {
            if ui.selectable_label(mode == "ports", "Ports")
                .on_hover_text("Expose per-zone Active/Selected pins for wiring.")
                .clicked() { new_mode = Some("ports"); }
            if ui.selectable_label(mode == "mapping", "Mapping")
                .on_hover_text("Map zones to outputs with internal cards — no wiring needed.")
                .clicked() { new_mode = Some("mapping"); }
        });
        if let Some(m) = new_mode {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("zone_mode".into(), serde_json::json!(m));
                regenerate_menu_ports(node);
            }
        }
    }

    // ── Zone editor ──
    show_menu_zone_editor(node_id, ui, snarl);

    // ── Selected-zone label edit ──
    {
        let sel = snarl.get_node(node_id)
            .and_then(|n| n.params.get("sel_zone").and_then(|v| v.as_u64()))
            .map(|v| v as u32);
        if let Some(sel) = sel {
            let cur = snarl.get_node(node_id).map(|n| zone_label(n, sel)).unwrap_or_default();
            let mut label = cur.clone();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("Zone {sel}")).small().weak());
                if ui.add(egui::TextEdit::singleline(&mut label)
                    .hint_text("Label").desired_width(110.0)).changed()
                {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        set_zone_label(node, sel, &label);
                    }
                }
            });
        }
    }

    // Placeholder until Phase 5 (menu overlay + edit mode) lands.
    ui.add_enabled(false, egui::Button::new("🖵 Position on screen…"))
        .on_hover_text("Place/size the menu on the screen overlay (coming up)");
}

// ── Zone editor (lean, over the shared BSP tree) ──────────────────────────────

const MIN_ZONE_FRAC: f32 = 0.06;

fn show_menu_zone_editor(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (tree, sel_zone, w, h) = {
        let Some(node) = snarl.get_node(node_id) else { return };
        (
            menu_tree(node),
            node.params.get("sel_zone").and_then(|v| v.as_u64()).map(|v| v as u32),
            pf32(node, "menu_field_w", 230.0),
            pf32(node, "menu_field_h", 140.0),
        )
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let vis = ui.visuals();

    painter.rect_filled(rect, 4.0, vis.extreme_bg_color);

    let to_screen = |r: [f32; 4]| -> egui::Rect {
        egui::Rect::from_min_max(
            egui::pos2(rect.left() + r[0] * rect.width(), rect.top() + r[1] * rect.height()),
            egui::pos2(rect.left() + r[2] * rect.width(), rect.top() + r[3] * rect.height()),
        )
    };

    // Zones: fill + label + click-select + hover split buttons.
    let mut new_tree: Option<ZoneNode> = None;
    let mut new_sel: Option<u32> = None;
    let zones = tree.zones();
    for (id, zr) in &zones {
        let zrect = to_screen(*zr).shrink(1.0);
        let is_sel = sel_zone == Some(*id);
        let fill = if is_sel {
            vis.selection.bg_fill.gamma_multiply(0.35)
        } else {
            egui::Color32::from_gray(46)
        };
        painter.rect_filled(zrect, 3.0, fill);
        let label = snarl.get_node(node_id).map(|n| zone_label(n, *id)).unwrap_or_default();
        painter.text(
            zrect.center(), egui::Align2::CENTER_CENTER, label,
            egui::FontId::proportional(11.0),
            if is_sel { vis.strong_text_color() } else { egui::Color32::from_gray(150) },
        );
        if is_sel {
            painter.rect_stroke(zrect, 3.0,
                egui::Stroke::new(1.5, vis.selection.stroke.color), egui::StrokeKind::Inside);
        }

        let resp = ui.interact(zrect, egui::Id::new(("menu_zone", node_id.0, *id)), egui::Sense::click());
        if resp.clicked() { new_sel = Some(*id); }

        // Hover: split buttons (V | H) bottom-center of the zone.
        if resp.hovered() && zrect.width() > 44.0 && zrect.height() > 30.0 {
            let base = egui::pos2(zrect.center().x, zrect.bottom() - 10.0);
            for (i, (glyph, axis, hint)) in [
                ("┃", Axis::V, "Split vertically"),
                ("━", Axis::H, "Split horizontally"),
            ].iter().enumerate() {
                let c = egui::pos2(base.x - 12.0 + 24.0 * i as f32, base.y);
                let brect = egui::Rect::from_center_size(c, egui::vec2(18.0, 16.0));
                let bresp = ui.interact(brect,
                    egui::Id::new(("menu_zone_split", node_id.0, *id, i)), egui::Sense::click());
                let bg = if bresp.hovered() { egui::Color32::from_gray(90) } else { egui::Color32::from_gray(64) };
                painter.rect_filled(brect, 3.0, bg);
                painter.text(c, egui::Align2::CENTER_CENTER, *glyph,
                    egui::FontId::proportional(10.0), egui::Color32::from_gray(210));
                if bresp.on_hover_text(*hint).clicked() {
                    let mut t = tree.clone();
                    let mid = match axis {
                        Axis::V => (zr[0] + zr[2]) * 0.5,
                        Axis::H => (zr[1] + zr[3]) * 0.5,
                    };
                    if t.subdivide(*id, *axis, mid).is_some() {
                        new_tree = Some(t);
                    }
                }
            }
        }
    }

    // Dividers: draggable lines + right-click / hover ✕ to merge.
    for (di, d) in tree.dividers().iter().enumerate() {
        let (p0, p1, drag_rect) = match d.axis {
            Axis::V => {
                let x = rect.left() + d.pos * rect.width();
                let y0 = rect.top() + d.span_lo * rect.height();
                let y1 = rect.top() + d.span_hi * rect.height();
                (egui::pos2(x, y0), egui::pos2(x, y1),
                 egui::Rect::from_min_max(egui::pos2(x - 4.0, y0), egui::pos2(x + 4.0, y1)))
            }
            Axis::H => {
                let y = rect.top() + d.pos * rect.height();
                let x0 = rect.left() + d.span_lo * rect.width();
                let x1 = rect.left() + d.span_hi * rect.width();
                (egui::pos2(x0, y), egui::pos2(x1, y),
                 egui::Rect::from_min_max(egui::pos2(x0, y - 4.0), egui::pos2(x1, y + 4.0)))
            }
        };
        let resp = ui.interact(drag_rect,
            egui::Id::new(("menu_divider", node_id.0, di)), egui::Sense::click_and_drag());
        let hot = resp.hovered() || resp.dragged();
        painter.line_segment([p0, p1], egui::Stroke::new(
            if hot { 2.5 } else { 1.5 },
            if hot { egui::Color32::from_gray(200) } else { egui::Color32::from_gray(110) },
        ));
        if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let t = match d.axis {
                    Axis::V => (pos.x - rect.left()) / rect.width(),
                    Axis::H => (pos.y - rect.top()) / rect.height(),
                };
                let t = t.clamp(d.lo + MIN_ZONE_FRAC, d.hi - MIN_ZONE_FRAC);
                let mut nt = new_tree.take().unwrap_or_else(|| tree.clone());
                if nt.set_divider_t(&d.path, t) {
                    new_tree = Some(nt);
                }
            }
            ui.ctx().set_cursor_icon(match d.axis {
                Axis::V => egui::CursorIcon::ResizeHorizontal,
                Axis::H => egui::CursorIcon::ResizeVertical,
            });
        }
        // Remove: ✕ chip at the divider midpoint while hovered.
        if resp.hovered() && !resp.dragged() {
            let mid = egui::pos2((p0.x + p1.x) * 0.5, (p0.y + p1.y) * 0.5);
            let xrect = egui::Rect::from_center_size(mid, egui::vec2(14.0, 14.0));
            let xresp = ui.interact(xrect,
                egui::Id::new(("menu_divider_x", node_id.0, di)), egui::Sense::click());
            painter.circle_filled(mid, 7.0,
                if xresp.hovered() { egui::Color32::from_rgb(160, 60, 60) } else { egui::Color32::from_gray(70) });
            painter.text(mid, egui::Align2::CENTER_CENTER, "✕",
                egui::FontId::proportional(9.0), egui::Color32::from_gray(220));
            if xresp.on_hover_text("Remove divider (merges its zones)").clicked() {
                let mut nt = tree.clone();
                if nt.remove_split(&d.path, sel_zone).is_some() {
                    new_tree = Some(nt);
                }
            }
        }
    }

    painter.rect_stroke(rect, 4.0,
        egui::Stroke::new(1.0, vis.widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Inside);

    // Commit edits.
    if new_tree.is_some() || new_sel.is_some() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(t) = new_tree {
                set_tree(node, &t);
                // Keep the selection on a surviving zone.
                let ids = menu_tree(node).ids();
                let keep = node.params.get("sel_zone").and_then(|v| v.as_u64()).map(|v| v as u32);
                if keep.map(|k| !ids.contains(&k)).unwrap_or(false) {
                    node.params.remove("sel_zone");
                }
                regenerate_menu_ports(node);
            }
            if let Some(sel) = new_sel {
                node.params.insert("sel_zone".into(), serde_json::json!(sel));
            }
        }
    }
}
