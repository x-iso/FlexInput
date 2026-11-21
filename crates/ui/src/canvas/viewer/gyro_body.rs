//! Gyro 3DOF node body + lean mapping section.

use super::*;

/// Resolve the current (family, axis) tuple from a node, applying legacy
/// `mode` fallback for patches saved before the split.
pub(crate) fn gyro_read_family_axis(n: &NodeData) -> (String, String) {
    if let Some(fam) = n.params.get("family").and_then(|v| v.as_str()) {
        let axis = n.params.get("axis").and_then(|v| v.as_str()).unwrap_or("pitch_yaw");
        return (fam.to_string(), axis.to_string());
    }
    match n.params.get("mode").and_then(|v| v.as_str()).unwrap_or("local") {
        "player" => ("pointer".into(),  "player".into()),
        "world"  => ("pointer".into(),  "world".into()),
        "laser"  => ("steering".into(), "pitch_yaw".into()),
        _        => ("pointer".into(),  "pitch_yaw".into()),
    }
}

/// The drawing on each mode's cell, in the order of `GYRO_AXIS_OPTIONS`: the turn Pitch+Yaw
/// and Pitch+Roll take X from, a figure for Player (turns about your own up), a globe for
/// World (turns about the world's), a compass for Orient (where the nose points).
const GYRO_AXIS_ICONS: [&[u8]; 5] = [
    include_bytes!("../../../../../app/assets/Motion/gyro_yaw.svg"),
    include_bytes!("../../../../../app/assets/Motion/gyro_roll.svg"),
    include_bytes!("../../../../../app/assets/general/UI/human-target.svg"),
    include_bytes!("../../../../../app/assets/general/UI/wireframe-globe.svg"),
    include_bytes!("../../../../../app/assets/general/UI/compass.svg"),
];
/// The drawing of axis mode `axis` (`pitch_yaw`, `player`, ...), for a row that shows the
/// modes elsewhere (a pinned Style 2 row).
pub(crate) fn gyro_axis_icon(axis: &str) -> Option<&'static [u8]> {
    GYRO_AXIS_OPTIONS.iter().position(|(id, _)| *id == axis).map(|i| GYRO_AXIS_ICONS[i])
}

/// Beside the two rows' captions: an aim, and a wheel.
const POINTER_ICON: &[u8] = include_bytes!("../../../../../app/assets/general/UI/crosshair.svg");
const STEERING_ICON: &[u8] = include_bytes!("../../../../../app/assets/general/UI/steering-wheel.svg");

pub(crate) const GYRO_AXIS_OPTIONS: [(&str, &str); 5] = [
    ("pitch_yaw",  "Pitch+Yaw"),
    ("pitch_roll", "Pitch+Roll"),
    ("player",     "Player"),
    ("world",      "World"),
    ("orient",     "Orient"),
];

pub(crate) fn show_gyro_3dof_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    body_w: f32,
    body_h: f32,
) {
    let _ = inputs; // wire-presence + upstream device id read below
    let snap = snarl.get_node(node_id);
    let (family, axis)       = snap.map(gyro_read_family_axis).unwrap_or_else(|| ("pointer".into(), "pitch_yaw".into()));
    let inv_yaw   = snap.and_then(|n| n.params.get("inv_yaw")   .and_then(|v| v.as_bool())).unwrap_or(false);
    let inv_pitch = snap.and_then(|n| n.params.get("inv_pitch")  .and_then(|v| v.as_bool())).unwrap_or(false);
    let inv_roll  = snap.and_then(|n| n.params.get("inv_roll")   .and_then(|v| v.as_bool())).unwrap_or(false);
    let inv_ax    = snap.and_then(|n| n.params.get("inv_accel_x").and_then(|v| v.as_bool())).unwrap_or(false);
    let inv_ay    = snap.and_then(|n| n.params.get("inv_accel_y").and_then(|v| v.as_bool())).unwrap_or(false);
    let inv_az    = snap.and_then(|n| n.params.get("inv_accel_z").and_then(|v| v.as_bool())).unwrap_or(false);
    let exclude_y = snap.and_then(|n| n.params.get("steering_exclude_y").and_then(|v| v.as_bool())).unwrap_or(false);
    let recenter_strength = snap.and_then(|n| n.params.get("recenter_strength").and_then(|v| v.as_f64())).unwrap_or(0.0) as f32;
    let reset_ease_in     = snap.and_then(|n| n.params.get("reset_ease_in").and_then(|v| v.as_f64())).unwrap_or(0.25) as f32;
    let lean_threshold    = snap.and_then(|n| n.params.get("lean_threshold").and_then(|v| v.as_f64())).unwrap_or(0.3) as f32;
    // Display scale for the 3D Orientation output only (applied to the
    // integration rate so it stays continuous). Does NOT affect the 2D outputs.
    let orient_scale      = snap.and_then(|n| n.params.get("orient_scale").and_then(|v| v.as_f64())).unwrap_or(1.0) as f32;
    // Edge turn's settings, for the preview to run it as the module does (Pointer only).
    let edge_params = snap.filter(|_| family == "pointer").and_then(gyro_edge_params);
    let model_alpha = snap.map_or(GYRO_MODEL_ALPHA, gyro_model_alpha);
    let orient_drift      = snap.and_then(|n| n.params.get("orient_drift").and_then(|v| v.as_f64())).unwrap_or(0.0) as f32;
    let yaw_recenter      = snap.and_then(|n| n.params.get("orient_auto_recenter").and_then(|v| v.as_bool())).unwrap_or(false);
    let yaw_thresh        = snap.and_then(|n| n.params.get("orient_recenter_thresh").and_then(|v| v.as_f64())).unwrap_or(0.005) as f32;
    // Where the 3D Orientation output (and Edge turn) takes its pose from:
    // "auto" (default), "computed" (integrated here) or "device" (the pad's own
    // fused orientation, e.g. a Joy-Con 2's).
    let orient_source     = snap.map(gyro_read_orient_source).unwrap_or("auto");
    let edge = snap.map(GyroEdge::read).unwrap_or_default();
    let roll_mix          = snap.and_then(|n| n.params.get("roll_contribution").and_then(|v| v.as_f64())).unwrap_or(0.0) as f32;
    let hold = snap.map(gyro_read_hold).unwrap_or([0.0; 3]);
    let lean_v = snap.and_then(|n| match n.extra.last_out.get(3) { Some(Some(Signal::Float(f))) => Some(*f), _ => None }).unwrap_or(0.0);
    // The pose the module publishes on its Orientation output: the same quaternion the 3D
    // viewer draws a pad with, which is the point of the viewport - the picture and a pinned
    // copy of the pad are looking at one thing.
    let pose = snap.and_then(|n| match n.extra.last_out.get(5) {
        Some(Some(Signal::Vec4(v))) => Some(*v),
        _ => None,
    })
    .filter(|v| v.length_squared() > 1e-6)
    .map(|v| glam::Quat::from_xyzw(v.x, v.y, v.z, v.w).normalize());
    let aim = snap.and_then(gyro_aim);
    let view_pose = snap.and_then(gyro_view_pose);
    // Whether the viewport wears the pad model instead of its axes. The switch lives in the
    // header, because it says how the module's one display is drawn.
    let look = snap.map(gyro_preview_look).unwrap_or("icon");

    let mut family = family;
    let mut axis = axis;
    let mut inv_gyro  = [inv_yaw, inv_pitch, inv_roll];
    let mut inv_accel = [inv_ax, inv_ay, inv_az];
    let mut exclude_y = exclude_y;
    let mut recenter_strength = recenter_strength;
    let mut reset_ease_in = reset_ease_in;
    let mut lean_threshold = lean_threshold;
    let mut orient_scale = orient_scale;
    let mut orient_drift = orient_drift;
    let mut yaw_recenter = yaw_recenter;
    let mut yaw_thresh   = yaw_thresh;
    let mut orient_source = orient_source;
    let mut edge         = edge;
    let mut roll_mix     = roll_mix;
    let mut hold         = hold;
    let mut changed   = false;

    const GYR_LABELS: [(&str, &str); 3] = [
        ("yaw",   "gyro_z — invert if rotating right gives negative X\n(expected: right = positive X)"),
        ("pitch", "gyro_y — invert if tilting up gives negative Y\n(expected: up = positive Y)"),
        ("roll",  "gyro_x — affects Lean output and Player/World gravity correction"),
    ];
    const ACC_LABELS: [(&str, &str); 3] = [
        ("X",  "accel_x — invert if Player/World horizontal correction is backwards"),
        ("Y",  "accel_y — invert if Player/World vertical correction is backwards"),
        ("+Z", "accel_z — expected POSITIVE when controller is held flat face-up (≈ +1 G).\nInvert if your device reports negative when flat."),
    ];
    /// What each axis family does, in the order of `GYRO_AXIS_OPTIONS`: the hover text of
    /// the cell that chooses it, and the same list the viewport's arrows are drawn from.
    const GYRO_AXIS_TIPS: [&str; 5] = [
        "Yaw turns X and pitch turns Y — the plain gyro aim.",
        "Roll turns X and pitch turns Y — for a grip that steers by rolling.",
        "JoyShockMapper's PLAYER_TURN: turning about gravity is X, however the pad\nis held (yaw and roll both count, up to how fast the pad is turning), and\nY is the pad's own pitch. Forgiving of how you hold it.",
        "JoyShockMapper's WORLD_TURN: turning about gravity is X, and Y is pitch\nmeasured level with the world, faded out as the pad goes onto its side.",
        "Follows where the pad's nose points: X is how fast its heading changes,\nY its elevation, so adding them up lands where it points whatever path it\ntook. Twisting the pad about its nose does nothing. Above 70° up or\ndown the heading stops speeding up (straight up it has none).",
    ];
    /// The same choices in the Steering row, which adds the turns up into a wheel's angle.
    /// JoyShockMapper has nothing like it (its lean steering, LEFT_STEER_X, is a tilt read off
    /// gravity, not a turn added up); what each reads as turning is the Pointer row's.
    const GYRO_STEER_TIPS: [&str; 5] = [
        "The wheel's angle is the pad's yaw, added up: hold it upright like a\nwheel and turn it in its own face. Upright, gravity can see the wheel,\nso ctr keeps it centred where Reset put it.",
        "The wheel's angle is the pad's roll, added up: hold it flat and roll it.\nFlat, gravity can see the wheel, so ctr keeps it centred where Reset\nput it.",
        "The wheel's angle is the turn about gravity (as Pointer's Player reads it),\nadded up: turn the pad about the vertical however it is held. Gravity\ncan't see a turn about itself, so only Reset centres it.",
        "The wheel's angle is the turn about gravity (as Pointer's World reads it),\nadded up, and Y the pitch measured level. Gravity can't see a turn about\nitself, so only Reset centres it.",
        "X and Y are where the pad's nose points from where Reset put it, as a\nheading and an elevation. Gravity can't see a heading, so only Reset\ncentres X.",
    ];

    // The neutral hold as a rotation, for the viewport's ghost pad: the same three angles
    // the module applies, in the engine's order — yaw about down, pitch about right, roll
    // about forward.
    let hold_q = glam::Quat::from_rotation_z(hold[1].to_radians())
        * glam::Quat::from_rotation_y(hold[0].to_radians())
        * glam::Quat::from_rotation_x(hold[2].to_radians());
    let axis_id: &'static str = match axis.as_str() {
        "pitch_roll" => "pitch_roll",
        "player" => "player",
        "world" => "world",
        "orient" => "orient",
        _ => "pitch_yaw",
    };

    // The model the viewport wears when the header says so: the same trace the 3D viewer
    // makes, up the Device wire and out through a sub-patch inlet if it has to, so the two
    // modules pick the same mesh for the same pad.
    let traced = snarl
        .in_pin(InPinId { node: node_id, input: 0 })
        .remotes
        .first()
        .copied()
        .map(|src| controller3d_physical_devices(snarl, src, automap_parent))
        .unwrap_or_default();
    let (devs, _) = controller3d_devices_and_deadzone(traced);
    // A pad the trace can't name still gets a model, as the 3D viewer's Auto does: the first
    // one there is.
    let model = if look == "model" {
        Some(crate::model::model_for_devices(&devs).unwrap_or_else(crate::model::default_model))
    } else {
        None
    };
    let pad_svg = gyro_pad_svg(&devs);

    let mut pointer_rect: Option<egui::Rect> = None;
    let mut steering_rect: Option<egui::Rect> = None;
    let mut stopts_rect:  Option<egui::Rect> = None;
    let mut roll_mix_rect: Option<egui::Rect> = None;
    let mut hold_rect:    Option<egui::Rect> = None;
    let mut edge_rect:    Option<egui::Rect> = None;
    let mut source_rect:  Option<egui::Rect> = None;
    let mut gyr_rect:     Option<egui::Rect> = None;
    let mut acc_rect:     Option<egui::Rect> = None;
    let mut lean_rect:    Option<egui::Rect> = None;
    let mut lean_left_rect:  Option<egui::Rect> = None;
    let mut lean_right_rect: Option<egui::Rect> = None;
    // Inside a whole-module widget, each row's fields as drawn - `Rect::NOTHING` for one
    // greyed out, which the pad passes by - for the pad to walk them one by one
    // (`publish_widget_row_fields`); and the groups of settings that aren't elements of
    // their own to pin but are parts of the widget (`register_widget_part`).
    let mut row_fields: Vec<(&'static str, Vec<egui::Rect>)> = Vec::new();
    let mut widget_parts: Vec<(&'static str, egui::Rect)> = Vec::new();
    let on = |enabled: bool, r: egui::Rect| if enabled { r } else { egui::Rect::NOTHING };

    ui.vertical(|ui| {
        // One surface and the rows under it, the whole of it sized by the module's corner
        // grip: the viewport takes whatever the rows leave over, so a block dragged onto the
        // frame is a block more of pad to look at.
        let mut surface = super::kit::Surface::begin(ui, node_id, body_w, body_h, super::gyro_view::GYRO_VIEW_MIN);
        let w = surface.size.x;
        // Everything after this is drawn inside the width the surface was given. Without the
        // bound, a row that fills `available_width()` — the hairline before the Lean lists,
        // which is the whole width of whatever the frame was last time — keeps a module that
        // the grip narrowed at its old width: the flow measures the space, not the drawing.
        ui.set_max_width(w);
        // What the rows need, never what they drew: they are drawn at the width the surface
        // was given, so reporting that back would hold the module at whatever width the grip
        // last gave it. The widest thing here is the pair of Lean lists, at the fixed width
        // the shared card renderer draws cards at; the mode rows and the Edge block want less.
        surface.need(LEAN_LIST_W);
        let band = super::curve_rows::band_h(ui);
        let ctrl = band - 4.0;

        // Wide enough, the preview goes in a column of its own on the left, as tall as the
        // module, and the settings in a column beside it (the Lean lists, the widest of them,
        // set how narrow it gets), a bar between the two sharing the width out. Wider still,
        // the Lean section takes a column of its own on the right, a second bar between it
        // and the settings. Narrower, it's one column, the preview on top; folded, the
        // preview is a heading across the top, and Lean can still go beside the settings.
        let view_open = snarl
            .get_node(node_id)
            .and_then(|n| n.params.get("fold_view").and_then(|v| v.as_bool()))
            .map_or(true, |folded| !folded);
        let wide = if view_open {
            super::kit::beside(snarl, node_id, w, PREVIEW_MIN_W.max(super::gyro_view::GYRO_VIEW_MIN.x), LEAN_LIST_W, "view_frac", 0.5)
        } else {
            None
        };
        let lean_beside = super::kit::beside(snarl, node_id, wide.map_or(w, |b| b.right_w), SETTINGS_MIN_W, LEAN_LIST_W, "lean_frac", 0.5);
        let top_left = ui.cursor().min;

        // ── The viewport: the pose, and the 2D it becomes ───────────────────
        let view = super::gyro_view::GyroView {
            axes: super::gyro_view::GyroAxes::of(ui.visuals()),
            pose,
            aim,
            view: view_pose,
            hold: hold_q,
            zone: (family == "pointer" && edge.mode != "off").then_some(edge.zone),
            proj: super::gyro_view::GyroProjection {
                axis: axis_id,
                roll_share: roll_mix / 100.0,
                inv: inv_gyro,
                physical: aim_physical(aim),
            },
            model: model.as_deref(),
            pad_svg,
            outline: look == "outline",
            lean: lean_v,
            lean_threshold,
            edge: edge_params.clone(),
            orient_scale,
            frame: None,
            model_alpha,
        };
        // The preview, `size` big, with how the pad is drawn in its corner.
        let draw_preview = |ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, size: egui::Vec2| {
            let (vr, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            super::gyro_view::draw_gyro_view(ui, node_id, vr, &view);
            // Pinnable on its own: a layout or an overlay can carry the picture of what the
            // module is doing.
            register_exposable_element(ui, node_id, "preview", vr);
            super::simple_bodies::trace_rect(ui, node_id, "gyro_view", vr);
            // How the pad is drawn, in the preview's corner: its icon, the icon's outline,
            // or its 3D model.
            {
                // As wide as its longest choice in the control font, plus the arrow: a fixed
                // width spilled out of the preview's corner past 13 points.
                let opts: [(&str, &str); 3] = [("icon", "icon"), ("outline", "outline"), ("model", "3D model")];
                let look_w = super::curve_rows::word_w(ui, "3D model") + 26.0;
                let r = egui::Rect::from_min_size(egui::pos2(vr.right() - look_w - 4.0, vr.top() + 4.0), egui::vec2(look_w, ctrl - 4.0));
                if let Some(pick) = super::curve_rows::pill_combo(ui, r, "", &look.to_string(), &opts.map(|(k, l)| (k.to_string(), l)),
                    "How the pad is drawn: its icon, the icon's outline, or its 3D model (half see-through).",
                    ("gyro_look", node_id))
                {
                    if let Some(n) = snarl.get_node_mut(node_id) {
                        n.params.insert("view_look".into(), Value::String(pick));
                        n.params.remove("view_model");
                    }
                }
                // With the model drawn, how opaque it is, under the choice.
                if look == "model" {
                    let r = r.translate(egui::vec2(0.0, ctrl));
                    let mut a = snarl.get_node(node_id).map_or(GYRO_MODEL_ALPHA, gyro_model_alpha);
                    let shown = format!("{:.0}%", a * 100.0);
                    if super::curve_rows::real_pill(ui, node_id, "gyro_model_alpha", r, "\u{03b1}|opacity", &mut a, &shown, 0.05..=1.0, 0.005,
                        "How opaque the pad's model is over the sphere (its neutral hold's ghost fainter in proportion).")
                    {
                        if let Some(n) = snarl.get_node_mut(node_id) {
                            n.params.insert("view_model_alpha".into(), serde_json::json!(a as f64));
                        }
                    }
                }
            }
        };
        // Folded with the rest of the settings when it isn't wanted: the frame then is the
        // rows alone, and the viewport gets the grip's room back when it is unfolded.
        let parent: &mut egui::Ui = ui;
        let mut view_drawn = false;
        // One column, the preview is on top, as wide as the frame and as tall as the rows
        // leave it, with a bar under it. Beside the rest it's drawn last, once how tall they
        // came out is known.
        if wide.is_none() && super::curve_rows::section(parent, snarl, node_id, "view", "3D preview", true) {
            draw_preview(parent, snarl, surface.size);
            view_drawn = true;
            // The bar under the preview: the preview takes what the frame leaves, so
            // dragging it grows or shrinks the frame and the preview with it.
            let (bar, _) = parent.allocate_exact_size(egui::vec2(surface.size.x, 8.0), egui::Sense::hover());
            let (dy, _) = super::kit::resize_bar(parent, bar, ("gyro_view_bar", node_id), true,
                "Drag to make the preview taller or shorter.");
            if dy != 0.0 {
                if let Some(n) = snarl.get_node_mut(node_id) {
                    super::kit::grow_frame(n, band, dy);
                }
            }
        }

        // The settings and the Lean section after them: in columns of their own beside the
        // preview and each other as far as the width goes, else on down the body's one.
        let rest_x = top_left.x + wide.map_or(0.0, |b| b.left_w + super::kit::BESIDE_BAR_W);
        let rest_top = if wide.is_some() { top_left.y } else { parent.cursor().min.y };
        let column = |parent: &mut egui::Ui, x: f32, y: f32, cw: f32| {
            parent.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cw, 1.0e5)))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            )
        };
        let (settings_w, lean_w) = match (lean_beside, wide) {
            (Some(lb), _) => (lb.left_w, lb.right_w),
            (None, Some(b)) => (b.right_w, b.right_w),
            (None, None) => (w, w),
        };
        let mut settings_col = (wide.is_some() || lean_beside.is_some()).then(|| column(parent, rest_x, rest_top, settings_w));
        let mut lean_col = lean_beside.map(|lb| column(parent, rest_x + lb.left_w + super::kit::BESIDE_BAR_W, rest_top, lb.right_w));
        let (ui, w): (&mut egui::Ui, f32) = match settings_col.as_mut() {
            Some(c) => (c, settings_w),
            None => (&mut *parent, w),
        };
        let steering_on = family == "steering";
        let roll_mix_on = axis_id == "pitch_yaw";
        let pose_rows_on = orient_source != "device";
        // Each category's numbers: groups that share a band while the width holds them and
        // take one each when it doesn't; the widths are what a group needs to read, not what
        // it drew.
        let lay = |ui: &mut egui::Ui, mins: &[f32]| -> Vec<egui::Rect> {
            let (cells, h) = super::curve_rows::bands(w, mins, band, 3.0 * super::kit::GAP);
            let (area, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
            cells.into_iter().map(|c| c.translate(area.min.to_vec2())).collect()
        };

        // The settings in categories, each folded and unfolded by its heading
        // (`curve_rows::section`): what the module does first, then the Orientation output's
        // own settings and the axis inversions - set up once, folded until wanted - then Lean.
        if super::curve_rows::section(ui, snarl, node_id, "mode", "2D Output mode", true) {
        // ── The two mode rows ───────────────────────────────────────────────
        // Four equal cells apiece: the same four choices in both rows, and only the row of
        // the family in use has a cell lit.
        for (caption, want, out) in [("Pointer", "pointer", &mut pointer_rect), ("Steering", "steering", &mut steering_rect)] {
            let (row, _) = ui.allocate_exact_size(egui::vec2(w, band), egui::Sense::hover());
            let tips = if want == "steering" { GYRO_STEER_TIPS } else { GYRO_AXIS_TIPS };
            if let Some(picked) = mode_row(ui, node_id, row, caption, &family, &axis, want, tips) {
                family = want.to_string();
                axis = picked.to_string();
                changed = true;
            }
            *out = Some(row);
            row_fields.push((if want == "steering" { "steering_mode" } else { "pointer_mode" }, vec![row]));
        }

        // Steering's own settings straight under the Steering row they belong to, then the
        // neutral hold and the roll mix.
        let steer_cell = lay(ui, &[STEER_W])[0];
        let cells = lay(ui, &[HOLD_W, ROLL_W]);
        let cell = |i: usize| cells[i];

        // The neutral hold: how the pad sits at rest, which is what everything else is
        // measured from.
        {
            let r = cell(0);
            let each = (r.width() - 2.0 * super::kit::GAP) / 3.0;
            let mut fields = Vec::new();
            for (i, tip) in GYRO_HOLD_AXIS_TIPS.into_iter().enumerate() {
                let pr = egui::Rect::from_min_size(
                    egui::pos2(r.left() + i as f32 * (each + super::kit::GAP), r.center().y - ctrl * 0.5),
                    egui::vec2(each, ctrl),
                );
                fields.push(pr);
                let shown = format!("{:+.0}°", hold[i]);
                changed |= pill_in(
                    ui, true, node_id, &format!("gyro_hold_{i}"), pr,
                    ["pitch", "yaw", "roll"][i], &mut hold[i], &shown, -180.0..=180.0, 0.4, tip,
                );
            }
            hold_rect = Some(r);
            row_fields.push(("hold_offset", fields));
        }

        // Roll mixed into the turn, and the Lean threshold with its live reading beside it.
        {
            let r = cell(1);
            let pr = egui::Rect::from_min_size(egui::pos2(r.left(), r.center().y - ctrl * 0.5), egui::vec2(r.width(), ctrl));
            let shown = format!("{roll_mix:+.0} %");
            changed |= pill_in(ui, roll_mix_on, node_id, "gyro_roll_mix", pr, "mix|roll mix", &mut roll_mix, &shown, -100.0..=100.0, 0.4, GYRO_ROLL_MIX_TIP);
            roll_mix_rect = Some(r);
            row_fields.push(("roll_mix", vec![on(roll_mix_on, pr)]));
        }
        // The steering accumulators: what pulls them toward a heading, and how fast a Reset
        // lets go.
        {
            let r = steer_cell;
            // The switch takes what its word needs - the whole word with room to spare - and
            // the two pills share the rest of the row, so a wide module gives them their names
            // ("recenter", "reset ease"). They stood at the right edge at a fixed 96 and 86
            // points, showing "ctr" however much row was left of them.
            let gap = super::kit::GAP;
            let whole = word_w(ui, "exclude Y");
            let excl_w = if r.width() - whole - 2.0 * gap >= 2.0 * 150.0 { whole } else { word_w(ui, "excl. Y") };
            let pill_w = ((r.width() - excl_w - 2.0 * gap) / 2.0).max(40.0);
            let at = |x: f32, w: f32| egui::Rect::from_min_size(egui::pos2(x, r.center().y - ctrl * 0.5), egui::vec2(w, ctrl));
            let excl = at(r.left(), excl_w);
            let ctr = at(excl.right() + gap, pill_w);
            let ease = at(ctr.right() + gap, pill_w);
            changed |= word_toggle(ui, excl, steering_on, &mut exclude_y, "excl. Y|exclude Y",
                "Suppress the Y steering output (keeps Y at 0).\nUseful when only the X axis matters — e.g. a steering wheel\nwhere pitching the controller shouldn't move Y.",
                ("gyro_excl_y", node_id));
            let shown = format!("{recenter_strength:.2} /s");
            // Gravity can see the wheel in Pitch+Yaw (held upright) and Pitch+Roll (held flat);
            // the other modes turn about gravity itself.
            let centres = matches!(axis.as_str(), "pitch_yaw" | "pitch_roll");
            changed |= pill_in(ui, steering_on && centres, node_id, "gyro_recenter", ctr, "ctr|recenter", &mut recenter_strength, &shown, 0.0..=4.0, 0.02,
                "Keeps the wheel centred where Reset put it, from gravity: how far\ngravity has swung about the wheel's axis since then is how far the\nwheel is turned, and the angle added up is pulled toward it this fast,\nper second. Pitch+Yaw sees it with the pad held upright, Pitch+Roll\nwith it held flat; the other modes turn about gravity itself, which it\ncan't see. 0 is off.");
            let shown = format!("{reset_ease_in:.2} s");
            changed |= pill_in(ui, steering_on, node_id, "gyro_ease", ease, "ease|reset ease", &mut reset_ease_in, &shown, 0.0..=2.0, 0.01,
                "Reset eases steering toward 0 over this many seconds.");
            stopts_rect = Some(r);
            row_fields.push(("steering_opts", vec![on(steering_on, excl), on(steering_on && centres, ctr), on(steering_on, ease)]));
        }

        // ── Edge turn ───────────────────────────────────────────────────────
        // Pointer only: past the zone drawn on the viewport, the camera keeps turning. A
        // rule above it, in the section headings' colour: it is a block of its own, not one
        // more row of the mode's settings.
        if family == "pointer" {
            let (rule, _) = ui.allocate_exact_size(egui::vec2(w, 9.0), egui::Sense::hover());
            ui.painter().line_segment(
                [egui::pos2(rule.left(), rule.center().y), egui::pos2(rule.right(), rule.center().y)],
                egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
            let block = ui.scope(|ui| {
                let mut fr = [egui::Rect::NOTHING; 9];
                if edge.ui_rects(ui, node_id, w, true, &mut fr) {
                    changed = true;
                }
                fr
            });
            edge_rect = Some(block.response.rect);
            row_fields.push(("edge_turn", block.inner.to_vec()));
        }

        }

        if super::curve_rows::section(ui, snarl, node_id, "orient", "Orientation output", false) {
        let cells = lay(ui, &[SOURCE_W, ROT_W, DRIFT_W, RECEN_W]);
        let cell = |i: usize| cells[i];

        // Where the Orientation output's pose comes from: the device's own, this module's
        // integration, or whichever is there.
        {
            let r = cell(0);
            let pill = egui::Rect::from_min_size(egui::pos2(r.left(), r.center().y - ctrl * 0.5), egui::vec2(r.width(), ctrl));
            if let Some(src) = super::curve_rows::segmented(ui, pill, &orient_source, &GYRO_SOURCE_OPTIONS, ("gyro_source", node_id)) {
                orient_source = src;
                changed = true;
            }
            source_rect = Some(r);
            row_fields.push(("orient_source", vec![pill]));
        }

        // What is done to that pose on the way out. Both settings are about the integration,
        // so both grey out when the pose is the device's own.
        {
            let r = cell(1);
            let pr = egui::Rect::from_min_size(egui::pos2(r.left(), r.center().y - ctrl * 0.5), egui::vec2(r.width(), ctrl));
            let shown = format!("{orient_scale:.2}");
            widget_parts.push(("orient_scale", r));
            row_fields.push(("orient_scale", vec![on(pose_rows_on, pr)]));
            changed |= pill_in(ui, pose_rows_on, node_id, "gyro_rot", pr, "rot ×|rotation ×", &mut orient_scale, &shown, 0.05..=5.0, 0.01,
                "Rotation scale for the 3D Orientation output only.\n1.0 = physically 1:1 on known controllers. If the\non-screen model over- or under-rotates vs. your real\ncontroller, adjust here. Applied to the integration rate\nso it never flips; does NOT affect pointer/steering.");
        }
        {
            let r = cell(2);
            let pr = egui::Rect::from_min_size(egui::pos2(r.left(), r.center().y - ctrl * 0.5), egui::vec2(r.width(), ctrl));
            let shown = format!("{orient_drift:.2}");
            widget_parts.push(("orient_drift", r));
            row_fields.push(("orient_drift", vec![on(pose_rows_on, pr)]));
            changed |= pill_in(ui, pose_rows_on, node_id, "gyro_drift", pr, "drift|drift fix", &mut orient_drift, &shown, 0.0..=1.0, 0.01,
                "Accelerometer drift correction for the 3D Orientation\noutput. While the controller is held steady, gravity\npulls accumulated pitch/roll drift back out (yaw can't\nbe corrected — reset for that). OFF by default: linear\nmotion (shakes/swings) is indistinguishable from tilt\nand can rotate the model falsely — prefer Auto re-center.\nHigher = faster pull; reference pose captured on Reset.");
        }
        {
            let r = cell(3);
            let mut right = r.right();
            right -= 92.0;
            let thr = egui::Rect::from_min_size(egui::pos2(right, r.center().y - ctrl * 0.5), egui::vec2(92.0, ctrl));
            right -= super::kit::GAP;
            let tw = word_w(ui, "auto re-center");
            right -= tw;
            let tog = egui::Rect::from_min_size(egui::pos2(right, r.center().y - ctrl * 0.5), egui::vec2(tw, ctrl));
            // Orient's camera re-centres whatever the pose's source: it is measured in the
            // module's own orientation either way.
            let recenter_on = pose_rows_on || axis_id == "orient";
            changed |= word_toggle(ui, tog, recenter_on, &mut yaw_recenter, "auto re-center",
                "When gyro readings stay under the threshold for 3 s,\ngradually re-center the 3D Orientation output (all axes)\nuntil it's centered or the threshold is exceeded again.\nIn Orient mode it also eases the pointer back onto the\npad's nose and the heading back to forward, as a Reset\nwould, sending no movement: a pad whose gyro read a turn\nit didn't make (just connected, still calibrating) lines\nup with its pointer again once it lies still.",
                ("gyro_recenter_toggle", node_id));
            widget_parts.push(("orient_recenter", r));
            row_fields.push(("orient_recenter", vec![on(recenter_on, tog), on(recenter_on && yaw_recenter, thr)]));
            let shown = format!("{yaw_thresh:.4}");
            changed |= pill_in(ui, recenter_on && yaw_recenter, node_id, "gyro_thr", thr, "still|still below", &mut yaw_thresh, &shown, 0.0005..=0.2, 0.0005,
                "Gyro magnitude (normalized, 1.0 = 2000 dps) that counts\nas \"not moving\". With the noise-floor deadzone armed,\nresting readings are exactly 0, so the default is fine.");
        }

        }

        if super::curve_rows::section(ui, snarl, node_id, "axes", "Invert Axes", false) {
        // ── The inversions ──────────────────────────────────────────────────
        // A group each - a caption and three words saying which way a reading has to go for
        // the module to be right, each a button that fills when it has to be flipped - side
        // by side while the width holds both, and a row each when it doesn't.
        let font = egui::TextStyle::Small.resolve(ui.style());
        let text_w = |ui: &egui::Ui, t: &str| ui.painter().layout_no_wrap(t.to_string(), font.clone(), egui::Color32::PLACEHOLDER).size().x;
        let group_w = |ui: &egui::Ui, caption: &str, labels: &[(&str, &str); 3]| {
            text_w(ui, caption) + 6.0 + labels.iter().map(|(l, _)| word_w(ui, l) + super::kit::GAP).sum::<f32>()
        };
        let mins = [group_w(ui, "Gyro", &GYR_LABELS), group_w(ui, "Accel", &ACC_LABELS)];
        let cells = lay(ui, &mins);
        for ((caption, values, labels, out), cell) in [
            ("Gyro", &mut inv_gyro, &GYR_LABELS, &mut gyr_rect),
            ("Accel", &mut inv_accel, &ACC_LABELS, &mut acc_rect),
        ]
        .into_iter()
        .zip(cells)
        {
            let weak = ui.visuals().weak_text_color();
            ui.painter().text(egui::pos2(cell.left(), cell.center().y), egui::Align2::LEFT_CENTER, caption, font.clone(), weak);
            let mut x = cell.left() + text_w(ui, caption) + 6.0;
            let mut fields = Vec::new();
            for (i, (label, tip)) in labels.iter().enumerate() {
                let bw = word_w(ui, label);
                let br = egui::Rect::from_min_size(egui::pos2(x, cell.center().y - ctrl * 0.5), egui::vec2(bw, ctrl));
                changed |= word_toggle(ui, br, true, &mut values[i], label, tip, (caption, i));
                fields.push(br);
                x += bw + super::kit::GAP;
            }
            *out = Some(cell);
            row_fields.push((if caption == "Gyro" { "gyro_invert" } else { "accel_invert" }, fields));
        }

        }

        // Lean: in its own column on the right when there's room, else on under the settings.
        // In a column of their own the lists are as wide as it, their cards scaled to it as
        // the other modules' mapping columns are; else at the width cards are drawn at.
        let list_w = if lean_col.is_some() { lean_w } else { LEAN_LIST_W };
        let (ui, w): (&mut egui::Ui, f32) = match (lean_col.as_mut(), settings_col.as_mut()) {
            (Some(c), _) => (c, lean_w),
            (None, Some(c)) => (c, settings_w),
            (None, None) => (&mut *parent, w),
        };
        let lay = |ui: &mut egui::Ui, mins: &[f32]| -> Vec<egui::Rect> {
            let (cells, h) = super::curve_rows::bands(w, mins, band, 3.0 * super::kit::GAP);
            let (area, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
            cells.into_iter().map(|c| c.translate(area.min.to_vec2())).collect()
        };
        if super::curve_rows::section(ui, snarl, node_id, "lean", "Lean", true) {
        let cells = lay(ui, &[LEAN_W]);
        let cell = |i: usize| cells[i];
        {
            let r = cell(0);
            let pr = egui::Rect::from_min_size(egui::pos2(r.left(), r.center().y - ctrl * 0.5), egui::vec2((r.width() - 44.0).max(70.0), ctrl));
            let shown = format!("{lean_threshold:.2}");
            changed |= super::curve_rows::real_pill(
                ui, node_id, "gyro_lean", pr, "lean|lean threshold", &mut lean_threshold, &shown, 0.01..=4.0, 0.01,
                "|Lean| ≥ threshold triggers the Lean! Bool output.",
            );
            row_fields.push(("lean_threshold", vec![pr]));
            // Painted, not laid out: the reading moves every frame and the module must not.
            ui.painter().text(
                egui::pos2(pr.right() + 4.0, r.center().y),
                egui::Align2::LEFT_CENTER,
                format!("{lean_v:+.2}"),
                super::simple_bodies::control_font(),
                ui.visuals().weak_text_color(),
            );
            lean_rect = Some(r);
        }


        // ── The Lean mapping lists ──────────────────────────────────────────
        // The shared card renderer: the same cards as a pinned Remapper, one list per lean
        // direction, each with its own Learn/Choose state machine - and each in a card window
        // of its own (`pinned::card_window`): its title and Learn row stay put, its cards
        // scroll under them in the height its bar gives it, the frame growing or shrinking by
        // the same so the preview keeps its size. Until a bar is first dragged its list is as
        // tall as it is. They shared one scroll, the first list's cards pushing the second's
        // out of view.
        for (side, tag, key, out) in [
            ("left", "lean_l", "lean_l_h", &mut lean_left_rect),
            ("right", "lean_r", "lean_r_h", &mut lean_right_rect),
        ] {
            let rect = super::pinned::card_window(
                ui, snarl, node_id, tag, list_w, key, band,
                live_signals, panic_shortcut, automap_parent,
                move |id, ins, ui, sn, sigs, panic, am| {
                    show_gyro_lean_mapping_section(id, ui, sn, side, ins, sigs, panic, am);
                },
                move |n: &NodeData| lean_hash(n, if side == "left" { "lean_left" } else { "lean_right" }),
                move |n: &NodeData| lean_hash(n, if side == "left" { "_lean_left_draft" } else { "_lean_right_draft" }),
                None,
            );
            if side == "left" {
                super::simple_bodies::trace_rect(ui, node_id, "gyro_lean", rect);
            }
            *out = Some(rect);
        }
        let _ = inputs;
        }
        // Beside the rest, the preview runs the whole height of the body: the grip's, or what
        // the columns beside it came out at if that's more - it never squeezes in under
        // them, and there's no bar of its own to say otherwise.
        let rest_bottom = [settings_col.as_ref(), lean_col.as_ref()]
            .into_iter()
            .flatten()
            .map(|c| c.min_rect().bottom())
            .fold(rest_top, f32::max);
        let preview_col = wide.map(|b| {
            let mut col = column(parent, top_left.x, top_left.y, b.left_w);
            if super::curve_rows::section(&mut col, snarl, node_id, "view", "3D preview", true) {
                let bottom = rest_bottom.max(top_left.y + body_h);
                let h = (bottom - col.cursor().min.y).max(super::gyro_view::GYRO_VIEW_MIN.y);
                draw_preview(&mut col, snarl, egui::vec2(b.left_w, h));
            }
            col
        });
        // In columns, the body takes them all in, with the bars between them; the surface's
        // own sizing is the stacked layout's, so beside the preview it measures all this as
        // rows.
        let all = [preview_col.as_ref(), settings_col.as_ref(), lean_col.as_ref()]
            .into_iter()
            .flatten()
            .map(|c| c.min_rect())
            .reduce(|a, b| a.union(b));
        if let Some(all) = all {
            parent.allocate_rect(all, egui::Sense::hover());
            if let Some(b) = wide {
                let bar = egui::Rect::from_min_max(
                    egui::pos2(top_left.x + b.left_w, top_left.y),
                    egui::pos2(top_left.x + b.left_w + super::kit::BESIDE_BAR_W, all.bottom()),
                );
                super::kit::beside_bar(parent, snarl, node_id, b, bar, "Drag to share the width between the preview and the settings. Double-click to reset.");
            }
            if let Some(lb) = lean_beside {
                let bar = egui::Rect::from_min_max(
                    egui::pos2(rest_x + lb.left_w, rest_top),
                    egui::pos2(rest_x + lb.left_w + super::kit::BESIDE_BAR_W, all.bottom()),
                );
                super::kit::beside_bar(parent, snarl, node_id, lb, bar, "Drag to share the width between the settings and Lean. Double-click to reset.");
            }
        }
        if wide.is_none() && view_drawn {
            surface.end(parent);
        } else {
            surface.end_folded(parent);
        }
    });

    if let Some(r) = pointer_rect    { register_exposable_element(ui, node_id, "pointer_mode",  r); }
    if let Some(r) = steering_rect   { register_exposable_element(ui, node_id, "steering_mode", r); }
    if let Some(r) = stopts_rect     { register_exposable_element(ui, node_id, "steering_opts", r); }
    if let Some(r) = roll_mix_rect   { register_exposable_element(ui, node_id, "roll_mix",      r); }
    if let Some(r) = hold_rect       { register_exposable_element(ui, node_id, "hold_offset",   r); }
    if let Some(r) = edge_rect       { register_exposable_element(ui, node_id, "edge_turn",     r); }
    if let Some(r) = source_rect     { register_exposable_element(ui, node_id, "orient_source", r); }
    if let Some(r) = gyr_rect        { register_exposable_element(ui, node_id, "gyro_invert",   r); }
    if let Some(r) = acc_rect        { register_exposable_element(ui, node_id, "accel_invert",  r); }
    if let Some(r) = lean_rect       { register_exposable_element(ui, node_id, "lean_threshold", r); }
    if let Some(r) = lean_left_rect  { register_exposable_element(ui, node_id, "lean_left",     r); }
    if let Some(r) = lean_right_rect { register_exposable_element(ui, node_id, "lean_right",    r); }
    for (element, r) in &widget_parts {
        register_widget_part(ui, node_id, element, *r);
    }
    for (element, fields) in &row_fields {
        publish_widget_row_fields(ui, node_id, element, fields);
    }

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("family".into(),         Value::String(family));
            node.params.insert("axis".into(),           Value::String(axis));
            // Strip the legacy `mode` key so it can't drift out of sync.
            node.params.remove("mode");
            node.params.insert("inv_yaw".into(),        Value::Bool(inv_gyro[0]));
            node.params.insert("inv_pitch".into(),      Value::Bool(inv_gyro[1]));
            node.params.insert("inv_roll".into(),       Value::Bool(inv_gyro[2]));
            node.params.insert("inv_accel_x".into(),    Value::Bool(inv_accel[0]));
            node.params.insert("inv_accel_y".into(),    Value::Bool(inv_accel[1]));
            node.params.insert("inv_accel_z".into(),    Value::Bool(inv_accel[2]));
            node.params.insert("steering_exclude_y".into(),
                Value::Bool(exclude_y));
            node.params.remove("recenter_blend");
            node.params.insert("recenter_strength".into(),
                serde_json::Number::from_f64(recenter_strength as f64).map(Value::Number).unwrap_or(Value::Null));
            node.params.insert("reset_ease_in".into(),
                serde_json::Number::from_f64(reset_ease_in as f64).map(Value::Number).unwrap_or(Value::Null));
            // Spike-filter moved to the device polling layer; drop the
            // legacy per-module params so they can't drift out of sync.
            node.params.remove("spike_suppress");
            node.params.remove("spike_sensitivity");
            node.params.remove("spike_k");
            node.params.insert("lean_threshold".into(),
                serde_json::Number::from_f64(lean_threshold as f64).map(Value::Number).unwrap_or(Value::Null));
            node.params.insert("orient_scale".into(),
                serde_json::Number::from_f64(orient_scale as f64).map(Value::Number).unwrap_or(Value::Null));
            node.params.insert("orient_drift".into(),
                serde_json::Number::from_f64(orient_drift as f64).map(Value::Number).unwrap_or(Value::Null));
            node.params.insert("orient_auto_recenter".into(), Value::Bool(yaw_recenter));
            node.params.insert("orient_source".into(), Value::String(orient_source.into()));
            edge.write(node);
            node.params.insert("orient_recenter_thresh".into(),
                serde_json::Number::from_f64(yaw_thresh as f64).map(Value::Number).unwrap_or(Value::Null));
            node.params.insert("roll_contribution".into(),
                serde_json::Number::from_f64(roll_mix as f64).map(Value::Number).unwrap_or(Value::Null));
            gyro_write_hold(node, hold);
        }
    }
}

/// The 3D preview pinned on its own, filling `container`: the same picture as the module's
/// body, read off the node - and the same orbit, which is kept per node. `parent` is the
/// sub-patch the module sits in, so the pad is traced out through its inlet to the device
/// (the model and icon are that pad's; the pin drew the first model on disk without it), and
/// `style` the pin's own frame and look (`IvStyleOverride`, set in the layout inspector).
pub(crate) fn render_gyro_preview_pinned(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
    container: egui::Vec2,
    parent: Option<&AutomapGlowParent<'_>>,
    style: Option<&crate::canvas::node::IvStyleOverride>,
) {
    let (rect, _) = ui.allocate_exact_size(container, egui::Sense::hover());
    let Some(n) = snarl.get_node(node_id) else { return };
    let (family, axis) = gyro_read_family_axis(n);
    let b = |k: &str| n.params.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let f = |k: &str, d: f64| n.params.get(k).and_then(|v| v.as_f64()).unwrap_or(d) as f32;
    let out = |i: usize| match n.extra.last_out.get(i) {
        Some(Some(Signal::Float(f))) => *f,
        _ => 0.0,
    };
    let pose = match n.extra.last_out.get(5) {
        Some(Some(Signal::Vec4(v))) if v.length_squared() > 1e-6 => Some(glam::Quat::from_xyzw(v.x, v.y, v.z, v.w).normalize()),
        _ => None,
    };
    let hold = gyro_read_hold(n);
    let hold_q = glam::Quat::from_rotation_z(hold[1].to_radians())
        * glam::Quat::from_rotation_y(hold[0].to_radians())
        * glam::Quat::from_rotation_x(hold[2].to_radians());
    let axis_id: &'static str = match axis.as_str() {
        "pitch_roll" => "pitch_roll",
        "player" => "player",
        "world" => "world",
        "orient" => "orient",
        _ => "pitch_yaw",
    };
    let edge = GyroEdge::read(n);
    let look = match style.and_then(|s| s.gyro_look.as_deref()) {
        Some("icon") => "icon",
        Some("outline") => "outline",
        Some("model") => "model",
        _ => gyro_preview_look(n),
    };
    let traced_devs = {
        let traced = snarl
            .in_pin(InPinId { node: node_id, input: 0 })
            .remotes
            .first()
            .copied()
            .map(|src| controller3d_physical_devices(snarl, src, parent))
            .unwrap_or_default();
        controller3d_devices_and_deadzone(traced).0
    };
    let model = if look == "model" {
        Some(crate::model::model_for_devices(&traced_devs).unwrap_or_else(crate::model::default_model))
    } else {
        None
    };
    let rgba = |c: [u8; 4]| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
    let frame = style.map(|s| (rgba(s.bg), egui::Stroke::new(s.outline_px, rgba(s.outline))));
    let view = super::gyro_view::GyroView {
        axes: super::gyro_view::GyroAxes::of(ui.visuals()),
        pose,
        aim: gyro_aim(n),
        view: gyro_view_pose(n),
        hold: hold_q,
        zone: (family == "pointer" && edge.mode != "off").then_some(edge.zone),
        proj: super::gyro_view::GyroProjection {
            axis: axis_id,
            roll_share: f("roll_contribution", 0.0) / 100.0,
            inv: [b("inv_yaw"), b("inv_pitch"), b("inv_roll")],
            physical: aim_physical(gyro_aim(n)),
        },
        model: model.as_deref(),
        pad_svg: gyro_pad_svg(&traced_devs),
        outline: look == "outline",
        lean: out(3),
        lean_threshold: f("lean_threshold", 0.3),
        edge: (family == "pointer").then(|| gyro_edge_params(n)).flatten(),
        orient_scale: f("orient_scale", 1.0),
        frame,
        // The pin's own, else the module's.
        model_alpha: style.and_then(|s| s.c3d_composite).unwrap_or_else(|| gyro_model_alpha(n)),
    };
    super::gyro_view::draw_gyro_view(ui, node_id, rect, &view);
}

/// The module's Edge-turn params, for the preview to run Edge turn as the module does - or
/// `None` while it is off.
fn gyro_edge_params(n: &NodeData) -> Option<std::collections::HashMap<String, Value>> {
    let mode = n.params.get("edge_mode").and_then(|v| v.as_str()).unwrap_or("off");
    if mode == "off" {
        return None;
    }
    Some(n.params.iter().filter(|(k, _)| k.starts_with("edge_")).map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// Where the module has added its X/Y outputs up to, as it puts them out: its display-only
/// trailing output (`GYRO_AIM_OUT`) - x°, y°, x° without roll's share, and the pose's kind.
fn gyro_aim(n: &NodeData) -> Option<glam::Vec4> {
    match n.extra.last_out.get(flexinput_engine::eval::GYRO_AIM_OUT) {
        Some(Some(Signal::Vec4(v))) => Some(*v),
        _ => None,
    }
}

/// The pad as the output has it (`GYRO_VIEW_OUT`), for the preview to draw.
fn gyro_view_pose(n: &NodeData) -> Option<glam::Quat> {
    match n.extra.last_out.get(flexinput_engine::eval::GYRO_VIEW_OUT) {
        Some(Some(Signal::Vec4(v))) if v.length_squared() > 1e-6 => Some(glam::Quat::from_xyzw(v.x, v.y, v.z, v.w).normalize()),
        _ => None,
    }
}

/// Whether the aim says the pose is the device's own (kind 2), which is physical.
fn aim_physical(aim: Option<glam::Vec4>) -> bool {
    aim.is_some_and(|a| (a.w - 2.0).abs() < 0.5)
}

/// How opaque the preview's 3D model is, by default.
const GYRO_MODEL_ALPHA: f32 = 0.5;

/// How opaque the preview's 3D model is (`view_model_alpha`).
fn gyro_model_alpha(n: &NodeData) -> f32 {
    n.params.get("view_model_alpha").and_then(|v| v.as_f64()).map_or(GYRO_MODEL_ALPHA, |v| v as f32).clamp(0.05, 1.0)
}

/// How the preview draws the pad: `icon`, `outline` or `model`. A patch from before the
/// choice had a `model` switch; on reads as `model`.
fn gyro_preview_look(n: &NodeData) -> &'static str {
    match n.params.get("view_look").and_then(|v| v.as_str()) {
        Some("outline") => "outline",
        Some("model") => "model",
        Some(_) => "icon",
        None if n.params.get("view_model").and_then(|v| v.as_bool()) == Some(true) => "model",
        None => "icon",
    }
}

/// The icon of the pad the module reads - its device card's - or `None` for the generic one.
fn gyro_pad_svg(devs: &[String]) -> Option<&'static [u8]> {
    devs.iter().find_map(|d| match crate::canvas::remapper_icons::device_node_icon_for_id(d)? {
        crate::canvas::remapper_icons::NodeIconSpec::Single(svg) => Some(svg),
    })
}

pub(crate) const GYRO_ROLL_MIX_TIP: &str =
    "Pitch+Yaw only: mix a share of roll into the turn (X).\n\
     Same setting as the JSM module's ROLL_CONTRIBUTION.\n\
     Positive puts back the turn a far-edge-raised grip loses\n\
     into roll; negative makes rolling right turn right, like\n\
     a wheel. 0 = off.";

pub(crate) const GYRO_HOLD_TIP: &str =
    "Neutral hold: how the pad sits when you hold it at rest.\n\
     The 2D outputs and Lean are measured from this hold\n\
     instead of from flat. The 3D Orientation output is not.\n\
     \n\
     Pitch+Yaw / Pitch+Roll: gyro axes and gravity are both\n\
     re-measured about the hold (JSM's LOCAL_AXIS_OFFSET, on\n\
     all three axes). E.g. a handheld tilted back 40° → P 40.\n\
     \n\
     Player / World: only gravity is rotated, shifting which\n\
     way counts as down. E.g. lying on your right side with\n\
     the pad held normally → R 90.";

pub(crate) const GYRO_HOLD_AXIS_TIPS: [&str; 3] = [
    "Pitch: + = far (trigger) edge raised",
    "Yaw: + = nose turned right",
    "Roll: + = right grip down",
];

/// The neutral hold offsets `[pitch, yaw, roll]` in degrees.
pub(crate) fn gyro_read_hold(n: &NodeData) -> [f32; 3] {
    let g = |k: &str| n.params.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    [g("hold_pitch"), g("hold_yaw"), g("hold_roll")]
}

/// `orient_source`, absent = "auto".
pub(crate) fn gyro_read_orient_source(n: &NodeData) -> &'static str {
    match n.params.get("orient_source").and_then(|v| v.as_str()) {
        Some("computed") => "computed",
        Some("device") => "device",
        _ => "auto",
    }
}

/// The three places the Orientation output's pose can come from, with what each means.
const GYRO_SOURCE_OPTIONS: [(&str, &str, &str); 3] = [
    ("auto", "auto", "The device's own orientation when it publishes one\n(Joy-Con 2), otherwise Computed."),
    ("computed", "computed", "Integrate this module's own orientation from the gyro\n(with the drift fix / re-center options below)."),
    ("device", "device", "Only the orientation the device fuses itself (Joy-Con 2:\ngravity-held roll and pitch, relative to the\nCalibration window's baseline; yaw is integrated,\nwith no compass, so it can still drift). A pad\nwithout one gets no orientation — and no Edge turn."),
];

/// The Auto / Computed / Device picker. Returns true on a change.
pub(crate) fn gyro_orient_source_buttons(ui: &mut egui::Ui, source: &mut &'static str, small: bool) -> bool {
    let mut changed = false;
    for (id, lbl, tip) in [
        ("auto", "Auto", "The device's own orientation when it publishes one\n\
                          (Joy-Con 2), otherwise Computed."),
        ("computed", "Computed", "Integrate this module's own orientation from the gyro\n\
                                  (with the drift fix / re-center options below)."),
        ("device", "Device", "Only the orientation the device fuses itself (Joy-Con 2:\n\
                              gravity-held roll and pitch, relative to the\n\
                              Calibration window's baseline; yaw is integrated,\n\
                              with no compass, so it can still drift). A pad\n\
                              without one gets no orientation — and no Edge turn."),
    ] {
        let text = if small { egui::RichText::new(lbl).small() } else { egui::RichText::new(lbl) };
        if ui.selectable_label(*source == id, text).on_hover_text(tip).clicked() && *source != id {
            *source = id;
            changed = true;
        }
    }
    changed
}

pub(crate) const GYRO_EDGE_TIP: &str =
    "Edge turn (Pointer only): within the zone around the neutral\n\
     pose — where Reset last put it — the gyro moves the camera as\n\
     usual. Turned past it, the camera keeps turning on its own,\n\
     the way it was already going, until you come back in.\n\
     \n\
     Nothing switches at the zone: it all eases in past it.\n\
     \n\
     Ramp: like a stick past its deadzone — the further out, the\n\
     faster, reaching max at zone + ramp.\n\
     Momentum: the camera doesn't slow below the speed you turned\n\
     out at (× gain), eased in over blend — keep turning and it\n\
     adds nothing, ease off and it makes up the difference. Pull\n\
     back to brake. fade lets it die away (0 = holds).\n\
     Both: the two together.\n\
     gyro: how much of the gyro's own aim is left once past the\n\
     blend — 100% keeps it, 0% hands over to the edge entirely.\n\
     \n\
     Measured on the Orientation output's pose. Its yaw can drift\n\
     (a computed pose more than a Joy-Con 2's) — Reset re-centres\n\
     the zone.";

/// Edge turn settings as edited (`edge_*` params).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GyroEdge {
    pub mode: &'static str,
    pub x_only: bool,
    pub zone: f32,
    pub blend: f32,
    pub ramp: f32,
    pub max_dps: f32,
    pub gain: f32,
    pub decay: f32,
    /// The gyro's direct share past the blend, 0..1 (shown as %).
    pub gyro_keep: f32,
}

impl Default for GyroEdge {
    fn default() -> Self {
        GyroEdge {
            mode: "off", x_only: false, zone: 30.0, blend: 15.0, ramp: 30.0, max_dps: 180.0,
            gain: 1.0, decay: 0.0, gyro_keep: 1.0,
        }
    }
}

pub(crate) const GYRO_EDGE_MODES: [(&str, &str); 4] =
    [("off", "Off"), ("ramp", "Ramp"), ("momentum", "Momentum"), ("both", "Both")];

impl GyroEdge {
    pub(crate) fn read(n: &NodeData) -> Self {
        let d = GyroEdge::default();
        let f = |k: &str, v: f32| n.params.get(k).and_then(|x| x.as_f64()).map_or(v, |x| x as f32);
        let mode = n.params.get("edge_mode").and_then(|v| v.as_str())
            .and_then(|m| GYRO_EDGE_MODES.iter().find(|(id, _)| *id == m).map(|(id, _)| *id))
            .unwrap_or("off");
        GyroEdge {
            mode,
            x_only: n.params.get("edge_x_only").and_then(|v| v.as_bool()).unwrap_or(false),
            zone: f("edge_zone_deg", d.zone),
            blend: f("edge_blend_deg", d.blend),
            ramp: f("edge_ramp_deg", d.ramp),
            max_dps: f("edge_max_dps", d.max_dps),
            gain: f("edge_momentum", d.gain),
            decay: f("edge_decay_s", d.decay),
            gyro_keep: f("edge_gyro_keep", d.gyro_keep),
        }
    }

    pub(crate) fn write(&self, node: &mut NodeData) {
        let num = |v: f32| serde_json::Number::from_f64(v as f64).map(Value::Number).unwrap_or(Value::Null);
        node.params.insert("edge_mode".into(), Value::String(self.mode.into()));
        node.params.insert("edge_x_only".into(), Value::Bool(self.x_only));
        node.params.insert("edge_zone_deg".into(), num(self.zone));
        node.params.insert("edge_blend_deg".into(), num(self.blend));
        node.params.insert("edge_gyro_keep".into(), num(self.gyro_keep));
        node.params.insert("edge_ramp_deg".into(), num(self.ramp));
        node.params.insert("edge_max_dps".into(), num(self.max_dps));
        node.params.insert("edge_momentum".into(), num(self.gain));
        node.params.insert("edge_decay_s".into(), num(self.decay));
    }

    /// The Edge block: one row of mode cells and `X only`, then the seven numbers as value
    /// pills in bands that wrap when the width runs out. `small` is the node body's text
    /// size - a pinned copy draws it at the regular one - and `w` the width the caller has
    /// for it. Returns true on a change, and fills `rects` with each control's rect in the
    /// order `app::nav::fields` walks them: mode, X only, zone, blend, gyro, ramp, max,
    /// gain, fade.
    pub(crate) fn ui_rects(
        &mut self,
        ui: &mut egui::Ui,
        node: NodeId,
        w: f32,
        small: bool,
        rects: &mut [egui::Rect; 9],
    ) -> bool {
        let band = super::curve_rows::band_h(ui);
        let ctrl = band - 4.0;
        let mut changed = false;
        let off = self.mode == "off";

        // The caption, the four mode cells, and `X only` at the row's end. The caption is
        // painted into room taken off the cells, so it can never be what widened the module.
        {
            let (row, _) = ui.allocate_exact_size(egui::vec2(w, band), egui::Sense::hover());
            let font = egui::TextStyle::Small.resolve(ui.style());
            let weak = ui.visuals().weak_text_color();
            let cap = "Edge turn";
            let cap_w = caption_w(ui);
            ui.painter().text(
                egui::pos2(row.left(), row.center().y),
                egui::Align2::LEFT_CENTER,
                cap,
                font,
                weak,
            );
            ui.interact(
                egui::Rect::from_min_size(egui::pos2(row.left(), row.top()), egui::vec2(cap_w, band)),
                egui::Id::new(("gyro_edge_cap", node)),
                egui::Sense::hover(),
            )
            .on_hover_text(GYRO_EDGE_TIP);

            let xw = word_w(ui, "X only");
            let cells = egui::Rect::from_min_max(
                egui::pos2(row.left() + cap_w + 4.0, row.top()),
                egui::pos2(row.right() - xw - 4.0, row.bottom()),
            );
            let _ = small;
            let strip = egui::Rect::from_min_size(egui::pos2(cells.left(), row.center().y - ctrl * 0.5), egui::vec2(cells.width(), ctrl));
            let opts: Vec<(&'static str, &'static str, &'static str)> =
                GYRO_EDGE_MODES.iter().map(|(id, lbl)| (*id, *lbl, GYRO_EDGE_TIP)).collect();
            if let Some(m) = super::curve_rows::segmented(ui, strip, &self.mode, &opts, ("gyro_edge_mode", node)) {
                self.mode = m;
                changed = true;
            }
            rects[0] = strip;
            let xr = egui::Rect::from_min_size(egui::pos2(row.right() - xw, row.center().y - ctrl * 0.5), egui::vec2(xw, ctrl));
            changed |= word_toggle(
                ui, xr, !off, &mut self.x_only, "X only",
                "Horizontal only: pitch neither counts toward the zone\nnor turns the camera.",
                ("edge_x_only", node),
            );
            rects[1] = xr;
        }

        // The seven numbers. Greyed rather than hidden when the mode has no use for them: a
        // control that vanishes on a mode change is a control you can't see to change back,
        // and the zone ring on the viewport would go with it.
        {
            let mins = [EDGE_ZONE_W, EDGE_BLEND_W, EDGE_GYRO_W, EDGE_RAMP_W, EDGE_MAX_W, EDGE_MOM_W, EDGE_FADE_W];
            let (cells, h) = super::curve_rows::bands(w, &mins, band, 3.0 * super::kit::GAP);
            let (area, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
            let at = |i: usize| {
                let c = cells[i].translate(area.min.to_vec2());
                egui::Rect::from_min_size(egui::pos2(c.left(), c.center().y - ctrl * 0.5), egui::vec2(c.width(), ctrl))
            };
            let ramp_on = matches!(self.mode, "ramp" | "both");
            let mom_on = matches!(self.mode, "momentum" | "both");

            let shown = format!("{:.0}°", self.zone);
            changed |= pill_in(ui, !off, node, "edge_zone", at(0), "zone|edge zone", &mut self.zone, &shown, 0.0..=170.0, 0.5,
                "Where the edge starts, from neutral. Pure gyro inside.");
            let shown = format!("{:.0}°", self.blend);
            changed |= pill_in(ui, !off, node, "edge_blend", at(1), "blend", &mut self.blend, &shown, 0.0..=170.0, 0.5,
                "Degrees past the zone over which momentum and the\ngyro hand-off ease in (0 = all at once).");
            let mut pct = self.gyro_keep * 100.0;
            let shown = format!("{pct:.0} %");
            if pill_in(ui, !off, node, "edge_gyro", at(2), "gyro|gyro kept", &mut pct, &shown, 0.0..=100.0, 0.5,
                "The gyro's own aim left once past the blend: 100% keeps\nit (the edge only adds), 0% hands over to the edge\nentirely, like a stick.")
            {
                self.gyro_keep = pct / 100.0;
                changed = true;
            }
            let shown = format!("{:.0}°", self.ramp);
            changed |= pill_in(ui, ramp_on, node, "edge_ramp", at(3), "ramp|ramp width", &mut self.ramp, &shown, 0.0..=170.0, 0.5,
                "Degrees past the zone where Ramp reaches max, eased in\n(0 = full speed as soon as you leave it).");
            let shown = format!("{:.0} °/s", self.max_dps);
            changed |= pill_in(ui, ramp_on, node, "edge_max", at(4), "max|max speed", &mut self.max_dps, &shown, 0.0..=4000.0, 2.0,
                "Ramp's top speed, as a gyro turn rate - downstream\nsensitivity scales it like the gyro.");
            let shown = format!("{:.2}", self.gain);
            changed |= pill_in(ui, mom_on, node, "edge_gain", at(5), "mom ×|momentum ×", &mut self.gain, &shown, 0.0..=10.0, 0.02,
                "Share of the turn-out speed kept as the camera's floor\n(1 = carries on as fast as the pad was turning).");
            let shown = format!("{:.2} s", self.decay);
            changed |= pill_in(ui, mom_on, node, "edge_decay", at(6), "fade|momentum fade", &mut self.decay, &shown, 0.0..=30.0, 0.02,
                "Momentum fade time constant (0 = holds while you stay out).");

            for (i, r) in (0..7).map(at).enumerate() {
                rects[2 + i] = r;
            }
        }
        changed
    }

}

// ── The controls this body places at a rect it worked out ────────────────────
// The band machinery (`curve_rows::bands`) hands a group a rect and these draw the control
// into it. Nothing here goes into the flow: a body handed its width by `kit::Surface` draws
// inside the width it was given and never asks for more, which is what keeps the corner grip
// meaning something.

/// The Lean card lists are drawn at the shared card renderer's own width whatever the grip
/// asks for: the same cards, at the same size as their pinned copy.
const LEAN_LIST_W: f32 = 380.0;
/// The narrowest the preview's column gets beside the settings.
const PREVIEW_MIN_W: f32 = 160.0;
/// The narrowest the settings' column gets beside Lean's: the widest band of them, the
/// Steering row's, with the mode rows' captions and icons.
const SETTINGS_MIN_W: f32 = 300.0;

/// The narrowest each group of pills reads at. `bands` shares the rest of a band among the
/// groups in it, so these are floors rather than fixed widths.
const HOLD_W: f32 = 216.0;
const ROLL_W: f32 = 96.0;
const LEAN_W: f32 = 150.0;
const STEER_W: f32 = 268.0;
const SOURCE_W: f32 = 176.0;
const ROT_W: f32 = 96.0;
const DRIFT_W: f32 = 104.0;
const RECEN_W: f32 = 216.0;
const EDGE_ZONE_W: f32 = 84.0;
const EDGE_BLEND_W: f32 = 92.0;
const EDGE_GYRO_W: f32 = 96.0;
const EDGE_RAMP_W: f32 = 86.0;
const EDGE_MAX_W: f32 = 104.0;
const EDGE_MOM_W: f32 = 104.0;
const EDGE_FADE_W: f32 = 88.0;

/// How wide a word button has to be to hold `label` at the size it is painted in.
/// The column the option rows' captions stand in - Pointer, Steering, Edge turn, Gyro,
/// Accel - so every row's pill starts at the same x whatever its word.
fn caption_w(ui: &egui::Ui) -> f32 {
    // The mode rows' captions have their drawing before the word.
    let small = egui::TextStyle::Small.resolve(ui.style());
    let steering = ui.painter().layout_no_wrap("Steering".to_string(), small, egui::Color32::PLACEHOLDER).size().x;
    word_w(ui, "Edge turn").max(caption_icon_h(ui) + 4.0 + steering + 4.0)
}

/// How big a mode row's caption drawing is: the small font's line, a little more.
fn caption_icon_h(ui: &egui::Ui) -> f32 {
    ui.text_style_height(&egui::TextStyle::Small) + 4.0
}

fn word_w(ui: &egui::Ui, label: &str) -> f32 {
    // In the control font the word buttons are drawn in (`kit_bodies::toggle_button`).
    let font = super::simple_bodies::control_font();
    let text = ui.painter().layout_no_wrap(label.to_string(), font, egui::Color32::PLACEHOLDER).size().x;
    (text + 2.0 * ui.spacing().button_padding.x + 2.0).max(28.0)
}

/// A boolean setting as a word button: it fills while the setting is on, and greys when the
/// row's other settings make it moot. A bare checkbox in a row of pills reads as a different
/// kind of thing; a button says "press me", which is exactly what it is.
fn word_toggle(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    enabled: bool,
    on: &mut bool,
    label: &str,
    hover: &str,
    salt: impl std::hash::Hash,
) -> bool {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight))
            .id_salt(("gyro_word", salt)),
    );
    if !enabled {
        child.disable();
        // The house button and the pill are both painted, so the grey has to come from the
        // child's opacity: `disable()` alone only stops the pointer.
        child.set_opacity(0.45);
    }
    // The house two-state plate: filled while the setting is on, outlined while it is off,
    // so the word reads as a button and not as a caption in the row.
    let resp = super::kit_bodies::toggle_button(&mut child, *on, label, rect.size());
    let resp = resp.on_hover_text(hover);
    if enabled && resp.clicked() {
        *on = !*on;
        return true;
    }
    false
}

/// A value pill that may not be settable right now: drawn either way, dragged only when it
/// is. The number is still the number the module is using, which is the reason it is shown.
#[allow(clippy::too_many_arguments)]
fn pill_in(
    ui: &mut egui::Ui,
    enabled: bool,
    node: NodeId,
    salt: &str,
    rect: egui::Rect,
    label: &str,
    value: &mut f32,
    shown: &str,
    range: std::ops::RangeInclusive<f32>,
    per_px: f32,
    hover: &str,
) -> bool {
    if enabled {
        return super::curve_rows::real_pill(ui, node, salt, rect, label, value, shown, range, per_px, hover);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("gyro_pill_off", salt)));
    child.disable();
    child.set_opacity(0.45);
    let mut v = *value;
    super::curve_rows::real_pill(&mut child, node, salt, rect, label, &mut v, shown, range, per_px, hover);
    false
}

/// One mode row: the caption, then the four axis choices as equal cells. Picking a cell sets
/// the family that row stands for as well as the axis, which is the pair the module stores;
/// the row of the family not in use lights no cell.
fn mode_row(ui: &mut egui::Ui, node: NodeId, rect: egui::Rect, caption: &str, family: &str, axis: &str, want: &str, tips: [&'static str; 5]) -> Option<&'static str> {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let weak = ui.visuals().weak_text_color();
    // The row's drawing, then its word.
    let ih = caption_icon_h(ui);
    let icon = egui::Rect::from_min_size(egui::pos2(rect.left(), rect.center().y - ih * 0.5), egui::vec2(ih, ih));
    super::curve_rows::paint_tinted_svg(ui, if want == "steering" { STEERING_ICON } else { POINTER_ICON }, icon, weak);
    ui.painter().text(egui::pos2(icon.right() + 4.0, rect.center().y), egui::Align2::LEFT_CENTER, caption, font, weak);
    let cap_w = caption_w(ui);
    let cells = egui::Rect::from_min_max(egui::pos2(rect.left() + cap_w + 4.0, rect.top()), rect.max);
    // One joined pill of the axis modes, lit only in the row of the family in use.
    let current = if family == want { axis.to_string() } else { String::new() };
    // Beside its drawing a cell says its name, or the half of it the drawing doesn't: the
    // turn X is taken from (`short|long`, `segmented_icons`).
    let word = |id: &str, lbl: &'static str| -> &'static str {
        match id {
            "pitch_yaw" => "Yaw|Pitch+Yaw",
            "pitch_roll" => "Roll|Pitch+Roll",
            _ => lbl,
        }
    };
    let opts: Vec<(String, &'static str, &'static str)> = GYRO_AXIS_OPTIONS
        .iter()
        .zip(tips)
        .map(|((id, lbl), tip)| (id.to_string(), word(id, lbl), tip))
        .collect();
    let ctrl = rect.height() - 4.0;
    let cells = egui::Rect::from_min_size(egui::pos2(cells.left(), rect.center().y - ctrl * 0.5), egui::vec2(cells.width(), ctrl));
    // The node in the id: two 3DOF modules on one canvas had the same rows, and egui gave a
    // click on either to neither.
    let icons = GYRO_AXIS_ICONS.map(Some);
    if let Some(picked) = super::curve_rows::segmented_icons(ui, cells, &current, &opts, &icons, ("gyro_mode_row", want, node.0)) {
        return Some(GYRO_AXIS_OPTIONS.iter().find(|(id, _)| *id == picked).map(|(id, _)| *id).unwrap_or("pitch_yaw"));
    }
    if family != want && ui.rect_contains_pointer(cells) && ui.input(|i| i.pointer.primary_clicked()) {
        // A click on the row of the family not in use picks that family with its axes as
        // they are - the pill has nothing lit to report a change from.
        return Some(GYRO_AXIS_OPTIONS.iter().find(|(id, _)| *id == axis).map(|(id, _)| *id).unwrap_or("pitch_yaw"));
    }
    None
}

/// A fingerprint of one of a node's array params (a Lean list's cards, or its draft): what
/// a card window re-bases its scroll on.
fn lean_hash(n: &NodeData, key: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    n.params.get(key).map(|v| v.to_string()).unwrap_or_default().hash(&mut h);
    h.finish()
}

pub(crate) fn gyro_write_hold(node: &mut NodeData, hold: [f32; 3]) {
    for (k, v) in ["hold_pitch", "hold_yaw", "hold_roll"].into_iter().zip(hold) {
        node.params.insert(k.into(),
            serde_json::Number::from_f64(v as f64).map(Value::Number).unwrap_or(Value::Null));
    }
}

/// Render one of the two lean mapping lists (left / right). Each section
/// owns an independent chord-capture state machine keyed by side so the
/// two can be edited simultaneously. Mappings are stored on the node as
/// `lean_left` / `lean_right` arrays of objects shaped like Map Action
/// mappings BUT with `out` (destination chord) instead of `in` — the lean
/// trigger IS the section, so what we capture is what to FIRE.
///
/// Schema: `{ out: [pin_id...], mode?, window_ms?, sustain?, turbo? }`.
/// Legacy entries with `in:[]` get upgraded in-place on first display.
pub(crate) fn show_gyro_lean_mapping_section(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    side: &'static str,
    inputs: &[InPin],
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    let key            = if side == "left" { "lean_left" } else { "lean_right" };
    let phase_key      = if side == "left" { "_lean_left_phase" } else { "_lean_right_phase" };
    let draft_key      = if side == "left" { "_lean_left_draft" } else { "_lean_right_draft" };
    let prev_key       = if side == "left" { "_lean_left_pressed_prev" } else { "_lean_right_pressed_prev" };
    let armed_key      = if side == "left" { "_lean_left_armed" } else { "_lean_right_armed" };
    let arm_idle_key   = if side == "left" { "_lean_left_arm_idle" } else { "_lean_right_arm_idle" };
    let title          = if side == "left" { "Lean Left → " } else { "Lean Right → " };

    // ── Migration: if legacy entries with `in` exist, rewrite to `out`.
    {
        let needs_upgrade = snarl.get_node(node_id)
            .and_then(|n| n.params.get(key).and_then(|v| v.as_array()))
            .map(|arr| arr.iter().any(|m| m.as_object()
                .map(|o| o.contains_key("in") && !o.contains_key("out")).unwrap_or(false)))
            .unwrap_or(false);
        if needs_upgrade {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if let Some(Value::Array(arr)) = node.params.get_mut(key) {
                    for v in arr.iter_mut() {
                        if let Some(obj) = v.as_object_mut() {
                            if let Some(ins) = obj.remove("in") {
                                obj.insert("out".to_string(), ins);
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Read state ───────────────────────────────────────────────────────
    let wired = inputs.first().map(|p| !p.remotes.is_empty()).unwrap_or(false);
    // Every device behind the input — all of a Combiner's ports — for capture.
    let upstream_devs: Vec<String> = if wired {
        remapper_upstream_device_ids(snarl, node_id, 0, automap_parent)
    } else { Vec::new() };

    let (phase, draft, pressed_prev, mappings) = snarl.get_node(node_id).map(|n| (
        n.params.get(phase_key).and_then(|v| v.as_str()).unwrap_or("idle").to_string(),
        remapper_read_str_array(n, draft_key),
        remapper_read_str_array(n, prev_key),
        n.params.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default(),
    )).unwrap_or_else(|| ("idle".into(), vec![], vec![], vec![]));

    // Gamepad UI-nav active for the upstream device this frame? While it is, the
    // controller drives FlexInput's UI, so capture must not begin until the
    // Learn press has been released. Mirrors the Remapper's arm/arm-idle
    // handshake but scoped per Lean side.
    let nav_active_for_device = upstream_devs.iter().any(|dev| {
        let stamp: Option<u64> = ui.ctx().data(|d|
            d.get_temp(egui::Id::new(("gp_nav_active", dev.to_string()))));
        stamp.map_or(false, |p| crate::widgets::nav_pass_matches(ui.ctx(), p))
    });
    let nav_capture_armed = snarl.get_node(node_id)
        .and_then(|n| n.params.get(armed_key)).and_then(|v| v.as_bool()).unwrap_or(false);
    let nav_arm_idle = snarl.get_node(node_id)
        .and_then(|n| n.params.get(arm_idle_key)).and_then(|v| v.as_bool()).unwrap_or(false);
    // Capture may proceed when not in nav mode, OR (nav mode) once armed AND the
    // device has gone idle once since arming (so the Learn press is released).
    let capture_ok = !nav_active_for_device || (nav_capture_armed && nav_arm_idle);
    let mut clear_capture_arm = false;
    let mut set_arm_idle: Option<bool> = None;

    // ── Capture state machine ───────────────────────────────────────────
    // Only ticks while `phase == "learning"` (active capture session).
    // No auto-enter-on-wire — Learn is button-gated, otherwise the gyro
    // module's normal AutoMap-in wire would constantly try to capture.
    let mut pressed_now: Vec<String> = Vec::new();
    if phase == "learning" {
        pressed_now = remapper_upstream_pressed_now(ui, live_signals, &upstream_devs);
        // Always merge global KB/M during learning.
        for p in remapper_kbm_pressed_now(ui, panic_shortcut) {
            if !pressed_now.iter().any(|q| q == &p) { pressed_now.push(p); }
        }
        // And every MIDI In in the patch: play a message to learn it as the
        // output, as a Remapper card does.
        for port in midi_learn_ports(ui.ctx()) {
            for p in remapper_midi_pressed_now(ui, live_signals, &port) {
                if !pressed_now.iter().any(|q| q == &p) { pressed_now.push(p); }
            }
        }
    }

    // Arm-idle latch: first frame the device is empty while armed, flip arm_idle
    // true so the NEXT non-empty press begins the capture (post Learn release).
    let now_empty = pressed_now.is_empty();
    if nav_capture_armed && !nav_arm_idle && now_empty {
        set_arm_idle = Some(true);
    }

    let mut new_phase = phase.clone();
    let mut new_draft = draft.clone();
    // The draft replaced by a new capture: its timing goes with it.
    let mut drop_out_timing = false;

    if new_phase == "learning" {
        let rising: Vec<&String> = pressed_now.iter()
            .filter(|p| !pressed_prev.iter().any(|q| q == *p))
            .collect();
        let prev_was_empty = pressed_prev.is_empty();
        // Capture gated on `capture_ok` so a still-held Learn press (nav mode)
        // doesn't get captured; once the chord lands and releases, the arm
        // clears so subsequent nav presses don't overwrite it.
        if capture_ok && !rising.is_empty() && prev_was_empty && !new_draft.is_empty() {
            // Re-capture: new burst after a previous chord — or, once the
            // draft has timing, the next step of the sequence being built.
            let chord = draft_timed_chord(snarl.get_node(node_id), draft_key, &new_draft);
            let rising: Vec<String> = rising.iter().map(|s| (*s).clone()).collect();
            new_draft = chord.learn_press(&rising);
            drop_out_timing = !chord.is_timed();
        } else if capture_ok && !pressed_now.is_empty() {
            for p in &pressed_now {
                if !new_draft.iter().any(|q| q == p) { new_draft.push(p.clone()); }
            }
        }
        // Latch on release: once a chord was captured and the device goes idle,
        // clear the arm so navigation resumes (the draft is kept for Add).
        if nav_capture_armed && now_empty && !new_draft.is_empty() {
            clear_capture_arm = true;
        }
        // No auto-latch of the mapping itself; user explicitly clicks Add/Stop.
    }

    // Persist state machine results before rendering.
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert(phase_key.to_string(), Value::String(new_phase.clone()));
        remapper_write_str_array(node, draft_key, &new_draft);
        if drop_out_timing {
            clear_draft_timing(node, draft_key);
        }
        remapper_write_str_array(node, prev_key, &pressed_now);
        if let Some(v) = set_arm_idle { node.params.insert(arm_idle_key.to_string(), Value::from(v)); }
        if clear_capture_arm {
            node.params.insert(armed_key.to_string(), Value::from(false));
            node.params.insert(arm_idle_key.to_string(), Value::from(false));
        }
    }

    // ── Render ──────────────────────────────────────────────────────────
    // Every chip here is an output: drawn as the device the lean sends to.
    let skin = remapper_out_skin(snarl, node_id, automap_parent);
    publish_picker_skins(ui.ctx(), automap_parent, node_id, (skin, skin));

    // Force a top-down layout in a fixed-width sub-UI so we lay out the
    // same way regardless of parent layout (some pinned-widget contexts
    // hand us a horizontal parent — without this the header + each card
    // would lay out left-to-right instead of stacking).
    const BODY_W: f32 = 380.0;
    ui.allocate_ui_with_layout(
        egui::vec2(BODY_W, 1.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
    ui.set_min_width(BODY_W);

    // Side-scoped `_nav_act_*` flag keys (set by the nav driver on South).
    let lk = if side == "left" { "_nav_act_learn_left" }   else { "_nav_act_learn_right" };
    let ak = if side == "left" { "_nav_act_add_left" }     else { "_nav_act_add_right" };
    let sk = if side == "left" { "_nav_act_special_left" } else { "_nav_act_special_right" };
    let ck = if side == "left" { "_nav_act_clear_left" }   else { "_nav_act_clear_right" };
    // Consume side-scoped one-shot gamepad activation flags. Mirrors the
    // Remapper/Map Action `_nav_act_*` pattern so a controller can drive
    // Learn / Choose / Clear / Add in the Lean sections too. (MIDI… is opened
    // by the nav driver directly, like the Remapper's.)
    let (act_learn, act_add, act_special, act_clear) = {
        let n = snarl.get_node(node_id);
        let g = |k: &str| n.and_then(|n| n.params.get(k)).and_then(|v| v.as_bool()).unwrap_or(false);
        (g(lk), g(ak), g(sk), g(ck))
    };
    if act_learn || act_add || act_special || act_clear {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert(lk.to_string(), Value::from(false));
            node.params.insert(ak.to_string(), Value::from(false));
            node.params.insert(sk.to_string(), Value::from(false));
            node.params.insert(ck.to_string(), Value::from(false));
        }
    }

    // The section's name and its card count, over everything else in it.
    ui.label(egui::RichText::new(format!("{title}  ({})", mappings.len())).small().weak());

    // Status line. Before Learn: prompt to Learn or Choose. During
    // learning: prompt for the chord (or show the draft).
    {
        let blue = Color32::from_rgb(106, 167, 255);
        let green = Color32::from_rgb(127, 201, 127);
        // idle (no draft) → prompt for Learn/Choose; "ready" (Choose-picked
        // draft) → prompt to Add; "learning" handled by the draft preview below.
        if new_phase != "learning" {
            if new_phase == "ready" && !draft.is_empty() {
                ui.label(egui::RichText::new("Picked — click Add (or Learn to add a chord)")
                    .size(crate::settings::ui_size(13.0)).color(green));
            } else {
                let txt = if !wired {
                    "Connect the gyro Device input, then Learn (or Choose… an output)"
                } else {
                    "Press Learn to capture an output, or Choose… one"
                };
                ui.label(egui::RichText::new(txt).size(crate::settings::ui_size(13.0)).color(blue));
            }
        }
    }

    let mut learn_rect = egui::Rect::NOTHING;
    let mut special_rect = egui::Rect::NOTHING;
    let mut clear_rect = egui::Rect::NOTHING;
    let mut add_rect = egui::Rect::NOTHING;
    // A draft exists if the section captured/picked any output, or is mid-learn.
    let has_draft = !new_draft.is_empty()
        || new_phase == "learning" || new_phase == "ready";
    // The section's name and its card count, then the action row the way the Remapper's
    // is: buttons a finger or a pad's highlight can find, sharing the row's width.
    let btn_w = super::remapper_bodies::action_width(ui, (BODY_W - 18.0).min(358.0), 3 + usize::from(has_draft));
    ui.horizontal(|ui| {
        let in_learning = new_phase == "learning";
        let learn_label = if in_learning { "Stop" } else { "Learn" };
        let learn_resp = ui.add_enabled(
            wired || in_learning,
            super::remapper_bodies::action_button(ui, learn_label, btn_w),
        ).on_hover_text(if wired {
            "Capture a gamepad / keyboard chord to fire when leaning this direction"
        } else {
            "Connect the gyro module's Device input to enable gamepad capture\n(keyboard can be captured without a wire once Learn is active)"
        });
        learn_rect = learn_resp.rect;
        if (learn_resp.clicked() || act_learn) && (wired || in_learning) {
            // Coming from "ready" (Choose pins already picked) keeps the draft
            // so a gamepad chord SUMS onto the chosen pins; from idle, start
            // fresh.
            let from_ready = new_phase == "ready";
            if let Some(node) = snarl.get_node_mut(node_id) {
                if in_learning {
                    node.params.insert(phase_key.to_string(), Value::String("idle".to_string()));
                    node.params.insert(armed_key.to_string(), Value::from(false));
                    node.params.insert(arm_idle_key.to_string(), Value::from(false));
                } else {
                    node.params.insert(phase_key.to_string(), Value::String("learning".to_string()));
                    if !from_ready {
                        remapper_write_str_array(node, draft_key, &[]);
                        clear_draft_timing(node, draft_key);
                    }
                    remapper_write_str_array(node, prev_key, &[]);
                    // Arm a one-shot nav capture: arm_idle=false so capture waits
                    // for the Learn press to release before it begins.
                    node.params.insert(armed_key.to_string(), Value::from(true));
                    node.params.insert(arm_idle_key.to_string(), Value::from(false));
                }
            }
        }

        // Choose… — opens the shared picker (mouse OR gamepad South via
        // `_nav_act_special_<side>`) for the output. The picker writes into
        // this section's `_lean_<side>_draft`. Lean inputs are always analog (the
        // lean gesture), so the swipe bindings are always available here.
        {
            let special_btn = ui.add(super::remapper_bodies::action_button(ui, "Choose…", btn_w))
                .on_hover_text("Pick the output from a board instead of pressing it.");
            special_rect = special_btn.rect;
            if special_btn.clicked() || act_special {
                crate::canvas::viewer::request_special_picker(ui.ctx(),
                    crate::canvas::viewer::SpecialPickerRequest {
                        inner: node_id,
                        path: crate::canvas::viewer::subpatch_path(automap_parent),
                        draft_key: draft_key.to_string(),
                        phase_key: Some(phase_key.to_string()),
                        touch_zones: false,
                        exclude_pin_prefix: None,
                        ..Default::default()
                    });
            }
        }

        // Clear button — abandons the captured/picked output and starts over
        // (back to idle, draft emptied, capture disarmed). Shown whenever a draft
        // exists so a botched capture/pick can be reset WITHOUT finishing.
        if has_draft {
            let clear_btn = ui.add(super::remapper_bodies::action_button(ui, "Clear", btn_w));
            clear_rect = clear_btn.rect;
            if clear_btn.clicked() || act_clear {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert(phase_key.to_string(), Value::String("idle".to_string()));
                    remapper_write_str_array(node, draft_key, &[]);
                    clear_draft_timing(node, draft_key);
                    remapper_write_str_array(node, prev_key, &[]);
                    node.params.insert(armed_key.to_string(), Value::from(false));
                    node.params.insert(arm_idle_key.to_string(), Value::from(false));
                }
                new_draft.clear();
                new_phase = "idle".to_string();
            }
        }

        let add_enabled = !new_draft.is_empty()
            && (new_phase == "learning" || new_phase == "ready");
        let add_resp = ui.add_enabled(add_enabled,
            super::remapper_bodies::action_button(ui, "Add", btn_w));
        add_rect = add_resp.rect;
        if (add_resp.clicked() || act_add) && add_enabled {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let out_arr: Vec<Value> = new_draft.iter().map(|s| Value::String(s.clone())).collect();
                let mut entry = serde_json::Map::new();
                entry.insert("out".to_string(), Value::Array(out_arr));
                // Touchpad swipe outputs are continuous → analog mode.
                if new_draft.iter().any(|p| remapper_out_is_swipe(p)) {
                    entry.insert("mode".to_string(), Value::String("analog".to_string()));
                }
                // The output's timing, picked with Choose, goes onto the card.
                take_draft_timing(node, draft_key, &new_draft, &mut entry);
                let mut all = mappings.clone();
                all.push(Value::Object(entry));
                node.params.insert(key.to_string(), Value::Array(all));
                remapper_write_str_array(node, draft_key, &[]);
                clear_draft_timing(node, draft_key);
                remapper_write_str_array(node, prev_key, &[]);
                // Disarm so a held nav press after Add doesn't re-capture.
                node.params.insert(armed_key.to_string(), Value::from(false));
                node.params.insert(arm_idle_key.to_string(), Value::from(false));
                // If we Added a Choose-only pick ("ready"), return to idle (no
                // active capture). From "learning" (gamepad), stay learning so
                // the user can chain more captures with the same Learn session.
                if new_phase == "ready" {
                    node.params.insert(phase_key.to_string(), Value::String("idle".to_string()));
                }
            }
        }
    });
    // Publish action-button rects (global) so the nav driver can glow the
    // focused one. Order MUST match `nav_remap_action_items` for Lean: Learn,
    // Choose, MIDI…, Clear(has_draft), Add((learning||ready) && draft). Use the
    // cards' scope.
    publish_nav_action_rects_scoped(ui, node_id, key,
        &[learn_rect, special_rect, clear_rect, add_rect]);
    remapper_hold_nav_while_capturing(ui.ctx(), snarl, node_id, nav_active_for_device,
        armed_key, phase_key, &["learning"]);

    // Status / draft preview line. During "learning" with an empty draft, prompt
    // for the chord; otherwise (learning or "ready" with a draft) show the
    // captured/picked chord chips so the user sees what Add will commit.
    if new_phase == "learning" && new_draft.is_empty() {
        ui.label(egui::RichText::new("Press a button or combination")
            .size(crate::settings::ui_size(13.0)).color(Color32::from_rgb(106, 167, 255)));
        request_repaint_throttled(ui.ctx());
    } else if (new_phase == "learning" || new_phase == "ready") && !new_draft.is_empty() {
        let chord = draft_timed_chord(snarl.get_node(node_id), draft_key, &new_draft);
        ui.horizontal_wrapped(|ui| {
            remapper_render_timed_chord(ui, &chord, skin);
        });
        if new_phase == "learning" { request_repaint_throttled(ui.ctx()); }
    }

    // ── Mapping cards ───────────────────────────────────────────────────
    // Pinned, on a layer of their own that scrolls under the rows above (`mapping_list_ui`).
    let mut list_holder = mapping_list_ui(ui, node_id);
    let ui: &mut egui::Ui = match list_holder.as_mut() {
        Some(list) => list,
        None => ui,
    };
    if mappings.is_empty() { return; }

    // Filter row. Lean cards capture OUTPUTS (the lean direction is the
    // trigger), so the filter matches the live-pressed input against each
    // card's assigned outputs. The grouped chip also catches analog
    // destinations (stick axes/cardinals) on the output side. Read SOURCE pins
    // only (the upstream device) — not OS KB/M — so injected output keys don't
    // flicker the filter.
    let filter_live: Vec<String> = remapper_upstream_pressed_now(ui, live_signals, &upstream_devs);
    let filter = mapping_filter_row(
        ui,
        egui::Id::new(("fxi_lean_filter", node_id.0, side)),
        &format!("({})", mappings.len()),
        &filter_live,
        skin,
    );

    let mut to_remove: Option<usize> = None;
    let mut to_update: Option<(usize, serde_json::Map<String, Value>)> = None;
    ui.spacing_mut().item_spacing.y = 2.0;
    let reorder_enabled = filter.kind == MapFilterKind::All;
    // Live lean magnitude for THIS side (gyro output slot 3, exported to the
    // UI via last_out) — the preview dot on any open card curve editor.
    let lean_live: Option<f32> = snarl.get_node(node_id)
        .and_then(|n| n.extra.last_out.get(3).copied().flatten())
        .map(|s| s.as_float())
        .map(|v| if side == "left" { (-v).max(0.0) } else { v.max(0.0) });
    // Output-conflict scan (once): a lean card whose out pin is also driven by
    // another card in this patch gets a ⚠ badge.
    let conflicts = scan_mapping_conflicts(snarl);
    let mut rv = ReorderView::begin(
        ui, egui::Id::new(("fxi_lean_reorder", node_id.0, side)), reorder_enabled,
    );
    let mut slot = 0usize;
    for (i, m) in mappings.iter().enumerate() {
        let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        if !mapping_passes_filter(&filter, &out_pins) { continue; }
        let card_conf = card_conflict_for(&conflicts, node_id, key, i, &out_pins);

        if let Some(h) = rv.gap_before(slot) { draw_insertion_gap(ui, h); }

        let mut working: serde_json::Map<String, Value> = m.as_object().cloned().unwrap_or_default();
        let mut working_changed = false;
        let drag_off = rv.offset_for(i);
        ui.push_id(("fxi_lean_card", node_id.0, side, i), |ui| {
            // Cap card width at 358 (Figma design width) so the painter's
            // scale factor `s` stays = 1.0. Above that, all painter-drawn
            // text + pills scale up while built-in widgets (DragValue)
            // keep their default size, so the time-gap value box ends up
            // looking oversized relative to the labels.
            ui.allocate_ui_with_layout(
                egui::vec2(358.0_f32.min(ui.available_width()), 1.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    // Its one row is its outputs (the lean is its trigger): a
                    // click opens the picker on them.
                    let picker_path = crate::canvas::viewer::subpatch_path(automap_parent);
                    let row_edit = RowEdit::one_row(crate::kbm_picker::PickStage::Output,
                        picker_card_edit_side(ui.ctx(), &picker_path, node_id, key, i));
                    let result = remapper_mapping_card_pixel(
                        ui, node_id, i, &mut working,
                        &out_pins, None, skin, skin, true, false,
                        reorder_enabled, drag_off, key, true,
                        card_conf.as_ref(), Some(row_edit),
                    );
                    if result.delete_clicked { to_remove = Some(i); }
                    if result.changed { working_changed = true; }
                    if result.row_clicked.is_some() {
                        request_special_picker(ui.ctx(),
                            SpecialPickerRequest::lean_card(node_id, picker_path, key, i));
                    }
                    // A MIDI chip. A Lean card draws its OUTPUTS in the card's one
                    // row (the one the renderer calls "in"), so the chip is an
                    // output's: its channel and levels.
                    if let Some((_, pin_idx)) = result.midi_chip_clicked {
                        crate::canvas::viewer::request_midi_modal(ui.ctx(), crate::canvas::viewer::MidiModalRequest {
                            inner: node_id,
                            path: crate::canvas::viewer::subpatch_path(automap_parent),
                            purpose: crate::canvas::viewer::MidiModalPurpose::Chip {
                                card: i,
                                cards_key: key.to_string(),
                                side_out: true,
                                pin_idx,
                            },
                        });
                    }
                    rv.observe(i, &result);
                    // Per-card response curve + manual activation threshold —
                    // the lean gesture is inherently analog, so every card
                    // qualifies. A card threshold replaces the node-level
                    // lean threshold for that card.
                    let nav_uid = curve_nav_uid(ui.ctx(), node_id, key, i);
                    if mapping_card_curve_section(
                        ui, node_id, key, i, &mut working,
                        true, lean_live, None, nav_uid, None,
                    ) {
                        working_changed = true;
                    }
                },
            );
        });
        if working_changed { to_update = Some((i, working)); }
        slot += 1;
    }
    if let Some(h) = rv.gap_after_last(slot) { draw_insertion_gap(ui, h); }
    let reorder = rv.finish(ui);
    if let Some((from, to)) = reorder {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(Value::Array(arr)) = node.params.get_mut(key) {
                reorder_array(arr, from, to);
            }
        }
    }
    if let Some(idx) = to_remove {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(Value::Array(arr)) = node.params.get_mut(key) {
                if idx < arr.len() { arr.remove(idx); }
            }
        }
    }
    if let Some((i, obj)) = to_update {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(Value::Array(arr)) = node.params.get_mut(key) {
                if let Some(slot) = arr.get_mut(i) { *slot = Value::Object(obj); }
            }
        }
    }
        }, // close allocate_ui_with_layout closure
    );
}
