//! Real World Calibration: RWS Aim's guided calibration, run over the game.
//!
//! JSM Config runs the same procedure on the same params, its answer going into the config's
//! text; everything here serves both, and only the outputs offered differ (a JSM config has
//! no gyro output to scale).
//!
//! An RWS Aim module doesn't carry its calibration controls on the canvas any more. Its body
//! has one button that pins a calibration widget (element [`ELEMENT`]) to the config overlay
//! of the tab in front - or says the widget is already there, naming the pad the module
//! reads. The widget is one big button; pressed with the pad (gamepad navigation has to be
//! on: the whole procedure is driven from the pad), it starts a session that this module
//! draws as large prompts in a click-through window of its own, over the game:
//!
//! 1. **Choose** what to calibrate: South the mouse output (Mouse Scale), West the stick
//!    output (Stick °/s), North the gyro output (Gyro scale, for games with gyro aiming of
//!    their own); East cancels. The pad's handler for this is in `app::nav::config`.
//! 2. **Turn** one full 360° in game: the flick stick for the bulk of it, the controller to
//!    line up exactly, against the snapshot of where the turn started (always on here - a
//!    360° is lined up against something). The angle the engine has measured is shown as it
//!    grows. South finishes, East cancels: the same sweep and the same back-solve the old
//!    widget ran (`cal_measure`, `cal_finish`).
//! 3. **Result**: what changed, for a few seconds.
//!
//! The session lives on the node (`cal_session` = `choose` / `guided`), like the sweep it
//! drives, so every window that draws the node sees the same state.

use std::time::Duration;

use egui::{Align2, Color32, Stroke};
use egui_snarl::{InPinId, NodeId, Snarl};
use flexinput_core::Signal;
use serde_json::Value;

use crate::app::PatchTab;
use crate::canvas::node::{ExposedModule, LayoutItem};
use crate::canvas::NodeData;

/// The pinnable element the calibration widget is.
pub(crate) const ELEMENT: &str = "rws_cal";

const TITLE: &str = "FlexInput Real World Calibration";
/// How long the result stays up once a sweep has ended.
const RESULT_S: f64 = 4.0;

fn pin_requests_id() -> egui::Id {
    egui::Id::new("fxi_rws_cal_pin_requests")
}
fn pinned_id() -> egui::Id {
    egui::Id::new("fxi_rws_cal_pinned")
}
fn passthrough_id() -> egui::Id {
    egui::Id::new("fxi_rws_cal_passthrough")
}
fn ended_id() -> egui::Id {
    egui::Id::new("fxi_rws_cal_ended")
}

/// Ask for RWS node `inner` (in the sub-patch at `path`, empty on the tab's own canvas) to
/// get its calibration widget pinned to the config overlay. Applied by [`sync`].
pub(crate) fn request_pin(ctx: &egui::Context, path: Vec<usize>, inner: usize) {
    ctx.data_mut(|d| {
        let list = d.get_temp_mut_or_default::<Vec<(Vec<usize>, usize)>>(pin_requests_id());
        if !list.iter().any(|(p, i)| *p == path && *i == inner) {
            list.push((path, inner));
        }
    });
}

/// Whether that node's calibration widget is pinned to the config overlay of the tab in
/// front, as [`sync`] last found it.
pub(crate) fn is_pinned(ctx: &egui::Context, path: &[usize], inner: usize) -> bool {
    ctx.data(|d| {
        d.get_temp::<Vec<(Vec<usize>, usize)>>(pinned_id())
            .is_some_and(|l| l.iter().any(|(p, i)| p.as_slice() == path && *i == inner))
    })
}

/// Apply the pin requests and publish which nodes have the widget pinned. Once per frame
/// from `FlexInputApp::update`, before the config overlay is drawn.
pub(crate) fn sync(ctx: &egui::Context, tabs: &mut [PatchTab], active: usize) {
    let Some(tab) = tabs.get_mut(active) else { return };
    let requests: Vec<(Vec<usize>, usize)> = ctx.data_mut(|d| d.remove_temp(pin_requests_id())).unwrap_or_default();
    if !requests.is_empty() {
        let monitor = ctx
            .input(|i| i.viewport().monitor_size)
            .filter(|m| m.x > 1.0 && m.y > 1.0)
            .unwrap_or(egui::vec2(1920.0, 1080.0));
        for (path, inner) in requests {
            let have = tab.config.items.iter().any(|it| {
                matches!(it, LayoutItem::Module(m)
                    if m.inner_node_id == inner && m.source_path == path && m.element_id == ELEMENT)
            });
            if have {
                continue;
            }
            // Near the top of the screen, out of the middle where the game's crosshair is,
            // stacked when there is more than one.
            let size = [360.0, 60.0];
            let stacked = tab.config.items.iter().filter(|it| matches!(it, LayoutItem::Module(m) if m.element_id == ELEMENT)).count();
            let pos = [(monitor.x - size[0]) * 0.5, monitor.y * 0.08 + stacked as f32 * (size[1] + 12.0)];
            // On every tab of the overlay: a calibration's prompt can't hide behind one.
            tab.config.items.push(LayoutItem::Module(ExposedModule {
                tab: crate::canvas::node::ALL_TABS,
                inner_node_id: inner,
                element_id: ELEMENT.to_string(),
                pos,
                size,
                text_override: None,
                switch_override: None,
                graph_override: None,
                source_path: path,
                iv_style_override: None,
                menu_style_override: None,
                keep_aspect: false,
                anchor: Default::default(),
                pin_style: Some(2),
                zoom: None,
                hidden_sections: Vec::new(),
                sections_open: false,
                graph_labels: None,
                text_max: None,
            }));
        }
    }
    let pinned: Vec<(Vec<usize>, usize)> = tab
        .config
        .items
        .iter()
        .filter_map(|it| match it {
            LayoutItem::Module(m) if m.element_id == ELEMENT => Some((m.source_path.clone(), m.inner_node_id)),
            _ => None,
        })
        .collect();
    ctx.data_mut(|d| d.insert_temp(pinned_id(), pinned));
}

/// The snarl a pinned node lives in: the tab's own canvas, or the sub-patch at `path`.
fn snarl_at<'a>(tab: &'a PatchTab, path: &[usize]) -> Option<&'a Snarl<NodeData>> {
    match path {
        [] => Some(&tab.canvas.snarl),
        [sp] => tab.canvas.snarl.get_node(NodeId(*sp)).and_then(|n| n.subpatch.as_ref()).map(|s| &*s.snarl),
        _ => None,
    }
}

fn snarl_at_mut<'a>(tab: &'a mut PatchTab, path: &[usize]) -> Option<&'a mut Snarl<NodeData>> {
    match path {
        [] => Some(&mut tab.canvas.snarl),
        [sp] => tab.canvas.snarl.get_node_mut(NodeId(*sp)).and_then(|n| n.subpatch.as_mut()).map(|s| &mut *s.snarl),
        _ => None,
    }
}

/// The name of the pad an RWS node reads: whatever drives its Rotation input. Empty when
/// nothing does.
pub(crate) fn pad_name(snarl: &Snarl<NodeData>, node: NodeId) -> String {
    let Some(src) = snarl.in_pin(InPinId { node, input: 0 }).remotes.first().copied() else {
        return String::new();
    };
    snarl.get_node(src.node).map(|n| n.display_name.clone()).unwrap_or_default()
}

/// The session's step, from the node's params.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Step {
    Choose,
    Turning,
    Result,
}

/// Everything the prompt draws, read off the node.
struct View {
    step: Step,
    /// A JSM Config node rather than RWS Aim: two outputs, named as its config names them.
    jsm: bool,
    pad: String,
    output: String,
    theta: f32,
    peak: f32,
    flick_wired: bool,
    result: Option<String>,
}

/// Run the session, if one is on, and draw its prompt. Once per frame from
/// `FlexInputApp::update`, beside the other over-the-game windows. No-op without one.
pub(crate) fn show(ctx: &egui::Context, tabs: &mut [PatchTab], active: usize, overlay_fps: u32) {
    let now = ctx.input(|i| i.time);
    let Some(tab) = tabs.get_mut(active) else { return };
    // The pinned calibration widget whose node has a session on.
    let pins: Vec<(Vec<usize>, usize)> = tab
        .config
        .items
        .iter()
        .filter_map(|it| match it {
            LayoutItem::Module(m) if m.element_id == ELEMENT => Some((m.source_path.clone(), m.inner_node_id)),
            _ => None,
        })
        .collect();
    let mut found: Option<(Vec<usize>, usize, View)> = None;
    for (path, inner) in pins {
        let Some(snarl) = snarl_at(tab, &path) else { continue };
        let id = NodeId(inner);
        let Some(node) = snarl.get_node(id) else { continue };
        let session = node.params.get("cal_session").and_then(|v| v.as_str()).unwrap_or("off");
        if session != "choose" && session != "guided" {
            continue;
        }
        let measuring = matches!(node.params.get("cal_measure").and_then(|v| v.as_str()), Some("pitch" | "yaw"));
        let float_out = |i: usize| match node.extra.last_out.get(i).copied().flatten() {
            Some(Signal::Float(f)) if f.is_finite() => f,
            _ => 0.0,
        };
        let n = node.outputs.len();
        let jsm = node.module_id == "module.jsm";
        let (theta, peak) = if jsm {
            crate::canvas::viewer::jsm_sweep_reading(snarl, id)
        } else {
            (
                float_out(flexinput_engine::eval::rws_cal_deg_out(n)),
                float_out(flexinput_engine::eval::rws_cal_peak_out(n)),
            )
        };
        let step = match (session, measuring) {
            ("choose", _) => Step::Choose,
            (_, true) => Step::Turning,
            _ => Step::Result,
        };
        let view = View {
            step,
            jsm,
            pad: pad_name(snarl, id),
            output: node.params.get("cal_output").and_then(|v| v.as_str()).unwrap_or("mouse").to_string(),
            theta,
            peak,
            // A JSM config turns the camera with whichever stick it maps; RWS Aim with the
            // stick wired to its Flick input.
            flick_wired: jsm || !snarl.in_pin(InPinId { node: id, input: 1 }).remotes.is_empty(),
            result: if jsm {
                crate::canvas::viewer::jsm_cal_last_result(ctx, id)
            } else {
                crate::canvas::viewer::rws_last_result(ctx, id)
            }
            .map(|(m, _)| m),
        };
        found = Some((path, inner, view));
        break;
    }
    let Some((path, inner, view)) = found else {
        ctx.data_mut(|d| {
            d.remove_temp::<bool>(passthrough_id());
            d.remove_temp::<f64>(ended_id());
        });
        return;
    };

    // The result's own clock: from the frame the sweep was seen to end. Past it, the
    // session is over.
    if view.step == Step::Result {
        let ended: f64 = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(ended_id(), || now));
        if now - ended > RESULT_S {
            if let Some(node) = snarl_at_mut(tab, &path).and_then(|s| s.get_node_mut(NodeId(inner))) {
                node.params.insert("cal_session".into(), Value::String("off".into()));
            }
            tab.canvas.mutation_gen = tab.canvas.mutation_gen.wrapping_add(1);
            ctx.data_mut(|d| d.remove_temp::<f64>(ended_id()));
            return;
        }
    } else {
        ctx.data_mut(|d| d.remove_temp::<f64>(ended_id()));
    }

    let (overlay_pos, monitor_size) = crate::monitors::overlay_geometry(ctx);
    let viewport_id = egui::ViewportId::from_hash_of("fxi_rws_cal_overlay");
    let builder = egui::ViewportBuilder::default()
        .with_app_id(crate::APP_ID)
        .with_title(TITLE)
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_mouse_passthrough(true)
        .with_active(false)
        .with_resizable(false)
        .with_has_shadow(false)
        .with_position(overlay_pos)
        .with_inner_size(monitor_size);
    ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
        crate::monitors::keep_overlay_on_monitor(vctx);
        if vctx.data(|d| d.get_temp::<bool>(passthrough_id())).is_none() {
            vctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(true));
            vctx.data_mut(|d| d.insert_temp(passthrough_id(), true));
        }
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(vctx, |ui| {
            paint(ui, ui.max_rect(), &view, now);
        });
    });
    ctx.request_repaint_after(Duration::from_secs_f64(1.0 / overlay_fps.max(1) as f64));
}

const ACCENT: Color32 = Color32::from_rgb(110, 190, 255);
const PANEL: Color32 = Color32::from_rgba_premultiplied(14, 16, 20, 225);
const TEXT: Color32 = Color32::from_rgb(235, 238, 242);
const WEAK: Color32 = Color32::from_rgb(160, 168, 178);
const WARN: Color32 = Color32::from_rgb(240, 170, 100);

/// The panel a step is drawn on: rounded, dark, see-through enough to keep the game in view.
fn panel(ui: &egui::Ui, rect: egui::Rect) {
    ui.painter().rect(rect, 14.0, PANEL, Stroke::new(1.5_f32, Color32::from_white_alpha(40)), egui::StrokeKind::Inside);
}

fn output_name(output: &str, jsm: bool) -> &'static str {
    match (output, jsm) {
        ("stick", true) => "VIRTUAL_STICK_CALIBRATION",
        (_, true) => "REAL_WORLD_CALIBRATION",
        ("stick", false) => "Stick °/s",
        ("gyro", false) => "Gyro scale",
        _ => "Mouse Scale",
    }
}

fn paint(ui: &mut egui::Ui, screen: egui::Rect, v: &View, now: f64) {
    let s = (screen.height() / 1080.0).clamp(0.7, 2.0);
    let title = if v.pad.is_empty() { "Real World Calibration".to_string() } else { format!("Real World Calibration  ·  {}", v.pad) };
    match v.step {
        Step::Choose => {
            // At the bottom: the middle of the screen is what the turn will be lined up on.
            let size = egui::vec2(820.0 * s, 300.0 * s);
            let rect = egui::Rect::from_center_size(egui::pos2(screen.center().x, screen.bottom() - size.y * 0.5 - 60.0 * s), size);
            panel(ui, rect);
            let p = ui.painter();
            p.text(rect.center_top() + egui::vec2(0.0, 34.0 * s), Align2::CENTER_CENTER, &title, crate::settings::ui_font(28.0 * s), TEXT);
            // The camera is live: aim it now, level, so the turn that follows is a turn
            // about the vertical and the snapshot it is lined up against is level too.
            p.text(
                rect.center_top() + egui::vec2(0.0, 66.0 * s),
                Align2::CENTER_CENTER,
                "The camera is yours: aim straight ahead, level with the horizon",
                crate::settings::ui_font(18.0 * s),
                TEXT,
            );
            p.text(
                rect.center_top() + egui::vec2(0.0, 90.0 * s),
                Align2::CENTER_CENTER,
                "then choose what to calibrate - the turn starts from where you are aiming",
                crate::settings::ui_font(15.0 * s),
                WEAK,
            );
            let all = [
                ("{btn_south}", "Mouse", "the game's mouse aiming", output_name("mouse", v.jsm)),
                ("{btn_west}", "Stick", "the game's stick turning", output_name("stick", v.jsm)),
                ("{btn_north}", "Gyro", "a game's own gyro aiming", output_name("gyro", v.jsm)),
            ];
            let cards = if v.jsm { &all[..2] } else { &all[..] };
            let n = cards.len() as f32;
            let cw = (size.x - 20.0 * s * (n + 1.0)) / n;
            for (i, (pin, name, what, sets)) in cards.iter().enumerate() {
                let c = egui::Rect::from_min_size(
                    egui::pos2(rect.left() + 20.0 * s + i as f32 * (cw + 20.0 * s), rect.top() + 112.0 * s),
                    egui::vec2(cw, 130.0 * s),
                );
                ui.painter().rect(c, 10.0, Color32::from_white_alpha(10), Stroke::new(1.0_f32, Color32::from_white_alpha(30)), egui::StrokeKind::Inside);
                crate::canvas::viewer::pad_hint_big_centered(
                    ui,
                    c.center_top() + egui::vec2(0.0, 34.0 * s),
                    &format!("{pin} {name}"),
                    30.0 * s,
                    TEXT,
                );
                let p = ui.painter();
                p.text(c.center_top() + egui::vec2(0.0, 76.0 * s), Align2::CENTER_CENTER, *what, crate::settings::ui_font(16.0 * s), WEAK);
                p.text(c.center_top() + egui::vec2(0.0, 104.0 * s), Align2::CENTER_CENTER, format!("sets {sets}"), crate::settings::ui_font(13.0 * s), ACCENT);
            }
            crate::canvas::viewer::pad_hint_big_centered(
                ui,
                rect.center_bottom() - egui::vec2(0.0, 26.0 * s),
                "{btn_east} Cancel",
                18.0 * s,
                WEAK,
            );
        }
        Step::Turning => {
            // At the top, small enough to keep the middle of the screen - the snapshot the
            // turn is lined up against - clear.
            let size = egui::vec2(760.0 * s, 210.0 * s);
            let rect = egui::Rect::from_center_size(egui::pos2(screen.center().x, screen.top() + size.y * 0.5 + 40.0 * s), size);
            panel(ui, rect);
            // The ring: how much of the 360° has been measured, and an arrow going round it
            // the way to turn.
            let c = egui::pos2(rect.left() + 110.0 * s, rect.center().y);
            let r = 72.0 * s;
            let p = ui.painter();
            p.circle_stroke(c, r, Stroke::new(8.0 * s, Color32::from_white_alpha(28)));
            let frac = (v.theta.abs() / 360.0).min(1.5);
            let sweep = |from: f32, to: f32, width: f32, col: Color32| {
                let n = 64;
                let pts: Vec<egui::Pos2> = (0..=n)
                    .map(|k| {
                        let a = from + (to - from) * k as f32 / n as f32 - std::f32::consts::FRAC_PI_2;
                        c + egui::vec2(a.cos(), a.sin()) * r
                    })
                    .collect();
                p.add(egui::Shape::line(pts, Stroke::new(width, col)));
            };
            if frac > 0.0 {
                let done = if (frac - 1.0).abs() < 0.02 { Color32::from_rgb(120, 220, 140) } else { ACCENT };
                sweep(0.0, frac.min(1.0) * std::f32::consts::TAU, 8.0 * s, done);
            }
            // The arrow: a short arc travelling round, a head at its front.
            let t = (now * 0.6).fract() as f32 * std::f32::consts::TAU;
            let dir = if v.theta < 0.0 { -1.0 } else { 1.0 };
            sweep(t, t + dir * 0.7, 3.0 * s, Color32::from_white_alpha(170));
            let tip = t + dir * 0.7 - std::f32::consts::FRAC_PI_2;
            let at = c + egui::vec2(tip.cos(), tip.sin()) * r;
            let tangent = egui::vec2(-tip.sin(), tip.cos()) * dir;
            let normal = egui::vec2(tip.cos(), tip.sin());
            p.add(egui::Shape::convex_polygon(
                vec![at + tangent * 12.0 * s, at - tangent * 4.0 * s + normal * 8.0 * s, at - tangent * 4.0 * s - normal * 8.0 * s],
                Color32::from_white_alpha(200),
                Stroke::NONE,
            ));
            p.text(c - egui::vec2(0.0, 8.0 * s), Align2::CENTER_CENTER, format!("{:.0}°", v.theta.abs()), crate::settings::ui_font(34.0 * s), TEXT);
            p.text(c + egui::vec2(0.0, 24.0 * s), Align2::CENTER_CENTER, "of 360°", crate::settings::ui_font(14.0 * s), WEAK);
            // What to do, in the order you do it.
            let x = rect.left() + 220.0 * s;
            let w = rect.right() - x - 24.0 * s;
            p.text(egui::pos2(x, rect.top() + 34.0 * s), Align2::LEFT_CENTER, format!("Turn one full 360° · {}", output_name(&v.output, v.jsm)), crate::settings::ui_font(24.0 * s), TEXT);
            let mut rows = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_size(egui::pos2(x, rect.top() + 62.0 * s), egui::vec2(w, 120.0 * s)))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            rows.spacing_mut().item_spacing.y = 8.0 * s;
            if v.flick_wired {
                rows.horizontal(|ui| crate::canvas::viewer::pad_hint_big(ui, "{right_stick} Circle the flick stick for the bulk of the turn", 18.0 * s, TEXT));
            }
            rows.horizontal(|ui| crate::canvas::viewer::pad_hint_big(ui, "Turn the controller to line up exactly with where you started", 18.0 * s, TEXT));
            if v.output == "stick" && v.peak > 1.02 {
                rows.label(egui::RichText::new(format!("Too fast: the stick maxed out ({:.1}×). Cancel and turn slower.", v.peak)).size(crate::settings::ui_size(16.0 * s)).color(WARN));
            }
            rows.horizontal(|ui| crate::canvas::viewer::pad_hint_big(ui, "{btn_south} Finish   {btn_east} Cancel", 18.0 * s, WEAK));
        }
        Step::Result => {
            let size = egui::vec2(640.0 * s, 110.0 * s);
            let rect = egui::Rect::from_center_size(egui::pos2(screen.center().x, screen.top() + size.y * 0.5 + 40.0 * s), size);
            panel(ui, rect);
            let msg = v.result.clone().unwrap_or_else(|| "Calibration cancelled".to_string());
            let col = if msg.starts_with('✓') { Color32::from_rgb(150, 225, 150) } else { WARN };
            ui.painter().text(rect.center() - egui::vec2(0.0, 14.0 * s), Align2::CENTER_CENTER, &title, crate::settings::ui_font(18.0 * s), WEAK);
            ui.painter().text(rect.center() + egui::vec2(0.0, 18.0 * s), Align2::CENTER_CENTER, msg, crate::settings::ui_font(26.0 * s), col);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_snarl::OutPinId;

    fn rws_desc() -> flexinput_core::ModuleDescriptor {
        flexinput_modules::all_modules().into_iter().map(|r| r.descriptor).find(|d| d.id == "processing.rws").expect("registered")
    }

    fn frame(ctx: &egui::Context, time: f64, f: impl FnMut(&egui::Context)) {
        let input = egui::RawInput { time: Some(time), ..Default::default() };
        let _ = ctx.run(input, f);
    }

    // The body's button asks for a pin; the next sync puts it on the config overlay once -
    // asking again doesn't add a second - and says so, per node, which is what the button
    // reads to say the widget is already there. A second module gets its own, below.
    #[test]
    fn a_pin_request_pins_the_widget_once_and_says_so() {
        let ctx = egui::Context::default();
        let mut tabs = vec![PatchTab::new_untitled(1)];
        let a = tabs[0].canvas.snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&rws_desc()));
        let b = tabs[0].canvas.snarl.insert_node(egui::pos2(300.0, 0.0), NodeData::from(&rws_desc()));
        frame(&ctx, 0.0, |ctx| {
            request_pin(ctx, vec![], a.0);
            request_pin(ctx, vec![], a.0);
            sync(ctx, &mut tabs, 0);
        });
        assert!(is_pinned(&ctx, &[], a.0));
        assert!(!is_pinned(&ctx, &[], b.0), "only the module that asked");
        frame(&ctx, 0.1, |ctx| {
            request_pin(ctx, vec![], a.0);
            request_pin(ctx, vec![], b.0);
            sync(ctx, &mut tabs, 0);
        });
        let pins: Vec<&ExposedModule> = tabs[0]
            .config
            .items
            .iter()
            .filter_map(|it| match it {
                LayoutItem::Module(m) if m.element_id == ELEMENT => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(pins.len(), 2, "one widget per module");
        assert!(is_pinned(&ctx, &[], b.0));
        assert!(pins[1].pos[1] > pins[0].pos[1], "the second stacks below the first");
        assert_eq!(pins[0].pin_style, Some(2));
        // Unpinned in the layout editor: the button offers it again.
        tabs[0].config.items.retain(|it| !matches!(it, LayoutItem::Module(m) if m.inner_node_id == a.0));
        frame(&ctx, 0.2, |ctx| sync(ctx, &mut tabs, 0));
        assert!(!is_pinned(&ctx, &[], a.0));
    }

    // The widget and the prompts name the pad the module reads: whatever drives its
    // Rotation input.
    #[test]
    fn the_pad_is_whatever_drives_the_rotation_input() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let rws = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&rws_desc()));
        assert_eq!(pad_name(&snarl, rws), "", "nothing wired");
        let gyro = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor).find(|d| d.id == "processing.gyro_3dof").expect("registered");
        let mut src = NodeData::from(&gyro);
        src.display_name = "DualSense".into();
        let src = snarl.insert_node(egui::pos2(-300.0, 0.0), src);
        snarl.connect(OutPinId { node: src, output: 0 }, InPinId { node: rws, input: 0 });
        assert_eq!(pad_name(&snarl, rws), "DualSense");
    }

    // Each step draws, at a small screen and a large one, without panicking.
    #[test]
    fn every_step_draws() {
        for step in [Step::Choose, Step::Turning, Step::Result] {
            for (output, flick, jsm) in [("mouse", true, false), ("stick", false, false), ("gyro", true, false), ("mouse", true, true), ("stick", true, true)] {
                let v = View {
                    step,
                    jsm,
                    pad: "DualSense".into(),
                    output: output.into(),
                    theta: 212.0,
                    peak: 1.4,
                    flick_wired: flick,
                    result: Some("✓ Gyro scale 1.000 → 0.850".into()),
                };
                for screen in [egui::vec2(1280.0, 720.0), egui::vec2(3840.0, 2160.0)] {
                    let ctx = egui::Context::default();
                    frame(&ctx, 0.0, |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| paint(ui, egui::Rect::from_min_size(egui::Pos2::ZERO, screen), &v, 0.5));
                    });
                }
            }
        }
    }

    // Pictures of the steps, RWS Aim's and JSM's, for looking at.
    #[test]
    #[ignore = "writes pictures, asserts nothing"]
    fn shoot_steps() {
        for (name, step, jsm, output) in [
            ("choose_rws", Step::Choose, false, "mouse"),
            ("choose_jsm", Step::Choose, true, "mouse"),
            ("turning_rws", Step::Turning, false, "gyro"),
            ("result_jsm", Step::Result, true, "mouse"),
        ] {
            let v = View {
                step,
                jsm,
                pad: "DualSense".into(),
                output: output.into(),
                theta: 212.0,
                peak: 0.6,
                flick_wired: true,
                result: Some("✓ RWC 12 → 14".into()),
            };
            let ctx = egui::Context::default();
            crate::app::fonts::setup_fonts(&ctx);
            let mut shot = crate::canvas::shot::Shot::default();
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));
            let mut shapes = Vec::new();
            for _ in 0..3 {
                let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
                let out = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| paint(ui, screen, &v, 0.5));
                });
                shot.absorb(&out.textures_delta);
                shapes = out.shapes;
            }
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/module_shots").join(format!("rws_cal_{name}.png"));
            shot.save(&ctx, &shapes, screen, 1.0, &path).expect("the picture saves");
            println!("{}", path.display());
        }
    }

    // The result stays up for a few seconds after the sweep ends, then the session is over.
    #[test]
    fn the_result_closes_the_session_after_a_few_seconds() {
        let ctx = egui::Context::default();
        let mut tabs = vec![PatchTab::new_untitled(1)];
        let a = tabs[0].canvas.snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&rws_desc()));
        let session = |tabs: &[PatchTab]| {
            tabs[0].canvas.snarl.get_node(a).and_then(|n| n.params.get("cal_session").and_then(|v| v.as_str()).map(str::to_string))
        };
        frame(&ctx, 0.0, |ctx| {
            request_pin(ctx, vec![], a.0);
            sync(ctx, &mut tabs, 0);
        });
        if let Some(n) = tabs[0].canvas.snarl.get_node_mut(a) {
            n.params.insert("cal_session".into(), Value::String("guided".into()));
            n.params.insert("cal_measure".into(), Value::String("off".into()));
        }
        frame(&ctx, 1.0, |ctx| show(ctx, &mut tabs, 0, 60));
        frame(&ctx, 1.0 + RESULT_S * 0.5, |ctx| show(ctx, &mut tabs, 0, 60));
        assert_eq!(session(&tabs).as_deref(), Some("guided"), "still showing the result");
        frame(&ctx, 1.0 + RESULT_S + 0.5, |ctx| show(ctx, &mut tabs, 0, 60));
        assert_eq!(session(&tabs).as_deref(), Some("off"), "the session is over");
    }
}
