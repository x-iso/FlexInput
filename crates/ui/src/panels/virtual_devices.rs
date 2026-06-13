use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText};
use flexinput_virtual::{
    available_device_kinds,
    driver_availability::vigem_available,
    VirtualDevice,
};

use crate::canvas::{Canvas, DeviceParamDefaults};
use crate::canvas::remapper_icons;
use crate::panels::device_icon::{render_device_icon, svg_icon_button};
use crate::panels::physical_devices::canvas_status_button;

const CHIP_ICON_H: f32 = 24.0;
const CHIP_H: f32 = 28.0;

/// Shared, app-level pool of virtual output devices. Tabs no longer own
/// devices — the pool is shared across all open tabs so the same
/// `virtual.xinput.0` instance is reused if multiple patches reference it.
pub type SharedDevicePool = Arc<Mutex<Vec<Box<dyn VirtualDevice>>>>;

fn kind_prefix_of(dev_id: &str) -> String {
    // HIDMaestro kinds live in a 3-segment namespace (`virtual.hm.ds4`); all
    // other kinds are 2-segment (`virtual.xinput`).
    let segs = if dev_id.starts_with("virtual.hm.") { 3 } else { 2 };
    dev_id.split('.').take(segs).collect::<Vec<_>>().join(".")
}

/// Lowest instance index for `kind_id` not already taken by a device in the pool
/// or a sink node on `canvas`. The device id is `kind_id` for instance 0 and
/// `kind_id.N` for N>0; this parses that suffix back out. Considering the canvas
/// (not just the pool) matters because a freshly-added device may still be a
/// pending async-create that hasn't landed in the pool yet.
fn next_free_instance(pool: &SharedDevicePool, canvas: &Canvas, kind_id: &str) -> usize {
    let mut used: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut note = |id: &str| {
        if id == kind_id {
            used.insert(0);
        } else if let Some(rest) = id.strip_prefix(kind_id) {
            // rest looks like ".N"; only count an exact kind match (avoid
            // "virtual.ds4" matching "virtual.ds4x" — kinds never collide that
            // way today, but be precise).
            if let Some(n) = rest.strip_prefix('.').and_then(|s| s.parse::<usize>().ok()) {
                used.insert(n);
            }
        }
    };
    {
        let devs = pool.lock().unwrap();
        for d in devs.iter() {
            note(d.id());
        }
    }
    for (_, n) in canvas.snarl.nodes_ids_data() {
        if n.value.module_id == "device.sink" {
            if let Some(id) = n.value.params.get("device_id").and_then(|v| v.as_str()) {
                note(id);
            }
        }
    }
    (0..).find(|i| !used.contains(i)).unwrap_or(0)
}

/// Stateless renderer for the top virtual-devices panel. The actual device
/// list lives in `FlexInputApp::shared_virtual_devices`; the panel reads it
/// every frame and shows the entire pool (the user's spec: top panel
/// reflects the whole shared pool, not just devices referenced by the
/// active tab).
pub struct VirtualDevicePanel;

impl VirtualDevicePanel {
    pub fn new() -> Self { Self }

    /// Render the panel.
    ///
    /// * `pool` — shared device pool (app-level Arc).
    /// * `canvas` — current tab's canvas; "+" adds a sink node here, and
    ///   the close (X) button uses canvas membership to decide whether the
    ///   device can be safely removed (only when no other tab references it).
    /// * `referenced_elsewhere` — for each device id, whether *some other
    ///   open tab's canvas* also references it. When true the close (X)
    ///   button on the chip is disabled with an explanatory tooltip.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        pool: &SharedDevicePool,
        canvas: &mut Canvas,
        default_collapsed: bool,
        defaults: DeviceParamDefaults,
        referenced_elsewhere: &dyn Fn(&str) -> bool,
    ) {
        // Snapshot device state briefly so we can render without holding the lock.
        let chips: Vec<(String, String, bool)> = {
            let devs = pool.lock().unwrap();
            devs.iter().enumerate().map(|(i, d)| {
                (d.id().to_string(), chip_name(&devs, i), d.is_connected())
            }).collect()
        };

        let scroll_out = egui::ScrollArea::horizontal()
            .id_salt("virtual_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
        ui.horizontal_top(|ui| {
            let mut to_remove: Option<String> = None;
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
                            {
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
                                let devs = pool.lock().unwrap();
                                if let Some(dev) = devs.get(i) {
                                    canvas.add_virtual_sink(dev.as_ref(), default_collapsed, defaults);
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
                            // Disabled when another open tab still references this
                            // device; the user must close that tab (or delete the
                            // sink node there) first.
                            let other_tab_uses_it = referenced_elsewhere(dev_id);
                            let resp = ui
                                .add_enabled_ui(!other_tab_uses_it, |ui| {
                                    svg_icon_button(ui, remapper_icons::CLOSE_SVG, 18.0)
                                })
                                .inner;
                            let resp = if other_tab_uses_it {
                                resp.on_hover_text(
                                    "Used by another open tab — close that tab first to remove this device",
                                )
                            } else {
                                resp.on_hover_text("Remove")
                            };
                            if !other_tab_uses_it && resp.clicked() {
                                to_remove = Some(dev_id.clone());
                            }
                        },
                    );
                });
            }

            if let Some(removed_id) = to_remove {
                // Only remove the canvas sink node here. The actual pool removal +
                // device teardown is done off the UI thread by the app's per-frame
                // `prune_devices_async` (a HIDMaestro device's Drop blocks on the
                // helper) — keyed on canvas membership, so dropping the node is the
                // trigger. This keeps remove from freezing the UI.
                if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                    n.value.module_id == "device.sink"
                        && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&removed_id)
                }) {
                    canvas.snarl.remove_node(nid);
                }
            }

            // Inline "+" SVG button — sits immediately after the last chip.
            add_svg_button(ui, |ui, ctl| {
                ui.label(RichText::new("Add virtual output").strong());
                ui.separator();

                let vigem_ok = vigem_available();
                for kind in available_device_kinds() {
                    let already = if kind.allows_multiple {
                        false
                    } else {
                        let devs = pool.lock().unwrap();
                        devs.iter().any(|a| a.id().starts_with(kind.kind_id))
                    };

                    // ViGEmBus-backed kinds (XInput / DS4) can't be created
                    // until the driver is installed; show an inline warning +
                    // install link next to the relevant option.
                    let needs_vigem = matches!(kind.kind_id, "virtual.xinput" | "virtual.ds4");
                    let blocked_by_driver = needs_vigem && !vigem_ok;

                    ui.horizontal(|ui| {
                    if already {
                        ui.add_enabled(false, egui::Button::new(kind.display_name));
                    } else if blocked_by_driver {
                        ui.add_enabled(false, egui::Button::new(kind.display_name));
                        let warn = ui
                            .add(egui::Label::new(
                                RichText::new("⚠ missing driver")
                                    .small()
                                    .color(Color32::from_rgb(220, 160, 40)),
                            ).sense(egui::Sense::click()))
                            .on_hover_text(
                                "Requires ViGEmBus — click to install",
                            );
                        if warn.clicked() {
                            let _ = open::that(
                                "https://github.com/nefarius/ViGEmBus/releases/latest",
                            );
                        }
                    } else if ui.button(kind.display_name).clicked() {
                        // Allocate the next free instance index for this kind by
                        // scanning ids already present in the pool OR referenced on
                        // any open canvas (a pending async-create may not be in the
                        // pool yet). The device id encodes the instance.
                        let instance = next_free_instance(pool, canvas, kind.kind_id);
                        let device_id = if instance == 0 {
                            kind.kind_id.to_string()
                        } else {
                            format!("{}.{}", kind.kind_id, instance)
                        };
                        // Add the canvas node NOW from static pin metadata — no
                        // blocking device build. The per-frame reconcile sees the
                        // new sink node and enqueues the real (async) create; the
                        // device-ops worker installs it into the pool when ready.
                        if let Some((sink, source, name)) =
                            flexinput_virtual::kind_pin_metadata(kind.kind_id, instance)
                        {
                            canvas.add_virtual_sink_static(
                                &device_id, &name, sink, source, default_collapsed, defaults,
                            );
                        }
                        ctl.close = true;
                    }
                    });
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
                    ui.set_min_width(260.0);
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
    let kind_prefix = kind_prefix_of(dev.id());
    let total = active.iter().filter(|d| d.id().starts_with(&kind_prefix)).count();
    let rank  = active[..i].iter().filter(|d| d.id().starts_with(&kind_prefix)).count();
    let base = kind_base_name(&kind_prefix);
    if total <= 1 { base.to_string() } else { format!("{} #{}", base, rank + 1) }
}

fn kind_base_name(kind_prefix: &str) -> &'static str {
    match kind_prefix {
        "virtual.xinput"        => "Virtual XInput",
        "virtual.ds4"           => "Virtual DualShock 4",
        "virtual.keymouse"      => "Virtual Keyboard & Mouse",
        "virtual.hm.ds4"        => "Virtual DualShock 4",
        "virtual.hm.dualsense"  => "Virtual DualSense",
        _                       => "Virtual Device",
    }
}

impl Default for VirtualDevicePanel {
    fn default() -> Self { Self::new() }
}
