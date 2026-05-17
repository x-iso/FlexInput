use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText};
use flexinput_virtual::{
    available_device_kinds, create_device,
    driver_availability::vigem_available,
    VirtualDevice,
};

use crate::canvas::Canvas;
use crate::canvas::remapper_icons;
use crate::panels::device_icon::{render_device_icon, svg_icon_button};
use crate::panels::physical_devices::canvas_status_button;

const CHIP_ICON_H: f32 = 24.0;
const CHIP_H: f32 = 28.0;

fn kind_prefix_of(dev_id: &str) -> String {
    dev_id.split('.').take(2).collect::<Vec<_>>().join(".")
}

pub struct VirtualDevicePanel {
    /// Devices for this tab. Shared with the I/O thread when this tab is active.
    pub active: Arc<Mutex<Vec<Box<dyn VirtualDevice>>>>,
}

impl VirtualDevicePanel {
    pub fn new() -> Self {
        Self { active: Arc::new(Mutex::new(vec![])) }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, canvas: &mut Canvas) {
        // Snapshot device state briefly so we can render without holding the lock.
        let chips: Vec<(String, String, bool)> = {
            let devs = self.active.lock().unwrap();
            devs.iter().enumerate().map(|(i, d)| {
                (d.id().to_string(), chip_name(&devs, i), d.is_connected())
            }).collect()
        };

        // Driver dependency banner
        if !vigem_available() {
            ui.horizontal(|ui| {
                let warn = RichText::new("⚠ ViGEmBus missing").color(Color32::from_rgb(220, 160, 40));
                ui.label(warn).on_hover_text("Required for Virtual XInput and Virtual DualShock 4");
                if ui.small_button("Install").clicked() {
                    let _ = open::that("https://github.com/nefarius/ViGEmBus/releases/latest");
                }
            });
        }

        let scroll_out = egui::ScrollArea::horizontal()
            .id_salt("virtual_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
        ui.horizontal_top(|ui| {
            let mut to_remove: Option<usize> = None;
            for (i, (dev_id, chip_label, connected)) in chips.iter().enumerate() {
                let chip = egui::Frame::default()
                    .inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 0 })
                    .corner_radius(10.0)
                    .fill(ui.visuals().widgets.inactive.bg_fill)
                    .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color));

                chip.show(ui, |ui| {
                    ui.set_height(CHIP_H);
                    ui.with_layout(
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            let kind_prefix = kind_prefix_of(dev_id);
                            if kind_prefix == "virtual.keymouse" {
                                let (kb, ms) = remapper_icons::keymouse_pair_svgs();
                                render_device_icon(ui, kb, CHIP_ICON_H);
                                render_device_icon(ui, ms, CHIP_ICON_H);
                            } else {
                                render_device_icon(
                                    ui,
                                    remapper_icons::virtual_device_card_svg(&kind_prefix),
                                    CHIP_ICON_H,
                                );
                            }
                            let (dot, hover) = if *connected {
                                (RichText::new("●").small().color(Color32::from_rgb(80, 200, 100)),
                                 "Connected")
                            } else {
                                (RichText::new("●").small().color(Color32::from_rgb(220, 80, 60)),
                                 "Not connected — driver unavailable (ViGEmBus / enigo)")
                            };
                            ui.label(dot).on_hover_text(hover);
                            ui.label(RichText::new(chip_label.as_str()).strong());

                            let on_canvas = canvas.snarl.nodes_ids_data().any(|(_, n)| {
                                n.value.module_id == "device.sink"
                                    && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(dev_id.as_str())
                            });
                            canvas_status_button(ui, on_canvas, || {
                                // Re-lock briefly to get the device reference for canvas registration.
                                let devs = self.active.lock().unwrap();
                                if let Some(dev) = devs.get(i) {
                                    canvas.add_virtual_sink(dev.as_ref());
                                    let new_name = chip_name(&devs, i);
                                    let did = dev.id().to_string();
                                    drop(devs);
                                    if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                                        n.value.module_id == "device.sink"
                                            && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&did)
                                    }) {
                                        if let Some(node) = canvas.snarl.get_node_mut(nid) {
                                            node.display_name = new_name;
                                        }
                                    }
                                }
                            });

                            // Close button — placed last so it's far-right.
                            if svg_icon_button(ui, remapper_icons::CLOSE_SVG, 18.0)
                                .on_hover_text("Remove")
                                .clicked()
                            {
                                to_remove = Some(i);
                            }
                        },
                    );
                });
            }

            if let Some(i) = to_remove {
                let (removed_id, kind_prefix) = {
                    let mut devs = self.active.lock().unwrap();
                    let removed = devs.remove(i);
                    let id = removed.id().to_string();
                    let prefix = id.split('.').take(2).collect::<Vec<_>>().join(".");
                    (id, prefix)
                };

                // Remove the canvas sink node for the removed device.
                if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                    n.value.module_id == "device.sink"
                        && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&removed_id)
                }) {
                    canvas.snarl.remove_node(nid);
                }

                // Re-sync canvas node display names for remaining same-kind devices.
                let renames: Vec<(String, String)> = {
                    let devs = self.active.lock().unwrap();
                    devs.iter().enumerate()
                        .filter(|(_, d)| d.id().starts_with(&kind_prefix))
                        .map(|(j, d)| (d.id().to_string(), chip_name(&devs, j)))
                        .collect()
                };
                for (did, new_name) in renames {
                    if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                        n.value.module_id == "device.sink"
                            && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&did)
                    }) {
                        if let Some(node) = canvas.snarl.get_node_mut(nid) {
                            node.display_name = new_name;
                        }
                    }
                }
            }

            // Inline "+" SVG button — sits immediately after the last chip.
            add_svg_button(ui, |ui, ctl| {
                ui.label(RichText::new("Add virtual output").strong());
                ui.separator();

                for kind in available_device_kinds() {
                    let already = if kind.allows_multiple {
                        false
                    } else {
                        let devs = self.active.lock().unwrap();
                        devs.iter().any(|a| a.id().starts_with(kind.kind_id))
                    };

                    if already {
                        ui.add_enabled(false, egui::Button::new(kind.display_name));
                    } else if ui.button(kind.display_name).clicked() {
                        let instance = {
                            let devs = self.active.lock().unwrap();
                            devs.iter().filter(|d| d.id().starts_with(kind.kind_id)).count()
                        };

                        let dev = create_device(kind.kind_id, instance);
                        canvas.add_virtual_sink(dev.as_ref());
                        let mut devs = self.active.lock().unwrap();
                        devs.push(dev);
                        let j = devs.len() - 1;
                        let new_name = chip_name(&devs, j);
                        let dev_id = devs[j].id().to_string();
                        drop(devs);
                        if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                            n.value.module_id == "device.sink"
                                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&dev_id)
                        }) {
                            if let Some(node) = canvas.snarl.get_node_mut(nid) {
                                node.display_name = new_name;
                            }
                        }
                        ctl.close = true;
                    }
                }
            });

            // Trailing space so the last chip + button never sit under the
            // right-edge fade shadow.
            ui.add_space(40.0);
        });
            });
        crate::panels::physical_devices::paint_scroll_edge_fades(ui.ctx(), &scroll_out, ui.clip_rect());
    }
}

/// Chip-styled menu button for adding a new virtual device. Matches the
/// device-chip frame visually so it reads as part of the row.
pub(crate) struct MenuCtl {
    pub close: bool,
}

/// Inline "+" SVG icon-button that opens the "add virtual device" menu.
/// The body receives a `&mut MenuCtl`; set `ctl.close = true` to dismiss
/// the menu (used after a device is selected).
fn add_svg_button(ui: &mut egui::Ui, menu_body: impl FnOnce(&mut egui::Ui, &mut MenuCtl)) {
    let menu_id = egui::Id::new("add_virtual_menu_open");
    // Wrap in a vertically-centered sub-ui so the button aligns with the
    // chip row instead of sitting top-aligned against the panel's top edge.
    let resp = ui
        .with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_height(CHIP_H);
            svg_icon_button(ui, remapper_icons::ADD_SVG, CHIP_H)
        })
        .inner;
    let resp = resp.on_hover_text("Add virtual output");
    if resp.clicked() {
        let open = ui.memory(|m| m.data.get_temp::<bool>(menu_id).unwrap_or(false));
        ui.memory_mut(|m| m.data.insert_temp(menu_id, !open));
    }
    let open = ui.memory(|m| m.data.get_temp::<bool>(menu_id).unwrap_or(false));
    if open {
        let mut ctl = MenuCtl { close: false };
        let area_resp = egui::Area::new(egui::Id::new("add_virtual_menu_area"))
            .order(egui::Order::Foreground)
            .fixed_pos(resp.rect.left_bottom() + egui::vec2(0.0, 4.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(160.0);
                    menu_body(ui, &mut ctl);
                });
            });
        if ctl.close {
            ui.memory_mut(|m| m.data.insert_temp(menu_id, false));
            return;
        }
        // Close on outside click.
        if ui.input(|i| i.pointer.any_click())
            && !area_resp.response.rect.contains(
                ui.input(|i| i.pointer.interact_pos()).unwrap_or(egui::Pos2::ZERO),
            )
            && !resp.rect.contains(
                ui.input(|i| i.pointer.interact_pos()).unwrap_or(egui::Pos2::ZERO),
            )
        {
            ui.memory_mut(|m| m.data.insert_temp(menu_id, false));
        }
    }
}

#[allow(dead_code)]
fn circular_ghost_button(ui: &mut egui::Ui, glyph: &str, size: f32) -> egui::Response {
    let btn = egui::Button::new(RichText::new(glyph).size(size * 0.65))
        .corner_radius(size * 0.5)
        .min_size(egui::vec2(size, size))
        .fill(Color32::TRANSPARENT)
        .stroke(egui::Stroke::NONE);
    ui.add(btn)
}

fn chip_name(active: &[Box<dyn VirtualDevice>], i: usize) -> String {
    let dev = &active[i];
    let kind_prefix = dev.id().split('.').take(2).collect::<Vec<_>>().join(".");
    let total = active.iter().filter(|d| d.id().starts_with(&kind_prefix)).count();
    let rank  = active[..i].iter().filter(|d| d.id().starts_with(&kind_prefix)).count();
    let base = kind_base_name(&kind_prefix);
    if total <= 1 { base.to_string() } else { format!("{} #{}", base, rank + 1) }
}

fn kind_base_name(kind_prefix: &str) -> &'static str {
    match kind_prefix {
        "virtual.xinput"    => "Virtual XInput",
        "virtual.ds4"       => "Virtual DualShock 4",
        "virtual.keymouse"  => "Virtual Keyboard & Mouse",
        _                   => "Virtual Device",
    }
}

impl Default for VirtualDevicePanel {
    fn default() -> Self { Self::new() }
}
