//! The second look a pinned row can wear: the module's own, as redesigned - value pills,
//! joined segments, switches drawn as switches - laid out across the pin's frame.
//!
//! A pinned element is drawn in the look it was made with unless it says otherwise
//! (`ExposedModule::pin_style`): a layout built before the redesign was built round the old
//! rows' proportions, and an overlay styled to sit over one game may suit either. So each row
//! that was redesigned has both, and the layout editor's inspector switches between them for
//! the pins that have a choice ([`supported`]). Elements that look the same either way - the
//! graphs, the whole-module pins - have no switch.

use super::*;

/// Whether `element` of `module_id` has a Style 2 look.
pub(crate) fn supported(module_id: &str, element: &str) -> bool {
    matches!(
        (module_id, element),
        (
            "module.response_curve" | "module.vec_response_curve" | "module.twoway_response_curve",
            "scale_row" | "range_row" | "grid_row"
        ) | ("display.oscilloscope" | "display.trigscope", "controls" | "grid_row")
            | ("processing.rws", "style" | "flick" | "flick_opts" | "flick_speed" | "suppress")
    )
}

/// How wide one band of `element` is at 1×, before it has been drawn once and said so
/// itself (see [`fit`]).
fn first_guess(element: &str) -> f32 {
    match element {
        "controls" => 650.0,
        "grid_row" => 380.0,
        "range_row" => 320.0,
        "style" => 400.0,
        "flick" | "flick_opts" => 430.0,
        _ => 220.0,
    }
}

/// Draw a Style 2 row inside `container` - exactly inside it, whatever its shape.
///
/// The row is one band at the largest scale that fits both ways: the frame's height over a
/// band's, and its width over the width one band of the row needs at 1×. That width comes
/// from the row's own constants (`curve_rows::with_zoom` collects it as the row lays itself
/// out), so it is the same at every scale and the scale is a function of the frame alone:
/// nothing drawn is measured and fed back. The band sits in the middle of a frame taller
/// than it, and the frame is allocated whole, so the pin never reports a size of its own.
pub(crate) fn fit(
    ui: &mut egui::Ui,
    inner_id: NodeId,
    element: &str,
    container: egui::Vec2,
    draw: impl FnOnce(&mut egui::Ui),
) {
    let container = egui::vec2(container.x.max(1.0), container.y.max(1.0));
    let key = egui::Id::new(("pin_style2_one_band", ui.ctx().viewport_id(), inner_id.0, element));
    let one: f32 = ui.ctx().data(|d| d.get_temp(key)).unwrap_or_else(|| first_guess(element)).max(1.0);
    let base = super::curve_rows::band_h(ui).max(1.0);
    let scale = (container.y / base).min(container.x / one).max(0.2).min(4.0);
    let (rect, _) = ui.allocate_exact_size(container, egui::Sense::hover());
    let h = (base * scale).min(rect.height());
    let row = egui::Rect::from_min_size(egui::pos2(rect.left(), rect.center().y - h * 0.5), egui::vec2(rect.width(), h));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::top_down(egui::Align::Min))
            .id_salt(("pin_style2", inner_id.0, element)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    super::scale::scale_ui_style(&mut child, scale);
    child.spacing_mut().item_spacing.y = 0.0;
    let ((), now) = super::curve_rows::with_zoom(scale, || draw(&mut child));
    if now > 0.0 && (now - one).abs() > 0.5 {
        ui.ctx().data_mut(|d| d.insert_temp(key, now));
    }
}

/// Draw `element` in its Style 2 look into `container`. Only called for [`supported`] ones.
pub(crate) fn render(
    inner_id: NodeId,
    module_id: &str,
    element: &str,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    fit(ui, inner_id, element, container, |ui| draw(inner_id, module_id, element, ui, snarl, container));
}

fn draw(
    inner_id: NodeId,
    module_id: &str,
    element: &str,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    match module_id {
        "display.oscilloscope" | "display.trigscope" => super::waveform::render_scope_rows_style2(
            module_id == "display.trigscope",
            inner_id,
            ui,
            snarl,
            container,
            element == "controls",
        ),
        "processing.rws" => super::rws::render_rws_row_style2(inner_id, element, ui, snarl, container),
        _ => super::curve_rows::render_curve_row_style2(inner_id, module_id, element, ui, snarl, container),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A layout saved before the field existed has no `pin_style`, and reads as Style 1 - the
    // look it was laid out for. A new pin is made as Style 2 and says so in the file.
    #[test]
    fn an_old_pin_keeps_its_look_and_a_new_one_says_which_it_has() {
        let old = r#"{"inner_node_id":3,"element_id":"scale_row","pos":[0,0],"size":[200,24]}"#;
        let m: crate::canvas::node::ExposedModule = serde_json::from_str(old).expect("an old pin loads");
        assert_eq!(m.pin_style, None);
        assert_eq!(m.pin_style.unwrap_or(1), 1);
        let mut new = m.clone();
        new.pin_style = Some(2);
        let text = serde_json::to_string(&new).expect("it saves");
        assert!(text.contains("\"pin_style\":2"), "{text}");
        assert!(!serde_json::to_string(&m).unwrap().contains("pin_style"), "an unset style is left out of the file");
    }

    // Every Style 2 row draws, at a roomy frame and a cramped one, without panicking, and
    // says where its fields are - as many as the gamepad walks for that element - so the
    // config overlay's focus glow has something to land on.
    #[test]
    fn every_second_look_draws_and_says_where_its_fields_are() {
        let pairs = [
            ("module.response_curve", "scale_row", 3),
            ("module.response_curve", "range_row", 4),
            ("module.response_curve", "grid_row", 3),
            ("module.vec_response_curve", "scale_row", 2),
            ("module.vec_response_curve", "range_row", 2),
            ("module.twoway_response_curve", "range_row", 4),
            ("display.oscilloscope", "controls", 6),
            ("display.oscilloscope", "grid_row", 6),
            ("display.trigscope", "controls", 5),
            ("processing.rws", "style", 4),
            ("processing.rws", "flick", 4),
            ("processing.rws", "flick_opts", 3),
            ("processing.rws", "flick_speed", 2),
            ("processing.rws", "suppress", 1),
        ];
        for (module, element, count) in pairs {
            assert!(supported(module, element), "{module}/{element}");
            let desc = flexinput_modules::all_modules()
                .into_iter()
                .map(|r| r.descriptor)
                .find(|d| d.id == module)
                .expect("registered");
            for container in [egui::vec2(420.0, 30.0), egui::vec2(90.0, 18.0)] {
                let mut snarl: Snarl<NodeData> = Snarl::new();
                let id = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&desc));
                let ctx = egui::Context::default();
                for _ in 0..3 {
                    let _ = ctx.run(egui::RawInput::default(), |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("gp_nav_cur_element", id.0)), element.to_string()));
                            render(id, module, element, ui, &mut snarl, container);
                        });
                    });
                }
                let rects: Option<(u64, Vec<egui::Rect>)> =
                    ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_field_rects", id.0, element.to_string()))));
                let rects = rects.map(|(_, r)| r).unwrap_or_default();
                assert_eq!(rects.len(), count, "{module}/{element} at {container:?}: {rects:?}");
                assert!(rects.iter().any(|r| r.is_positive()), "{module}/{element}: no field drawn");
            }
        }
    }

    // The bug that blew a sub-patch layout up: a Style 2 row's size has to come from its
    // frame and nothing else. At every shape of frame - tiny, a sliver, tall, huge - and
    // through a resize there and back, everything the row draws and every field it
    // publishes stays inside the frame, the frame is all it allocates, and a frame that
    // holds still gets the same drawing every pass.
    #[test]
    fn a_second_look_stays_inside_its_frame_at_any_size() {
        let pairs = [
            ("module.response_curve", "scale_row"),
            ("module.response_curve", "range_row"),
            ("module.response_curve", "grid_row"),
            ("module.vec_response_curve", "range_row"),
            ("module.twoway_response_curve", "scale_row"),
            ("display.oscilloscope", "controls"),
            ("display.oscilloscope", "grid_row"),
            ("display.trigscope", "controls"),
            ("processing.rws", "style"),
            ("processing.rws", "flick"),
            ("processing.rws", "flick_opts"),
            ("processing.rws", "flick_speed"),
            ("processing.rws", "suppress"),
        ];
        let sizes = [
            egui::vec2(420.0, 30.0),
            egui::vec2(90.0, 18.0),
            egui::vec2(40.0, 200.0),
            egui::vec2(1600.0, 24.0),
            egui::vec2(900.0, 260.0),
            egui::vec2(260.0, 90.0),
            egui::vec2(12.0, 6.0),
            egui::vec2(420.0, 30.0),
        ];
        for (module, element) in pairs {
            let desc = flexinput_modules::all_modules()
                .into_iter()
                .map(|r| r.descriptor)
                .find(|d| d.id == module)
                .expect("registered");
            let mut snarl: Snarl<NodeData> = Snarl::new();
            let id = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&desc));
            let ctx = egui::Context::default();
            let origin = egui::pos2(30.0, 40.0);
            for container in sizes {
                let frame = egui::Rect::from_min_size(origin, container);
                let mut last: Option<(egui::Rect, Vec<egui::Rect>)> = None;
                for pass in 0..6 {
                    let mut drawn = egui::Rect::NOTHING;
                    let _ = ctx.run(egui::RawInput::default(), |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            let mut pin = ui.new_child(egui::UiBuilder::new().max_rect(frame));
                            pin.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("gp_nav_cur_element", id.0)), element.to_string()));
                            render(id, module, element, &mut pin, &mut snarl, container);
                            drawn = pin.min_rect();
                        });
                    });
                    let rects: Option<(u64, Vec<egui::Rect>)> =
                        ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_field_rects", id.0, element.to_string()))));
                    let rects = rects.map(|(_, r)| r).unwrap_or_default();
                    let tol = frame.expand(0.6);
                    assert!(
                        (drawn.width() - container.x).abs() < 0.6 && (drawn.height() - container.y).abs() < 0.6,
                        "{module}/{element} in {container:?}: the pin took {drawn:?}"
                    );
                    if container.x >= 60.0 && container.y >= 14.0 {
                        for r in rects.iter().filter(|r| r.is_positive()) {
                            assert!(tol.contains_rect(*r), "{module}/{element} in {container:?}: a field at {r:?} is outside {frame:?}");
                        }
                    }
                    if pass >= 3 {
                        if let Some(prev) = &last {
                            assert_eq!(prev, &(drawn, rects.clone()), "{module}/{element} in {container:?}: still moving on pass {pass}");
                        }
                    }
                    last = Some((drawn, rects));
                }
            }
        }
    }

    // Through the real pinned path: a Style 2 row leaves no measured size behind, so the
    // layout editor's resize clamp (`clamp_pin_frame_to_content`) has nothing to hold the
    // frame to - the measurement it used to find was the runaway one - and a measurement
    // left from when the pin wore Style 1 is dropped.
    #[test]
    fn a_second_look_leaves_no_measurement_for_the_resize_to_chase() {
        let desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == "module.response_curve")
            .expect("registered");
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let id = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&desc));
        let outer = NodeId(77);
        let ctx = egui::Context::default();
        let live = std::collections::HashMap::new();
        let panic = crate::app::PanicShortcut::default();
        let mut key = None;
        for style in [1u8, 1, 1, 2, 2, 2] {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let frame = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(296.0, 38.0));
                    let mut pin = ui.new_child(egui::UiBuilder::new().max_rect(frame));
                    key = Some(super::super::scale::pin_ws_nat_key(pin.ctx(), outer.0, id.0, "scale_row"));
                    super::super::pinned::render_pinned_element(
                        id, "module.response_curve", "scale_row", &mut pin, &mut snarl, frame.size(),
                        &live, &panic, None, None, outer, true, None, None, None, style,
                    );
                    if style == 2 {
                        assert!(
                            (pin.min_rect().width() - 296.0).abs() < 0.6,
                            "the Style 2 pin took {:?}", pin.min_rect()
                        );
                    }
                });
            });
            let nat: Option<egui::Vec2> = ctx.data(|d| d.get_temp(key.expect("rendered")));
            if style == 1 {
                assert!(nat.is_some(), "Style 1 still measures itself");
            } else {
                assert!(nat.is_none(), "a Style 2 pin left a measurement: {nat:?}");
            }
        }
    }

    // The frame the feedback loop saved, from a real workspace: it comes back a usable size
    // on load, and frames that never ran away are left exactly as they were.
    #[test]
    fn a_runaway_frame_comes_back_and_a_sane_one_is_left_alone() {
        use crate::canvas::node::{repair_runaway_items, ExposedModule, LayoutItem};
        let pin = |size: [f32; 2], pos: [f32; 2]| {
            let mut m: ExposedModule = serde_json::from_str(
                r#"{"inner_node_id":3,"element_id":"scale_row","pos":[0,0],"size":[1,1]}"#,
            )
            .expect("a pin");
            m.size = size;
            m.pos = pos;
            LayoutItem::Module(m)
        };
        let mut items = vec![
            pin([9_699_328.0, 37.84375], [8.0, 280.0]),
            pin([296.0, 18.0], [328.0, 632.0]),
            pin([f32::NAN, f32::INFINITY], [-5.0, 1.0e9]),
        ];
        assert!(repair_runaway_items(&mut items));
        let rect = |i: usize| match &items[i] {
            LayoutItem::Module(m) => (m.size, m.pos),
            _ => unreachable!(),
        };
        let (size, pos) = rect(0);
        assert!((size[0] - 378.4375).abs() < 0.01 && size[1] == 37.84375 && pos == [8.0, 280.0], "{size:?} {pos:?}");
        assert_eq!(rect(1), ([296.0, 18.0], [328.0, 632.0]), "a sane frame is not touched");
        let (size, pos) = rect(2);
        assert_eq!((size, pos), ([400.0, 40.0], [0.0, 0.0]));
        assert!(!repair_runaway_items(&mut items), "and a repaired layout stays repaired");
    }

    // Pictures of the Style 2 rows in frames of several shapes, each frame outlined, for
    // looking at what the numbers above can't say. One sheet per row.
    #[test]
    #[ignore = "writes pictures, asserts nothing"]
    fn shoot_second_looks() {
        let pairs = [
            ("module.response_curve", "scale_row"),
            ("module.response_curve", "range_row"),
            ("module.response_curve", "grid_row"),
            ("display.oscilloscope", "controls"),
            ("processing.rws", "flick"),
        ];
        let sizes = [
            egui::vec2(296.0, 18.0),
            egui::vec2(296.0, 38.0),
            egui::vec2(600.0, 30.0),
            egui::vec2(160.0, 60.0),
            egui::vec2(900.0, 90.0),
        ];
        for (module, element) in pairs {
            let desc = flexinput_modules::all_modules()
                .into_iter()
                .map(|r| r.descriptor)
                .find(|d| d.id == module)
                .expect("registered");
            let mut snarl: Snarl<NodeData> = Snarl::new();
            let id = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&desc));
            let ctx = egui::Context::default();
            crate::app::fonts::setup_fonts(&ctx);
            let mut shot = crate::canvas::shot::Shot::default();
            let mut shapes = Vec::new();
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 360.0));
            for _ in 0..4 {
                let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
                let out = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let mut y = 16.0;
                        for (k, size) in sizes.iter().enumerate() {
                            let frame = egui::Rect::from_min_size(egui::pos2(16.0, y), *size);
                            ui.painter().rect_stroke(frame.expand(1.0), 0.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(255, 200, 80)), egui::StrokeKind::Outside);
                            let mut pin = ui.new_child(egui::UiBuilder::new().max_rect(frame).id_salt(k));
                            render(id, module, element, &mut pin, &mut snarl, *size);
                            y += size.y + 16.0;
                        }
                    });
                });
                shot.absorb(&out.textures_delta);
                shapes = out.shapes;
            }
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/module_shots")
                .join(format!("style2_{}_{element}.png", module.replace('.', "_")));
            shot.save(&ctx, &shapes, screen, 1.0, &path).expect("the picture saves");
            println!("{}", path.display());
        }
    }

    #[test]
    fn the_redesigned_rows_have_a_second_look_and_the_graphs_dont() {
        assert!(supported("module.response_curve", "scale_row"));
        assert!(supported("display.trigscope", "grid_row"));
        assert!(supported("processing.rws", "flick"));
        assert!(!supported("module.response_curve", "curve"), "a graph looks the same either way");
        assert!(!supported("module.remapper", "whole_module"));
    }
}
