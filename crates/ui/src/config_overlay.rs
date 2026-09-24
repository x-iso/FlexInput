//! Config overlay (M3) — a shortcut-summoned, transparent, always-on-top layer
//! for tweaking module parameters LIVE while a game runs. Unlike the info
//! overlay (display-only, click-through), the config overlay pins the module's
//! INTERACTIVE elements (sliders, response curves, toggles, dropdowns, numeric
//! rows) and is interactive over them (and its toolbar) while click-through
//! everywhere else — so the game behind stays reachable but every pin can be
//! adjusted on the fly. Its defining behavior — suppress the inputs used to
//! navigate it, pass through the input the tweaked parameter affects — lands in
//! later phases (M3.3/M3.4) on the `__src_block__` machinery.
//!
//! M3.2: curated tweak-pins. The pick flow reuses the info overlay's armed-pick
//! machinery (amber highlights + the app.rs/subpatch.rs path resolution) via a
//! DESTINATION discriminator (`overlay_pick_dest_config`), and filters picks to
//! [`is_editable_element`] so only adjustable controls can be pinned. Rendering
//! + edit-mode arrange reuse [`show_overlay_body`] and the shared layout tools
//! verbatim, driving the per-tab `config` [`OverlayLayout`] instead of `overlay`.
//!
//! Same transparency machinery as the info + menu overlays (unique title +
//! transparent + skip-taskbar triggers the vendored NOREDIRECTIONBITMAP patch;
//! passthrough commands latched per state change).

use std::time::Duration;

use egui_snarl::Snarl;

use crate::app::FlexInputApp;
use crate::canvas::node::{ExposedModule, LayoutItem, OverlayLayout};
use crate::canvas::NodeData;

const CONFIG_OVERLAY_TITLE: &str = "FlexInput Config Overlay";

/// Ctx temp-data slot: is the config overlay currently summoned?
pub const CONFIG_OVERLAY_VISIBLE_KEY: &str = "fxi_config_overlay_visible";
/// Ctx temp-data slot: is the config overlay in edit (arrange tweak-pins) mode?
pub const CONFIG_OVERLAY_EDIT_KEY: &str = "fxi_config_overlay_edit";
/// Ctx temp-data slot: wall-clock time a non-editable pick was rejected, so the
/// "not tweakable" hint chip can fade after a couple seconds.
const CONFIG_REJECT_KEY: &str = "fxi_config_pick_rejected";
/// Ctx temp-data slot: the physical device id whose input should PASS THROUGH to
/// the game right now — the upstream device of the ACTIVE tweak-pin (the one
/// under the cursor, or the gamepad-focused one). Empty string = nothing passing
/// through. Written by the overlay each frame; read by `FlexInputApp::update`
/// when building the source-block set. (M3.4 mouse, M3.5 gamepad.)
pub const CONFIG_PASSTHROUGH_DEV_KEY: &str = "fxi_config_passthrough_dev";
/// Ctx temp-data slot: this frame's gamepad-navigable config pins, as
/// `(pass_nr, Vec<(item_index, screen_rect)>)`. Published by the overlay in live
/// mode; read by `nav_drive_config_overlay` to move focus between pins.
pub const CONFIG_NAV_TARGETS_KEY: &str = "fxi_config_nav_targets";

fn visible_id() -> egui::Id {
    egui::Id::new(CONFIG_OVERLAY_VISIBLE_KEY)
}
fn edit_id() -> egui::Id {
    egui::Id::new(CONFIG_OVERLAY_EDIT_KEY)
}
/// Toolbar bounds published each frame so the passthrough hit-test (which reads
/// the OS cursor — a click-through window gets no pointer events) keeps the
/// window interactive while the cursor is over the toolbar.
fn toolbar_rect_id() -> egui::Id {
    egui::Id::new("fxi_config_toolbar_rect")
}
fn reject_id() -> egui::Id {
    egui::Id::new(CONFIG_REJECT_KEY)
}
fn passthrough_dev_id() -> egui::Id {
    egui::Id::new(CONFIG_PASSTHROUGH_DEV_KEY)
}
/// The last non-FlexInput foreground window seen while the overlay is up —
/// normally the game. Foreground is handed back to it whenever the overlay
/// holds it without needing it (see the passthrough block).
fn game_hwnd_id() -> egui::Id {
    egui::Id::new("fxi_config_game_hwnd")
}

/// What the config overlay wants passed through to the game right now, as
/// `(device, pins)` — the active tweak-pin's upstream device and the SPECIFIC
/// pins of it the module reads (empty = whole device). Read by `update()` to
/// poke a hole in the source-block set. `None` = nothing passing through.
pub fn config_passthrough(ctx: &egui::Context) -> Option<(String, Vec<String>)> {
    ctx.data(|d| d.get_temp::<(String, Vec<String>)>(passthrough_dev_id()))
        .filter(|(dev, _)| !dev.is_empty())
}

fn nav_targets_id() -> egui::Id {
    egui::Id::new(CONFIG_NAV_TARGETS_KEY)
}

/// One legend glyph: a rasterized controller-button icon (skin-specific) or a
/// text token fallback. Built by `FlexInputApp::config_legend_specs` from the
/// shared `gp_legend_hints` + `gp_legend_glyph`, so the config overlay's legend
/// matches the Easy-mode bottom bar (same icons, same per-state hints).
pub(crate) enum ConfigGlyph {
    Tex(egui::TextureHandle),
    Token(String),
}

/// This frame's gamepad-navigable config-pin targets: `(item_index, screen_rect)`
/// for each Module pin. Empty when the overlay isn't showing pins (hidden, edit,
/// or pick mode). Read by `nav_drive_config_overlay`.
pub fn config_nav_targets(ctx: &egui::Context) -> Vec<(usize, egui::Rect)> {
    ctx.data(|d| d.get_temp::<(u64, Vec<(usize, egui::Rect)>)>(nav_targets_id()))
        .map(|(_, t)| t)
        .unwrap_or_default()
}

pub fn config_overlay_visible(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(visible_id())).unwrap_or(false)
}

pub fn set_config_overlay_visible(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(visible_id(), on));
    // Leaving the overlay drops edit mode too (mirrors set_overlay_visible),
    // and clears any lingering passthrough request so a closed overlay can't
    // keep a device unblocked.
    if !on {
        ctx.data_mut(|d| {
            d.remove_temp::<bool>(edit_id());
            d.remove_temp::<(String, Vec<String>)>(passthrough_dev_id());
            d.remove_temp::<isize>(game_hwnd_id());
        });
    }
}

/// Dedicated always-on-top viewport for the KB/M "Special" picker when it's
/// summoned from a pinned Remapper / Touch Zones body in the config overlay.
/// The main-window picker would draw behind the game (unreachable); this one
/// floats over it like the overlay itself. Distinct from the overlay viewport
/// so it doesn't clash with `show_config_overlay`'s own immediate viewport.
pub fn picker_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("fxi_config_kbm_picker")
}

pub fn config_overlay_edit(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(edit_id())).unwrap_or(false)
}

pub fn set_config_overlay_edit(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(edit_id(), on));
}

/// The pad buttons that drive a running measure sweep (A finishes, B cancels —
/// see `nav_drive_config_overlay`). They stay blocked from the game while the
/// rest of the device passes through.
const RWS_SWEEP_CONTROL_PINS: &[&str] = &["btn_south", "btn_east"];

/// Modules that run a measure calibration sweep on the shared `cal_*` params:
/// RWS Aim's own widget, and the JSM Config tune panel's "Calibrate RWC" row.
/// Both need the same two things from the overlay — the pad passed through so
/// the camera can be turned, and the 360° method's reference frame.
fn runs_measure_sweep(node: &NodeData) -> bool {
    matches!(node.module_id.as_str(), "processing.rws" | "module.jsm")
}

/// The sweep this node is running, if any: "pitch" or "yaw".
fn sweep_axis(node: &NodeData) -> Option<&str> {
    if !runs_measure_sweep(node) {
        return None;
    }
    node.params
        .get("cal_measure")
        .and_then(|v| v.as_str())
        .filter(|a| *a == "pitch" || *a == "yaw")
}

/// While an RWS pin is running a MEASURE calibration sweep (`cal_measure` =
/// pitch/yaw), the user's gyro/stick must reach the RWS node so the camera moves
/// — but the config overlay source-blocks every physical input by default. This
/// finds such a pin and returns its upstream physical device to force through,
/// overriding the normal hover/tweak gate. Every pin of the device passes (so
/// the sweep works even with selectors/curves/aim buttons before RWS) EXCEPT the
/// sweep's own controls; `live_signals` supplies the device's pins, since the
/// passthrough is an allowlist (an empty list would mean the whole device).
fn rws_measure_passthrough<V>(
    tab_snarl: &Snarl<NodeData>,
    config_layout: &OverlayLayout,
    live_signals: &std::collections::HashMap<(String, String), V>,
) -> Option<(String, Vec<String>)> {
    for it in &config_layout.items {
        let LayoutItem::Module(m) = it else { continue };
        let node = match m.source_path.as_slice() {
            [] => tab_snarl.get_node(egui_snarl::NodeId(m.inner_node_id)),
            [sp] => tab_snarl
                .get_node(egui_snarl::NodeId(*sp))
                .and_then(|n| n.subpatch.as_ref())
                .and_then(|s| s.snarl.get_node(egui_snarl::NodeId(m.inner_node_id))),
            _ => None,
        };
        let Some(node) = node else { continue };
        if sweep_axis(node).is_some() {
            if let Some(dev) = crate::app::config_passthrough_device(tab_snarl, &m.source_path, m.inner_node_id) {
                let pins: Vec<String> = live_signals
                    .keys()
                    .filter(|(d, p)| *d == dev && !RWS_SWEEP_CONTROL_PINS.contains(&p.as_str()))
                    .map(|(_, p)| p.clone())
                    .collect();
                // No pins seen for it (device gone): pass nothing rather than an
                // empty list, which would unblock the whole device.
                return (!pins.is_empty()).then_some((dev, pins));
            }
        }
    }
    None
}

/// Manage the RWS calibration REFERENCE frame: when a measure sweep with the
/// screenshot reference enabled starts, grab the game frame behind the overlay
/// once and cache it as a texture; drop it when the sweep ends. Returns the
/// texture to paint while a sweep is active. Keyed in ctx temp-data (only one
/// sweep runs at a time), so it survives across frames without an app field.
fn rws_reference_frame(
    ctx: &egui::Context,
    tab_snarl: &Snarl<NodeData>,
    config_layout: &OverlayLayout,
) -> Option<egui::TextureHandle> {
    let active = config_layout.items.iter().any(|it| {
        let LayoutItem::Module(m) = it else { return false };
        let node = match m.source_path.as_slice() {
            [] => tab_snarl.get_node(egui_snarl::NodeId(m.inner_node_id)),
            [sp] => tab_snarl
                .get_node(egui_snarl::NodeId(*sp))
                .and_then(|n| n.subpatch.as_ref())
                .and_then(|s| s.snarl.get_node(egui_snarl::NodeId(m.inner_node_id))),
            _ => None,
        };
        let Some(node) = node else { return false };
        node.params.get("cal_ref_shot").and_then(|v| v.as_bool()).unwrap_or(false)
            // Snapshot comparison is the 360° horizontal aid only.
            && sweep_axis(node) == Some("yaw")
    });

    let tex_key = egui::Id::new("fxi_rws_ref_tex");
    let prev_key = egui::Id::new("fxi_rws_ref_active");
    let prev = ctx.data(|d| d.get_temp::<bool>(prev_key)).unwrap_or(false);
    if active && !prev {
        // Sweep just started — capture the game frame (our layered overlay is
        // excluded by the plain BitBlt), cache as a texture on this ctx.
        let tex = crate::capture::capture_primary_monitor()
            .map(|img| ctx.load_texture("fxi_rws_ref", img, egui::TextureOptions::LINEAR));
        ctx.data_mut(|d| d.insert_temp(tex_key, tex));
    }
    if !active && prev {
        ctx.data_mut(|d| d.insert_temp(tex_key, None::<egui::TextureHandle>));
    }
    ctx.data_mut(|d| d.insert_temp(prev_key, active));
    if active {
        ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(tex_key)).flatten()
    } else {
        None
    }
}

/// Show the config overlay viewport (call once per frame from
/// `FlexInputApp::update`, right after the menu overlay). No-op while hidden.
pub fn show_config_overlay(app: &mut FlexInputApp, ctx: &egui::Context) {
    if !config_overlay_visible(ctx) {
        return;
    }
    let edit = config_overlay_edit(ctx);
    let frame_interval = Duration::from_secs_f64(1.0 / app.overlay_fps() as f64);
    // The gamepad-focused tweak-pin index (M3.5), read before the tab borrow.
    // Acts as the active pin when the mouse isn't hovering one.
    let gp_focus = app.config_nav_focus();
    // Gamepad state for the legend: (editing?, a pad is driving?).
    let (gp_editing, gp_pad_active) = app.config_nav_state();
    // Controller-icon legend groups, matching the Easy-mode bottom bar.
    let legend = if gp_pad_active { app.config_legend_specs(ctx) } else { Vec::new() };
    // For a focused mapping-module pin: the card whose curve is being edited (so
    // its input passes through). `None` = block everything (default for mapping).
    let remapper_card_edit = app.config_remapper_card_edit();
    let jsm_focus = app.config_jsm_focus();
    // While card-navigating a pinned Remapper / TZ list: (outer, inner, scope) so
    // the overlay draws the card glow (the selection now gates via nav_pass, so
    // no per-channel republish is needed — see the note in the closure below).
    let remap_glow = app.config_remap_glow();
    // Focused value-field to draw a glow ring on (computed here — the closure
    // can't borrow `app`); redrawn in the overlay viewport by the closure.
    let field_glow_target = app.config_field_glow_target();
    // Pass-through policy: OFF (default) = input reaches the game only while a pin
    // is actually being tweaked; ON = the focused pin's input always passes.
    let passthrough_default = app.config_passthrough_default();
    // Right-stick virtual cursor (drawn in the overlay viewport) — the SAME
    // reticle texture the main-window nav cursor uses, for visual consistency.
    let (gp_cursor_pos, gp_cursor_vis) = app.config_cursor();
    let cursor_tex = if gp_cursor_vis { app.nav_cursor_tex(ctx) } else { None };

    // A pick is only ours if it was armed by the config overlay. It's only
    // meaningful while editing (entered from the toolbar) — clear a stray one.
    let mut pick = crate::canvas::viewer::overlay_pick_active(ctx)
        && crate::canvas::viewer::overlay_pick_dest_config(ctx);
    if pick && !edit {
        crate::canvas::viewer::set_overlay_pick_active(ctx, false);
        pick = false;
    }

    let monitor_size = ctx
        .input(|i| i.viewport().monitor_size)
        .filter(|s| s.x > 1.0 && s.y > 1.0)
        .unwrap_or(egui::vec2(1920.0, 1080.0));

    // Remember the game (whatever non-FlexInput window is foreground) so the
    // overlay can hand foreground back after a click activated it. Cleared on
    // close, so a window from before the overlay was summoned is never raised.
    if let Some(hwnd) = crate::process_list::foreground_hwnd() {
        ctx.data_mut(|d| d.insert_temp(game_hwnd_id(), hwnd));
    }
    let game_hwnd = ctx.data(|d| d.get_temp::<isize>(game_hwnd_id()));

    let (tab, live_signals, panic_shortcut) = app.overlay_parts();
    // Disjoint field borrows: the snarl renders the pins, the config layout is
    // edited (mirrors `show_overlay`'s split on `tab.overlay`).
    let tab_snarl = &mut tab.canvas.snarl;
    let config_layout = &mut tab.config;

    // A pick landed this frame (the main canvas + sub-patch editors ran before
    // us in `update`, so the path-resolved result is already stashed). Only pin
    // it if the picked element is an editable control; otherwise flash a hint.
    if pick {
        if let Some((source_path, inner_uid, eid, size)) =
            crate::canvas::viewer::take_overlay_pick_result(ctx)
        {
            let pinnable = crate::canvas::overlay_body::resolve_overlay_module(
                tab_snarl, &source_path, inner_uid,
            )
            .map(|n| crate::canvas::viewer::is_pinnable_element(&n.module_id, &eid))
            .unwrap_or(false);
            if pinnable {
                let init_size = if size[0] >= 1.0 && size[1] >= 1.0 { size } else { [220.0, 100.0] };
                let n = config_layout.items.len() as f32;
                let cascade = (n % 8.0) * 28.0;
                let pos = [
                    (monitor_size.x - init_size[0]) * 0.5 + cascade,
                    (monitor_size.y - init_size[1]) * 0.5 + cascade,
                ];
                config_layout.items.push(LayoutItem::Module(ExposedModule {
                    inner_node_id: inner_uid,
                    element_id: eid,
                    pos,
                    size: init_size,
                    text_override: None,
                    switch_override: None,
                    graph_override: None,
                    source_path,
                    iv_style_override: None,
                    menu_style_override: None,
                    keep_aspect: false,
                    anchor: Default::default(),
                }));
                let idx = config_layout.items.len() - 1;
                config_layout.selected_item = Some(idx);
                config_layout.selected_items = vec![idx];
                config_layout.cycle_pos = None;
            } else {
                // Not an adjustable control (a viewer, scope, readout, label…).
                // Read the clock BEFORE taking the data lock — `ctx.input` and
                // `ctx.data_mut` both lock the same Context RwLock, so nesting
                // them self-deadlocks (epaint's 10s watchdog then panics).
                let now = ctx.input(|i| i.time);
                ctx.data_mut(|d| d.insert_temp(reject_id(), now));
            }
            crate::canvas::viewer::set_overlay_pick_active(ctx, false);
            pick = false;
        }
    }

    let viewport_id = egui::ViewportId::from_hash_of("fxi_config_overlay");
    let builder = egui::ViewportBuilder::default()
        .with_title(CONFIG_OVERLAY_TITLE)
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_mouse_passthrough(true)
        .with_active(false)
        .with_resizable(false)
        .with_has_shadow(false)
        .with_position(egui::pos2(0.0, 0.0))
        .with_inner_size(monitor_size);

    let mut exit_edit = false;
    let mut enter_edit = false;
    let mut close = false;
    // Set by the top-bar checkbox to the new pass-through-default value.
    let mut set_passthrough_default: Option<bool> = None;

    ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
        // Geometry self-correction from inside the viewport (see overlay.rs):
        // force the window to fill the monitor if the OS handed us a wrong size.
        let (inner_rect, child_monitor) = vctx.input(|i| {
            (i.viewport().inner_rect, i.viewport().monitor_size)
        });
        if let (Some(inner_rect), Some(want)) = (inner_rect, child_monitor) {
            if (inner_rect.size() - want).length() > 1.0 {
                vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
                vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
            }
        }

        const HIT_MARGIN: f32 = 14.0;
        let live = !edit && !pick;
        // Screen-anchor resolution for hit-test / passthrough / nav targets, so
        // these match the displayed (re-anchored) rects `show_overlay_body`
        // paints. Current overlay size = the monitor it fills.
        let cur_size: [f32; 2] = child_monitor
            .or_else(|| inner_rect.map(|r| r.size()))
            .map(|s| [s.x.max(1.0), s.y.max(1.0)])
            .unwrap_or([1920.0, 1080.0]);
        let authored = config_layout.authored_size.unwrap_or(cur_size);
        // Resolve every pin's rect up front (screen-anchored or following a
        // linked target), so hit-test / passthrough / nav targets match what
        // `show_overlay_body` paints. Precomputed into an owned Vec so the
        // closure doesn't borrow `config_layout` (which is mutated below).
        let resolved_rects: Vec<egui::Rect> = (0..config_layout.items.len())
            .map(|i| {
                let (p, s) = crate::canvas::node::resolve_layout_rect(
                    &config_layout.items, i, authored, cur_size,
                );
                egui::Rect::from_min_size(
                    egui::pos2(p[0], p[1]),
                    egui::vec2(s[0].max(8.0), s[1].max(8.0)),
                )
            })
            .collect();
        let item_rect = |i: usize| -> egui::Rect {
            resolved_rects.get(i).copied().unwrap_or(egui::Rect::NOTHING)
        };
        // OS cursor in overlay-local points (the overlay fills the monitor at
        // origin, so screen points == item coords). None during a pick.
        let cursor = if pick {
            None
        } else {
            crate::overlay::os_cursor_in_points(vctx.pixels_per_point())
        };

        // Publish the gamepad-navigable pin targets each live frame. Only pins
        // whose element is actually a nav-editable widget qualify — a purely
        // visual pin (scope/vector/oscilloscope display, 3D viewer, label, SVG)
        // is pinnable for feedback but must NOT steal gamepad focus (else a scope
        // stacked over a real control would intercept LS/dpad selection, leaving
        // only the RS cursor able to reach the control beneath it).
        if live {
            let targets: Vec<(usize, egui::Rect)> = config_layout
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, it)| {
                    let LayoutItem::Module(m) = it else { return None };
                    let editable = crate::canvas::overlay_body::resolve_overlay_module(
                        tab_snarl, &m.source_path, m.inner_node_id,
                    )
                    .map(|n| FlexInputApp::elem_is_nav_target(&n.module_id, &m.element_id))
                    .unwrap_or(false);
                    editable.then(|| (i, item_rect(i)))
                })
                .collect();
            let pass = vctx.cumulative_pass_nr();
            vctx.data_mut(|d| d.insert_temp(nav_targets_id(), (pass, targets)));
        }

        // An active RWS measure sweep forces its input device through regardless of
        // hover/tweak, so the gyro/stick actually moves the camera during cal.
        let cal_passthrough = rws_measure_passthrough(tab_snarl, config_layout, live_signals);
        // Hands-off: input is being driven INTO the game by something other than
        // the user's own pointer — a running measure sweep, or a gamepad editing a
        // pin — and either may be moving the OS cursor through a mouse output. The
        // overlay then ignores the cursor entirely (fully click-through, no
        // hover-picked pin) and keeps foreground with the game, so the cursor
        // crossing a pin can't pull the game's mouse away.
        let hands_off = live && (cal_passthrough.is_some() || gp_editing);

        // The ACTIVE tweak-pin: the topmost Module pin under the cursor (items
        // paint bottom→top, so the LAST match is on top), else the gamepad-focused
        // pin. Its upstream physical device passes through to the game (M3.4/M3.5)
        // — you feel/steer the parameter while adjusting and its live graph dot
        // keeps moving.
        let hovered_module_idx = cursor.filter(|_| !hands_off).and_then(|c| {
            config_layout.items.iter().enumerate().rev().find_map(|(i, it)| {
                matches!(it, LayoutItem::Module(_))
                    .then(|| item_rect(i).expand(HIT_MARGIN).contains(c))
                    .filter(|&hit| hit)
                    .map(|_| i)
            })
        });
        let gp_active = gp_focus.filter(|&i| {
            matches!(config_layout.items.get(i), Some(LayoutItem::Module(_)))
        });
        // When a gamepad owns the config focus (`config_index`, set only from the
        // gamepad's own RS cursor / d-pad — never from the OS pointer), IT is
        // authoritative for the passthrough target. The OS cursor may itself be
        // our virtual mouse (e.g. a gyro→mouse passthrough the user is testing);
        // letting it hover-steal onto another pin would suppress the very input
        // being tweaked. A pure-mouse session (no gamepad focus) still hover-picks
        // the pin under the cursor.
        let active_idx = if !live {
            None
        } else if gp_focus.is_some() {
            gp_active
        } else {
            hovered_module_idx.or(gp_active)
        };
        let raw_passthrough = active_idx.and_then(|i| match &config_layout.items[i] {
            LayoutItem::Module(m) => crate::app::config_passthrough_pins_for(
                tab_snarl, &m.source_path, m.inner_node_id, remapper_card_edit,
                jsm_focus.as_deref(),
            ),
            _ => None,
        });
        let dragging = vctx.input(|i| i.pointer.any_down()) || vctx.is_using_pointer();
        // Pass-through gating: by default the focused pin's input reaches the game
        // ONLY while it's actually being tweaked (gamepad-editing, or mouse-
        // dragging a control) — so merely navigating the overlay never drives the
        // game. The top-bar checkbox restores always-on passthrough for the
        // focused pin.
        let tweaking = gp_editing || dragging;
        let passthrough = if cal_passthrough.is_some() {
            cal_passthrough
        } else if passthrough_default || tweaking {
            raw_passthrough
        } else {
            None
        };
        if live && (active_idx.is_some() || !dragging) {
            // Overwrite the passthrough — but not on a stray cursor-off-pin frame
            // mid-drag, so a fast drag keeps the pin's input flowing.
            vctx.data_mut(|d| {
                d.insert_temp(passthrough_dev_id(), passthrough.clone().unwrap_or_default())
            });
        } else if !live {
            vctx.data_mut(|d| {
                d.insert_temp(passthrough_dev_id(), (String::new(), Vec::<String>::new()))
            });
        }

        // Passthrough (window click-through): interactive over the toolbar or any
        // pinned item, or during a drag/popup; click-through elsewhere so the game
        // stays reachable. During a pick the window is fully click-through so the
        // pin click lands on FlexInput behind it, and hands-off (above) is too.
        let interactive = if pick || hands_off {
            false
        } else {
            let over_toolbar = cursor
                .and_then(|c| {
                    vctx.data(|d| d.get_temp::<egui::Rect>(toolbar_rect_id()))
                        .map(|r| r.expand(4.0).contains(c))
                })
                .unwrap_or(false);
            let over_item = cursor
                .map(|c| (0..config_layout.items.len()).any(|i| item_rect(i).expand(HIT_MARGIN).contains(c)))
                .unwrap_or(false);
            egui::Popup::is_any_open(vctx) || dragging || over_toolbar || over_item || cursor.is_none()
        };
        let pt_id = egui::Id::new("fxi_config_passthrough_applied");
        let want_passthrough = !interactive;
        let applied: Option<bool> = vctx.data(|d| d.get_temp(pt_id));
        if applied != Some(want_passthrough) {
            // No `Focus` when turning interactive: that fired on mere hover —
            // including a game's hidden cursor parked over a pin when the overlay
            // is summoned — and took foreground from the game. A real click still
            // activates the overlay on its own.
            vctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(want_passthrough));
            vctx.data_mut(|d| d.insert_temp(pt_id, want_passthrough));
        }

        // Hand foreground back to the game once the overlay no longer needs it:
        // a click activated it, and now the cursor has left every pin (no drag,
        // popup or text entry in progress) or input went hands-off. Otherwise a
        // game that only reads input while focused stays deaf until the overlay
        // is re-summoned. Throttled in case the OS refuses the switch.
        let overlay_focused = vctx.input(|i| i.viewport().focused).unwrap_or(false);
        if let Some(hwnd) = game_hwnd.filter(|_| {
            overlay_focused
                && live
                && (hands_off || (want_passthrough && !vctx.wants_keyboard_input()))
        }) {
            let now = vctx.input(|i| i.time);
            let retry_id = egui::Id::new("fxi_config_fg_return_at");
            let last: f64 = vctx.data(|d| d.get_temp(retry_id)).unwrap_or(f64::NEG_INFINITY);
            if now - last > 0.25 {
                crate::process_list::return_foreground(hwnd);
                vctx.data_mut(|d| d.insert_temp(retry_id, now));
            }
        }

        // Esc: in a pick the main window handles cancel; otherwise Esc exits
        // edit mode, or (in live mode) dismisses the overlay.
        if !pick && vctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if edit { exit_edit = true; } else { close = true; }
        }

        // A pinned widget that scrolls (the JSM editor) takes the wheel here the
        // same way it does on the canvas — see `canvas/wheel.rs`.
        crate::canvas::wheel::take_wheel_from_canvas(vctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(vctx, |ui| {
                let rect = ui.max_rect();

                if pick {
                    // Pick state: collapse to the glowing pin-mode border (shared
                    // with the info overlay) so the FlexInput window behind is
                    // unobstructed while an element is chosen.
                    crate::overlay::paint_pick_frame(ui, rect);
                    return;
                }

                if edit {
                    // Faint dim so edit mode reads as a distinct state.
                    ui.painter().rect_filled(rect, 0.0, egui::Color32::from_black_alpha(48));
                    if config_layout.items.is_empty() {
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "No tweak-pins yet — use “Add element” to pin an\nadjustable control (slider, curve, toggle, numeric row).",
                            egui::FontId::proportional(15.0),
                            egui::Color32::from_rgba_unmultiplied(220, 235, 255, 220),
                        );
                    }
                }

                // NOTE: the per-channel highlight re-stamps that used to live here
                // (curve dots/bias, gp_nav_active, gp_nav_tz, remapper card/field/
                // action) are gone — the pinned bodies now gate those channels via
                // `crate::widgets::nav_pass`, which returns the ROOT nav pass in any
                // viewport, so the highlights match in this overlay viewport with no
                // republishing. New highlight channels get this for free.

                // RWS calibration reference: the frozen game frame's LEFT half at
                // 70%, so the user turns until the live right half realigns (a full
                // 360° / the pitch endpoint). Drawn under the pins/toolbar.
                if !edit {
                    if let Some(tex) = rws_reference_frame(vctx, tab_snarl, config_layout) {
                        let painter = ui.painter();
                        let left = egui::Rect::from_min_max(
                            rect.min,
                            egui::pos2(rect.center().x, rect.max.y),
                        );
                        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(0.5, 1.0));
                        painter.image(tex.id(), left, uv, egui::Color32::from_white_alpha(179));
                        painter.line_segment(
                            [egui::pos2(rect.center().x, rect.min.y), egui::pos2(rect.center().x, rect.max.y)],
                            egui::Stroke::new(1.0, egui::Color32::from_white_alpha(130)),
                        );
                        painter.text(
                            egui::pos2(left.center().x, rect.min.y + 40.0),
                            egui::Align2::CENTER_CENTER,
                            "Reference — turn until the live view realigns, then Finish",
                            egui::FontId::proportional(15.0),
                            egui::Color32::from_rgba_unmultiplied(255, 230, 180, 230),
                        );
                    }
                }

                crate::canvas::overlay_body::show_overlay_body(
                    ui, rect, tab_snarl, config_layout, edit,
                    live_signals, &panic_shortcut,
                );

                // Draw the mapping-card selection glow on THIS viewport (the
                // shared drawer is viewport-agnostic; run here so the ring appears
                // over the overlay, not behind it in the main window).
                if let Some((outer, inner, scope)) = &remap_glow {
                    crate::app::draw_remap_card_glow(ui.ctx(), *outer, *inner, scope);
                }

                // Value-field bloom ring (knob / min-max / range rows). nav draws
                // this on the main window; redraw it here so it shows over the game.
                crate::app::draw_config_field_glow(ui.ctx(), field_glow_target.clone());

                // Focus ring on the active pin (its input is passing through);
                // brighter + larger while it's being edited, like Easy mode.
                // Drawn after the body so it sits on top; the &mut borrow above
                // has ended, so reading the item back is safe.
                if let Some(i) = active_idx.filter(|&i| i < config_layout.items.len()) {
                    // Resolve to the re-anchored rect (not the raw authored bbox)
                    // so the ring tracks the pin after a cross-resolution shift —
                    // matching what `show_overlay_body` paints.
                    let r = item_rect(i);
                    paint_focus_ring(ui, r, gp_editing);
                }

                paint_reject_hint(ui, rect);

                config_toolbar(
                    ui, tab_snarl, config_layout, edit,
                    &mut exit_edit, &mut enter_edit, &mut close,
                    passthrough_default, &mut set_passthrough_default,
                );

                // Gamepad navigation legend along the bottom (live mode only) —
                // controller icons + per-state hints, matching Easy mode.
                if !edit && !legend.is_empty() {
                    paint_config_legend(ui, rect, &legend);
                }
                // Right-stick virtual cursor — the shared reticle texture (falls
                // back to a drawn ring only if the texture failed to load).
                if gp_cursor_vis && !pick {
                    match &cursor_tex {
                        Some(tex) => {
                            let size = egui::vec2(56.0, 56.0);
                            let r = egui::Rect::from_center_size(gp_cursor_pos, size);
                            ui.painter().image(
                                tex.id(),
                                r,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                egui::Color32::WHITE,
                            );
                        }
                        None => paint_nav_cursor(ui, gp_cursor_pos),
                    }
                }
            });
    });

    if enter_edit {
        set_config_overlay_edit(ctx, true);
    }
    if exit_edit {
        set_config_overlay_edit(ctx, false);
        crate::canvas::viewer::set_overlay_pick_active(ctx, false);
    }
    if close {
        set_config_overlay_visible(ctx, false);
    }
    if let Some(v) = set_passthrough_default {
        app.set_config_passthrough_default(v);
    }
    // A pinned widget (RWS calibration, the JSM editor) wrote node params from
    // this viewport, which the canvas's own edit tracking can't see. Bump the tab
    // canvas generation so an open sub-patch editor re-pulls those values rather
    // than displaying — or writing back over them — its stale copy.
    if crate::canvas::viewer::take_overlay_param_write(ctx) {
        let (tab, _, _) = app.overlay_parts();
        tab.canvas.mutation_gen = tab.canvas.mutation_gen.wrapping_add(1);
    }
    // Pace the parent context (immediate viewports render with the parent).
    ctx.request_repaint_after(frame_interval);
}

/// The always-present top-center toolbar: title, edit toggle, Done. In edit
/// mode it expands with "Add element" + the shared layout tools (snap grid,
/// decoration adders) on row 1 and the selected item's inspector strip on row 2.
fn config_toolbar(
    ui: &mut egui::Ui,
    tab_snarl: &Snarl<NodeData>,
    config_layout: &mut OverlayLayout,
    edit: bool,
    exit_edit: &mut bool,
    enter_edit: &mut bool,
    close: &mut bool,
    passthrough_default: bool,
    set_passthrough_default: &mut Option<bool>,
) {
    let area = egui::Area::new(egui::Id::new("fxi_config_toolbar"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
        .interactable(true);
    let area_resp = area.show(ui.ctx(), |ui| {
        let bg = ui.visuals().window_fill();
        egui::Frame::default()
            .fill(egui::Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 240))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(120, 140, 200)))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                // Selected-item info computed before the mutable layout borrow.
                let sel_module =
                    crate::canvas::overlay_body::overlay_selected_module_info(tab_snarl, config_layout);
                let mut state = crate::canvas::viewer::LayoutStateMut::of_overlay(config_layout);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("⚙ Config")
                                .strong()
                                .color(egui::Color32::from_rgb(200, 215, 255)),
                        );
                        ui.separator();
                        if ui
                            .add(egui::Button::selectable(edit, egui::RichText::new("✏ Edit")))
                            .on_hover_text("Arrange tweak-pins: add / move / resize / remove.\nExit back to live tweaking with Esc or Done.")
                            .clicked()
                        {
                            if edit { *exit_edit = true; } else { *enter_edit = true; }
                        }
                        if ui
                            .button(egui::RichText::new("✔ Done").strong())
                            .on_hover_text("Close the config overlay (or press the shortcut).")
                            .clicked()
                        {
                            *close = true;
                        }
                        ui.separator();
                        let mut pt = passthrough_default;
                        if ui
                            .checkbox(&mut pt, "Pass-through")
                            .on_hover_text(
                                "OFF (default): input reaches the game only while a pin is\n\
                                 actually being tweaked — navigating the overlay is fully\n\
                                 suppressed.\nON: the focused pin's input always passes through\n\
                                 (navigating also drives the game).",
                            )
                            .changed()
                        {
                            *set_passthrough_default = Some(pt);
                        }
                        if edit {
                            ui.separator();
                            if ui.button("➕ Add element")
                                .on_hover_text("Pick an adjustable control to pin: the overlay collapses to a\nglowing border and pinnable elements light up amber in the\nFlexInput window. Non-adjustable elements are ignored. Esc cancels.")
                                .clicked()
                            {
                                crate::canvas::viewer::set_overlay_pick_active(ui.ctx(), true);
                                crate::canvas::viewer::set_overlay_pick_dest_config(ui.ctx(), true);
                                // Bring the main window forward so the highlighted
                                // elements are visible/clickable.
                                ui.ctx().send_viewport_cmd_to(
                                    egui::ViewportId::ROOT,
                                    egui::ViewportCommand::Minimized(false),
                                );
                                ui.ctx().send_viewport_cmd_to(
                                    egui::ViewportId::ROOT,
                                    egui::ViewportCommand::Focus,
                                );
                            }
                            ui.separator();
                            crate::canvas::viewer::layout_toolbar_controls_core(ui, &mut state);
                        }
                    });
                    if edit {
                        crate::canvas::viewer::layout_inspector_strip_core(ui, &mut state, sel_module);
                    }
                });
            });
    });
    ui.ctx().data_mut(|d| d.insert_temp(toolbar_rect_id(), area_resp.response.rect));
}

/// Right-stick virtual cursor: a target-ring reticle at `pos`. Drawn directly
/// (no texture upload) so it works inside the overlay viewport with no asset
/// plumbing. Purely visual — the nav driver reads `cursor_pos` to focus pins.
fn paint_nav_cursor(ui: &mut egui::Ui, pos: egui::Pos2) {
    let accent = ui.visuals().selection.stroke.color;
    let p = ui.painter();
    // Soft outer halo.
    for (r, a) in [(13.0_f32, 45.0_f32), (10.0, 90.0)] {
        p.circle_stroke(
            pos,
            r,
            egui::Stroke::new(3.0, egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), a as u8)),
        );
    }
    p.circle_stroke(pos, 8.0, egui::Stroke::new(2.0, accent));
    p.circle_filled(pos, 2.0, accent);
    // Crosshair ticks.
    let tick = egui::Stroke::new(1.5, accent);
    for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let d = egui::vec2(dx, dy);
        p.line_segment([pos + d * 8.0, pos + d * 13.0], tick);
    }
}

/// Bottom-centered gamepad legend: controller-button icons + labels per group,
/// with `/` between multi-glyph groups and a divider between groups — the same
/// visual language as Easy mode's `draw_gp_legend_bar`, painted inside the
/// overlay viewport from pre-rasterized glyph handles.
fn paint_config_legend(ui: &mut egui::Ui, rect: egui::Rect, legend: &[(Vec<ConfigGlyph>, String)]) {
    const GLYPH: f32 = 22.0;
    const LABEL_GAP: f32 = 4.0;
    const SLASH_GAP: f32 = 4.0;
    const DIV_GAP: f32 = 10.0;
    let label_font = egui::FontId::proportional(14.0);
    let tok_font = egui::FontId::proportional(14.0);
    let p = ui.painter();
    let measure = |s: &str, f: &egui::FontId| {
        p.layout_no_wrap(s.to_string(), f.clone(), egui::Color32::WHITE).size().x
    };
    let slash_w = measure("/", &tok_font);

    // Measure total width to center the bar.
    let mut total = 0.0f32;
    for (gi, (glyphs, label)) in legend.iter().enumerate() {
        if gi > 0 {
            total += DIV_GAP * 2.0 + 1.0;
        }
        for (j, g) in glyphs.iter().enumerate() {
            if j > 0 {
                total += SLASH_GAP * 2.0 + slash_w;
            }
            total += match g {
                ConfigGlyph::Tex(_) => GLYPH,
                ConfigGlyph::Token(t) => measure(t, &tok_font),
            };
        }
        total += LABEL_GAP + measure(label, &label_font);
    }
    let pad = egui::vec2(16.0, 8.0);
    let bar = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.bottom() - (GLYPH + pad.y * 2.0) * 0.5 - 10.0),
        egui::vec2(total + pad.x * 2.0, GLYPH + pad.y * 2.0),
    );
    p.rect_filled(bar, 8.0, egui::Color32::from_rgba_unmultiplied(16, 18, 26, 225));
    p.rect_stroke(
        bar,
        8.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(120, 140, 200, 150)),
        egui::StrokeKind::Inside,
    );

    // Lay out left→right, vertically centered in the bar.
    let cy = bar.center().y;
    let mut x = bar.min.x + pad.x;
    let div_col = ui.visuals().weak_text_color();
    let label_col = egui::Color32::from_gray(225);
    for (gi, (glyphs, label)) in legend.iter().enumerate() {
        if gi > 0 {
            x += DIV_GAP;
            p.vline(x, (cy - GLYPH * 0.5)..=(cy + GLYPH * 0.5), egui::Stroke::new(1.0, div_col));
            x += DIV_GAP;
        }
        for (j, g) in glyphs.iter().enumerate() {
            if j > 0 {
                x += SLASH_GAP;
                let gal = p.layout_no_wrap("/".to_string(), tok_font.clone(), div_col);
                p.galley(egui::pos2(x, cy - gal.size().y * 0.5), gal, div_col);
                x += slash_w + SLASH_GAP;
            }
            match g {
                ConfigGlyph::Tex(tex) => {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(x, cy - GLYPH * 0.5),
                        egui::vec2(GLYPH, GLYPH),
                    );
                    p.image(
                        tex.id(),
                        r,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    x += GLYPH;
                }
                ConfigGlyph::Token(t) => {
                    let gal = p.layout_no_wrap(t.clone(), tok_font.clone(), egui::Color32::WHITE);
                    let sz = gal.size();
                    p.galley(egui::pos2(x, cy - sz.y * 0.5), gal, egui::Color32::WHITE);
                    x += sz.x;
                }
            }
        }
        x += LABEL_GAP;
        let gal = p.layout_no_wrap(label.clone(), label_font.clone(), label_col);
        let sz = gal.size();
        p.galley(egui::pos2(x, cy - sz.y * 0.5), gal, label_col);
        x += sz.x;
    }
}

/// Outward accent bloom around the ACTIVE tweak-pin — the same selection-accent
/// glow the Easy-mode field HUD draws on a focused widget (6 outward rings in the
/// theme selection color with falling alpha), so focus reads identically here.
fn paint_focus_ring(ui: &mut egui::Ui, rect: egui::Rect, editing: bool) {
    let accent = ui.visuals().selection.stroke.color;
    let [r, g, b, _] = accent.to_array();
    let p = ui.painter();
    // Editing reads as a stronger, wider bloom than plain focus.
    let (rings, spread, peak) = if editing { (8, 12.0f32, 210.0f32) } else { (6, 7.0, 150.0) };
    for i in 0..rings {
        let t = (i as f32 + 1.0) / rings as f32;
        let grow = t * spread;
        let a = (peak * (1.0 - t)).round() as u8;
        if a == 0 {
            continue;
        }
        p.rect_stroke(
            rect.expand(grow),
            5.0 + grow,
            egui::Stroke::new(if editing { 2.5 } else { 2.0 }, egui::Color32::from_rgba_unmultiplied(r, g, b, a)),
            egui::StrokeKind::Outside,
        );
    }
    p.rect_stroke(
        rect.expand(1.5),
        5.0,
        egui::Stroke::new(if editing { 2.5 } else { 2.0 }, accent),
        egui::StrokeKind::Outside,
    );
}

/// Flash a "not tweakable" chip for a couple seconds after a rejected pick.
fn paint_reject_hint(ui: &mut egui::Ui, rect: egui::Rect) {
    let Some(t0) = ui.ctx().data(|d| d.get_temp::<f64>(reject_id())) else { return; };
    let age = ui.input(|i| i.time) - t0;
    if age > 2.5 {
        ui.ctx().data_mut(|d| d.remove_temp::<f64>(reject_id()));
        return;
    }
    let fade = (1.0 - (age / 2.5)).clamp(0.0, 1.0) as f32;
    let msg = "That element isn't adjustable — pick a slider, curve, toggle, or numeric row.";
    let font = egui::FontId::proportional(14.0);
    let p = ui.painter();
    let galley = p.layout_no_wrap(msg.to_string(), font, egui::Color32::from_rgb(255, 210, 160));
    let pad = egui::vec2(14.0, 8.0);
    let chip = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 90.0),
        galley.size() + pad * 2.0,
    );
    let a = |base: u8| (base as f32 * fade) as u8;
    p.rect_filled(chip, 8.0, egui::Color32::from_rgba_unmultiplied(40, 26, 10, a(235)));
    p.rect_stroke(
        chip, 8.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(230, 160, 60, a(255))),
        egui::StrokeKind::Inside,
    );
    p.galley(chip.min + pad, galley, egui::Color32::from_rgba_unmultiplied(255, 235, 200, a(255)));
    // Fade needs frames; the parent-paced overlay interval covers it.
}
