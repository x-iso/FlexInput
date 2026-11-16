//! The 3D viewport in the 3DOF to 2D module: what the module thinks the pad is doing, and
//! where that ends up in 2D.
//!
//! The 3D model viewer on the canvas is for a stream or an overlay - it shows a pad to
//! somebody watching. This one is for the person holding the pad: it answers "which way does
//! FlexInput think my controller is pointed right now, and which way does that push the 2D
//! output with the settings I have?", so trial and error over the pointer's behaviour turns
//! into reading a picture. It draws the pose as a gimbal of the pad's own axes - the three
//! rings they sweep, the neutral hold as a ghost, the Edge-turn zone as a ring - and beside
//! it the output plane, with one arrow per turn rate pointing where that rate drives the
//! cursor under the current mode and invert settings, and the live output on top.
//!
//! A pad model can be drawn in place of the gimbal, on the same mesh, palette and quaternion
//! as the viewer's. The axes are the default because a model is hard to read at 90 points
//! across, and the axes are the thing the module projects.
//!
//! Everything is painted rather than allocated, apart from the one rect the caller reserves:
//! a display that sizes itself from its own content is a module that resizes every time the
//! pad moves. The pane is the module's `kit::Surface`, so the corner grip sizes it and the
//! size rides in the patch with the rest of the module.

use super::*;

/// The smallest viewport worth drawing: under this the gimbal is a smear of lines and the
/// output plane has no room for an arrowhead.
pub(crate) const GYRO_VIEW_MIN: egui::Vec2 = egui::vec2(112.0, 84.0);

/// The three axis colours, held across both panes. A colour belongs to a *rate* - the turn
/// about an axis - and the axis drawn in it is the axis that rate turns about, so the yaw
/// arrow in the output plane and the pad's up axis in the gimbal are the same colour and the
/// pair reads as one thing.
#[derive(Clone, Copy)]
pub(crate) struct GyroAxes {
    pub yaw: egui::Color32,
    pub pitch: egui::Color32,
    pub roll: egui::Color32,
}

impl GyroAxes {
    /// Chosen to be told apart at ten points across on a dark pane, with the selection
    /// colour for yaw so the module's most-used rate matches how the canvas marks a choice.
    pub(crate) fn of(visuals: &egui::Visuals) -> Self {
        GyroAxes {
            yaw: visuals.selection.bg_fill,
            pitch: egui::Color32::from_rgb(124, 201, 128),
            roll: egui::Color32::from_rgb(255, 196, 90),
        }
    }

    fn rate(self, r: Rate) -> egui::Color32 {
        match r {
            Rate::Yaw => self.yaw,
            Rate::Pitch => self.pitch,
            Rate::Roll => self.roll,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Rate {
    /// About the pad's up axis.
    Yaw,
    /// About the pad's left axis.
    Pitch,
    /// About the pad's forward axis.
    Roll,
}

/// How the pad's three turn rates reach the two output axes - the module's own projection,
/// as `compute_gyro_3dof` works it, in the picture's units. `roll_share` is the fraction of
/// roll joining the horizontal output (`roll_contribution`, shown as a percent); the invert
/// flags are applied to the outputs, which is the same as flipping the arrow.
#[derive(Clone, Copy)]
pub(crate) struct GyroProjection {
    /// `pitch_yaw`, `pitch_roll`, `player`, `world`.
    pub axis: &'static str,
    pub roll_share: f32,
    /// `inv_yaw`, `inv_pitch`, `inv_roll`.
    pub inv: [bool; 3],
}

impl GyroProjection {
    /// Which way a unit turn about one axis drives the output, before the output's own
    /// invert flag. Zero when the mode doesn't read that rate at all.
    ///
    /// The signs are the engine's, including the one that looks like a mistake: in
    /// Pitch+Yaw the roll share is *subtracted* (`hgz - hgx * roll_share`), because the
    /// setting exists to put back the share of a body turn that a tilted hold loses to roll,
    /// and that goes the other way round. Pitch+Roll reads the roll rate itself, so there it
    /// is positive.
    fn share(&self, rate: Rate) -> egui::Vec2 {
        match (rate, self.axis) {
            // Yaw - the turn about the pad's up axis - is the sideways output, except in
            // Pitch+Roll, which puts roll there instead and reads no yaw at all.
            (Rate::Yaw, "pitch_roll") => egui::Vec2::ZERO,
            // Player and World take the sideways output from the turn about gravity: the
            // pad's own yaw is what's left of that once the pad is level, so it still maps
            // to X here, which is what the plane says.
            (Rate::Yaw, _) => egui::Vec2::X,
            (Rate::Pitch, _) => egui::Vec2::Y,
            (Rate::Roll, "pitch_roll") => egui::Vec2::X,
            (Rate::Roll, "pitch_yaw") => egui::Vec2::X * -self.roll_share,
            (Rate::Roll, _) => egui::Vec2::ZERO,
        }
    }

    /// Does this mode read the rate at all? A zero-length arrow reads as a setting doing
    /// nothing better than a full one whose value happens to be zero.
    fn reads(&self, rate: Rate) -> bool {
        self.share(rate) != egui::Vec2::ZERO
    }
}

/// What the viewport draws this frame.
pub(crate) struct GyroView<'a> {
    pub axes: GyroAxes,
    /// The module's Orientation output: the pose, in the canonical (forward, left, up)
    /// frame. `None` when nothing is published, which draws the pad level and says so.
    pub pose: Option<glam::Quat>,
    /// The neutral hold, as the module applies it: the ghost the pose is measured against.
    pub hold: glam::Quat,
    /// The Edge-turn zone in degrees off neutral, or `None` while Edge turn is off.
    pub zone: Option<f32>,
    pub proj: GyroProjection,
    /// The model folder to draw, half see-through, where the pad icon goes.
    pub model: Option<&'a str>,
    /// The pad's own icon (its device card's), and whether it is drawn as an outline.
    pub pad_svg: Option<&'static [u8]>,
    pub outline: bool,
    /// The Lean output, and the threshold past which Lean! fires.
    pub lean: f32,
    pub lean_threshold: f32,
}

/// The orbit camera, per node in egui's temp memory: which way the picture looks at the pad.
/// Deliberately not a patch param - it says how you are looking, not what the module does,
/// and a patch that carried it would open every 3DOF module pointed the same odd way.
#[derive(Clone, Copy)]
struct Cam {
    yaw: f32,
    pitch: f32,
}

/// Behind the pad, a little to its right and above: over the shoulder of the person holding
/// it, so a turn to the right moves the pointer to the right.
const CAM_DEFAULT: Cam = Cam { yaw: std::f32::consts::PI + 0.55, pitch: 0.46 };
/// The camera yaw straight behind the pad, the view the 3D model renderer's own camera has:
/// the model is turned by how far the orbit is from it.
const CAM_BEHIND: f32 = std::f32::consts::PI;

fn cam_key(node: NodeId) -> egui::Id {
    egui::Id::new(("gyro_view_cam", node.0))
}

fn cam(ctx: &egui::Context, node: NodeId) -> Cam {
    let [yaw, pitch] = ctx
        .data(|d| d.get_temp::<[f32; 2]>(cam_key(node)))
        .unwrap_or([CAM_DEFAULT.yaw, CAM_DEFAULT.pitch]);
    Cam { yaw, pitch }
}

fn set_cam(ctx: &egui::Context, node: NodeId, c: Cam) {
    ctx.data_mut(|d| d.insert_temp(cam_key(node), [c.yaw, c.pitch]));
}

/// Draw the viewport into `rect`, which the caller has already reserved, and orbit it under
/// the pointer.
pub(crate) fn draw_gyro_view(ui: &mut egui::Ui, node: NodeId, rect: egui::Rect, v: &GyroView) {
    if !rect.is_positive() {
        return;
    }
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);

    // The whole pane is the sphere: the output's own numbers are a wire away, on any scope.
    // With a model to draw, it stands where the pad icon would, half see-through, the
    // sphere's marks over it.
    let c = cam(ui.ctx(), node);
    if let Some(model) = v.model {
        let r = sphere_radius(rect) * 1.15;
        draw_model(ui, node, egui::Rect::from_center_size(rect.center(), egui::vec2(r, r) * 2.0), v, model, c);
    }
    draw_gimbal(ui.ctx(), &painter, rect, v, c, &visuals);

    // Orbiting: the viewport answers the drag across its whole area, because nothing in it
    // is a control. A display you can't turn is a display that hides half of what it shows.
    let resp = ui.interact(rect, egui::Id::new(("gyro_view_orbit", node.0)), egui::Sense::click_and_drag());
    let mut moved = false;
    if resp.dragged() {
        let mut c = cam(ui.ctx(), node);
        c.yaw += resp.drag_delta().x * 0.01;
        c.pitch = (c.pitch + resp.drag_delta().y * 0.01).clamp(-1.45, 1.45);
        set_cam(ui.ctx(), node, c);
        moved = true;
    }
    if resp.double_clicked() {
        set_cam(ui.ctx(), node, CAM_DEFAULT);
        moved = true;
    }
    resp.on_hover_text(
        "How this module sees the pad, and where that puts the 2D output.\n\
         Drag to look round the pad; double-click puts the view back.\n\
         \n\
         The white pointer runs from the pad to the point on the sphere the output lands on.\n\
         The heading ring is the horizontal, in degrees: the arc along it from forward is\n\
         the turn that becomes X, the arc up from there the tilt that becomes Y. With roll\n\
         let into X (roll mix, or Pitch+Roll) the pointer pivots along the yellow arc from\n\
         the yellow dot - where it would be without roll - to where it lands. The arc under\n\
         the sphere is Lean, its ticks the threshold. The ghost is the neutral hold; the\n\
         dashed ring is where Edge turn takes over.",
    );
    if moved || v.pose.is_some() {
        // A live pose means the picture follows the pad, so the frames have to keep coming.
        ui.ctx().request_repaint();
    }
}

/// The sphere's radius in a pane.
fn sphere_radius(rect: egui::Rect) -> f32 {
    (rect.width() * 0.40).min(rect.height() * 0.36).max(8.0)
}

/// The camera basis. Canonical world axes are (forward, left, up) - the frame the
/// Orientation output lives in - and the camera orbits that at a fixed distance with an
/// orthographic projection, so a turn of the pad is a turn of the picture with no
/// perspective making a big angle look small.
fn view_basis(c: Cam) -> (glam::Vec3, glam::Vec3) {
    let (sy, cy) = c.yaw.sin_cos();
    let (sp, cp) = c.pitch.sin_cos();
    // `towards` points from the pad to the camera; the camera looks the other way, and the
    // screen's right is that look crossed with up. (It was `towards × up`, which is the
    // screen's LEFT: every pose was drawn mirrored, a turn right going left.)
    let towards = glam::Vec3::new(cy * cp, sy * cp, sp);
    let look = -towards;
    let right = look.cross(glam::Vec3::Z).normalize_or_zero();
    let up = right.cross(look);
    (right, up)
}

/// Where the pad's nose points, read off its pose as the module's projection reads it - in
/// degrees, right, up and right-grip-down positive - and where that puts the horizontal
/// output once roll has had its say (`effective`): the yaw itself in Pitch+Yaw with no roll
/// mix, the yaw less the roll's share with one (the engine's `x = yaw - roll * share`), the
/// roll alone in Pitch+Roll, and the turn about gravity in Player and World.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Heading {
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub effective: f32,
    /// Where the output lands, as a direction on the sphere: `effective` and the pitch, each
    /// turned round by its output's invert flag - the white pointer.
    pub out_x: f32,
    pub out_y: f32,
    /// The same without roll's share - the yellow dot the pointer pivots away from.
    pub base_x: f32,
}

pub(crate) fn heading_of(pose: glam::Quat, proj: &GyroProjection) -> Heading {
    // The canonical frame is (forward, left, up): a nose to the right has a negative left
    // component, and a right grip down lifts the pad's left axis.
    let nose = pose * glam::Vec3::X;
    let yaw = (-nose.y).atan2(nose.x).to_degrees();
    let pitch = nose.z.clamp(-1.0, 1.0).asin().to_degrees();
    let left = pose * glam::Vec3::Y;
    let up = pose * glam::Vec3::Z;
    let roll = left.z.atan2(up.z).to_degrees();
    let (base, effective) = match proj.axis {
        "pitch_roll" => (0.0, roll),
        "pitch_yaw" => (yaw, yaw - roll * proj.roll_share),
        _ => (yaw, yaw),
    };
    let sx = if proj.inv[0] { -1.0 } else { 1.0 };
    let sy = if proj.inv[1] { -1.0 } else { 1.0 };
    Heading { yaw, pitch, roll, effective, out_x: effective * sx, out_y: pitch * sy, base_x: base * sx }
}

/// A direction on the heading ring - the world's horizontal - `deg` right of forward.
fn on_heading(deg: f32) -> glam::Vec3 {
    let r = deg.to_radians();
    glam::Vec3::new(r.cos(), -r.sin(), 0.0)
}

/// The reference sphere: a pointer from the pad's centre along its nose to the point it
/// hits on a sphere of directions, marked off in degrees, with the two arcs that are the 2D
/// output - along the heading ring from forward (the yaw, X) and up the meridian to the hit
/// (the pitch, Y). When the mode lets roll into X (a roll mix in Pitch+Yaw, all of it in
/// Pitch+Roll) the heading ring fans out from the nose's heading to the heading the output
/// actually takes, with a pointer of its own there - roll engaging turns that pointer round
/// the gauge, off the nose. The pad sits small in the middle, its neutral hold as a ghost,
/// and the Edge-turn zone is the dashed ring round forward.
fn draw_gimbal(ctx: &egui::Context, painter: &egui::Painter, rect: egui::Rect, v: &GyroView, c: Cam, visuals: &egui::Visuals) {
    let weak = visuals.weak_text_color();
    let text = visuals.text_color();
    let strong = visuals.strong_text_color();
    let (right, up) = view_basis(c);
    let towards = up.cross(right);
    let centre = rect.center();
    let radius = sphere_radius(rect);
    let at = |p: glam::Vec3| egui::pos2(centre.x + p.dot(right) * radius, centre.y - p.dot(up) * radius);
    // The sphere's own lines are fainter on its far side, so it reads as a ball. What the
    // module does - the arcs and the pointer - is drawn at full strength wherever it is: from
    // behind the pad, forward is the far side, and that is where everything happens.
    let grid_path = |pts: &[glam::Vec3], col: egui::Color32, w: f32| {
        for pair in pts.windows(2) {
            let near = (pair[0] + pair[1]).dot(towards) >= 0.0;
            let c = if near { col } else { col.gamma_multiply(0.5) };
            painter.line_segment([at(pair[0]), at(pair[1])], egui::Stroke::new(w, c));
        }
    };
    let path = |pts: &[glam::Vec3], col: egui::Color32, w: f32| {
        let pts: Vec<egui::Pos2> = pts.iter().map(|p| at(*p)).collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(w, col)));
    };
    let circle = |a: glam::Vec3, b: glam::Vec3, from: f32, to: f32, n: usize| -> Vec<glam::Vec3> {
        (0..=n)
            .map(|k| {
                let t = (from + (to - from) * k as f32 / n as f32).to_radians();
                a * t.cos() + b * t.sin()
            })
            .collect()
    };
    // A direction `x` degrees right of forward and `y` up.
    let dir = |x: f32, y: f32| on_heading(x) * y.to_radians().cos() + glam::Vec3::Z * y.to_radians().sin();
    // An arc along a latitude, from heading `from` to `to`.
    let along = |lat: f32, from: f32, to: f32| -> Vec<glam::Vec3> {
        let n = (((to - from).abs() / 3.0).ceil() as usize).clamp(2, 120);
        (0..=n).map(|k| dir(from + (to - from) * k as f32 / n as f32, lat)).collect()
    };

    let pose = v.pose.unwrap_or(glam::Quat::IDENTITY);
    let h = heading_of(pose, &v.proj);
    let yaw_col = v.axes.rate(Rate::Yaw);
    let pitch_col = v.axes.rate(Rate::Pitch);
    let roll_col = v.axes.rate(Rate::Roll);
    let lean_col = egui::Color32::from_rgb(190, 140, 255);
    let small = egui::FontId::proportional(9.0);

    // ── The sphere: its outline, latitudes every 30°, meridians every 45°, and the heading
    //    ring (the equator) marked every 15° and numbered every 45°.
    painter.circle_stroke(centre, radius, egui::Stroke::new(0.8_f32, weak.gamma_multiply(0.35)));
    let grid = weak.gamma_multiply(0.35);
    for lat in [-60.0_f32, -30.0, 30.0, 60.0] {
        grid_path(&along(lat, 0.0, 360.0), grid, 0.6);
    }
    for m in 0..4 {
        grid_path(&circle(on_heading(m as f32 * 45.0), glam::Vec3::Z, 0.0, 360.0, 72), grid, 0.6);
    }
    grid_path(&along(0.0, 0.0, 360.0), weak.gamma_multiply(0.8), 1.0);
    for k in 0..24 {
        let deg = k as f32 * 15.0;
        let d = on_heading(deg);
        let major = k % 3 == 0;
        let near = d.dot(towards) >= -0.1;
        let col = if near { weak } else { weak.gamma_multiply(0.7) };
        painter.line_segment([at(d), at(d * if major { 1.09 } else { 1.05 })], egui::Stroke::new(1.0_f32, col));
        // Numbered round the front, where the output lands, and at the back straight behind.
        if major && radius > 40.0 && (deg <= 135.0 || deg >= 225.0 || deg == 180.0) {
            let shown = if deg > 180.0 { deg - 360.0 } else { deg };
            painter.text(at(d * 1.2), egui::Align2::CENTER_CENTER, format!("{shown:+.0}"), small.clone(), col);
        }
    }

    // ── The pad in the middle: the neutral hold as a ghost, and where it is now - a gamepad
    //    lying in its own frame. With a model, the model is the pad.
    // The hold is stored as a rotation in the gyro's (forward, right, down) basis; the frame
    // drawn here is canonical (forward, left, up), which is that basis with two axes flipped
    // - itself a half turn about forward, hence the sandwich.
    let flip = glam::Quat::from_rotation_x(std::f32::consts::PI);
    let hold = flip * v.hold * flip;
    let at_pad = |p: glam::Vec3| at(p * 0.55);
    // With a model, the model is the pad: no icon at all, the neutral hold's ghost included.
    if v.model.is_none() {
        let svg = v.pad_svg.unwrap_or_else(super::glyphs::gamepad_svg);
        pad_icon(ctx, painter, &at_pad, hold, weak.gamma_multiply(0.35), svg, v.outline);
        pad_icon(ctx, painter, &at_pad, pose, text.gamma_multiply(0.9), svg, v.outline);
    }

    // ── The Edge-turn zone, on the sphere round forward.
    if let Some(zone) = v.zone {
        zone_ring(painter, &at, zone.to_radians(), roll_col);
    }

    // ── Where the output lands, on the sphere: along the heading ring from forward (X),
    //    and up the meridian from there (Y).
    path(&along(0.0, 0.0, h.out_x), yaw_col, 2.2);
    if v.proj.reads(Rate::Pitch) {
        let foot = on_heading(h.out_x);
        path(&circle(foot, glam::Vec3::Z, 0.0, h.out_y, 32), pitch_col, 2.2);
    }

    // ── Roll into X: the arc the pointer pivots along, behind it, from where it would be
    //    without roll (the yellow dot) to where it lands.
    let roll_in = (v.proj.axis == "pitch_yaw" && v.proj.roll_share.abs() > 1e-3) || v.proj.axis == "pitch_roll";
    if roll_in && (h.out_x - h.base_x).abs() > 0.3 {
        path(&along(h.out_y, h.base_x, h.out_x), roll_col, 2.4);
    }
    if roll_in {
        painter.circle_filled(at(dir(h.base_x, h.out_y)), 3.6, roll_col);
    }

    // ── The pointer: from the pad to where the output lands.
    let tip = dir(h.out_x, h.out_y);
    let pointer_col = if tip.dot(towards) >= -0.05 { strong } else { strong.gamma_multiply(0.5) };
    painter.line_segment([centre, at(tip)], egui::Stroke::new(1.6_f32, pointer_col));
    painter.circle_filled(at(tip), 3.6, pointer_col);

    // ── Lean: an arc under the sphere, from straight down out to the side the pad leans, its
    //    ticks the threshold either way.
    let lean_r = radius * 1.12;
    let lean_at = |deg: f32| {
        // 0 is straight down; positive leans right.
        let a = (90.0 - deg).to_radians();
        egui::pos2(centre.x + a.cos() * lean_r, centre.y + a.sin() * lean_r)
    };
    let lean_deg = |v: f32| v.clamp(-1.0, 1.0).asin().to_degrees();
    let arc = |from: f32, to: f32, col: egui::Color32, w: f32| {
        let n = (((to - from).abs() / 3.0).ceil() as usize).clamp(2, 60);
        let pts: Vec<egui::Pos2> = (0..=n).map(|k| lean_at(from + (to - from) * k as f32 / n as f32)).collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(w, col)));
    };
    arc(-60.0, 60.0, weak.gamma_multiply(0.35), 1.0);
    let thr = lean_deg(v.lean_threshold.abs());
    for t in [-thr, thr] {
        let a = (90.0 - t).to_radians();
        let d = egui::vec2(a.cos(), a.sin());
        painter.line_segment([centre + d * (lean_r - 4.0), centre + d * (lean_r + 4.0)], egui::Stroke::new(1.0_f32, lean_col.gamma_multiply(0.7)));
    }
    let lv = lean_deg(v.lean).clamp(-60.0, 60.0);
    let lit = v.lean.abs() >= v.lean_threshold.abs();
    arc(0.0, lv, if lit { lean_col } else { lean_col.gamma_multiply(0.6) }, 2.4);

    // ── The legend: what each mark is, top left - only the ones drawn.
    if radius > 36.0 {
        let mut y = rect.top() + 6.0;
        let x = rect.left() + 6.0;
        let mut row = |swatch: &dyn Fn(egui::Pos2), label: &str| {
            swatch(egui::pos2(x + 5.0, y + 5.0));
            painter.text(egui::pos2(x + 14.0, y + 5.0), egui::Align2::LEFT_CENTER, label, small.clone(), weak);
            y += 12.0;
        };
        let dot = |col: egui::Color32| move |p: egui::Pos2| {
            painter.circle_filled(p, 3.0, col);
        };
        let line = |col: egui::Color32| move |p: egui::Pos2| {
            painter.line_segment([p - egui::vec2(5.0, 0.0), p + egui::vec2(5.0, 0.0)], egui::Stroke::new(2.2_f32, col));
        };
        row(&dot(strong), "output");
        row(&line(yaw_col), "turn → X");
        if v.proj.reads(Rate::Pitch) {
            row(&line(pitch_col), "tilt → Y");
        }
        if roll_in {
            row(&dot(roll_col), "without roll");
            row(&line(roll_col), "roll into X");
        }
        row(&line(lean_col), "lean");
    }

    // The angles, along the bottom.
    if radius > 40.0 {
        let label = if roll_in {
            format!("yaw {:+.0}°  pitch {:+.0}°  roll {:+.0}°  →  x {:+.0}°  y {:+.0}°", h.yaw, h.pitch, h.roll, h.out_x, h.out_y)
        } else {
            format!("yaw {:+.0}°  pitch {:+.0}°  →  x {:+.0}°  y {:+.0}°", h.yaw, h.pitch, h.out_x, h.out_y)
        };
        painter.text(egui::pos2(rect.left() + 6.0, rect.bottom() - 4.0), egui::Align2::LEFT_BOTTOM, label, small.clone(), weak);
        painter.text(
            egui::pos2(rect.right() - 6.0, rect.bottom() - 4.0),
            egui::Align2::RIGHT_BOTTOM,
            format!("lean {:+.2}", v.lean),
            small,
            if lit { lean_col } else { weak },
        );
    }

    // Past the Edge-turn zone the camera keeps turning on its own: say so on the nose.
    let nose = pose * glam::Vec3::X;
    if v.zone.map_or(false, |z| nose.angle_between(glam::Vec3::X).to_degrees() > z) {
        painter.circle_stroke(at(nose), 5.0, egui::Stroke::new(1.4_f32, roll_col));
    }

    if v.pose.is_none() {
        painter.text(
            egui::pos2(centre.x, rect.top() + 8.0),
            egui::Align2::CENTER_TOP,
            "no Orientation reading",
            egui::FontId::proportional(10.0),
            weak,
        );
    }
}

/// The pad's icon lying in frame `q`: its triggers toward forward, its right grip to the
/// pad's right - the pad you are holding, drawn as its device card draws it, filled or as an
/// outline. Orthographic, so the quad it lies on is drawn exactly as two triangles.
fn pad_icon(
    ctx: &egui::Context,
    painter: &egui::Painter,
    at: &impl Fn(glam::Vec3) -> egui::Pos2,
    q: glam::Quat,
    tint: egui::Color32,
    svg: &'static [u8],
    outline: bool,
) {
    let Some((tex, aspect)) = super::glyphs::svg_icon(ctx, svg, outline) else {
        pad_outline(painter, at, q, tint, 1.2_f32);
        return;
    };
    // Across the grips the pad is 1.24 units; nose to back follows the icon's proportions.
    let half_w = 0.62;
    let half_h = half_w / aspect.max(0.1);
    // Image corners: top-left is forward-left, and so on round.
    let corners = [
        (glam::Vec3::new(half_h, half_w, 0.0), egui::pos2(0.0, 0.0)),
        (glam::Vec3::new(half_h, -half_w, 0.0), egui::pos2(1.0, 0.0)),
        (glam::Vec3::new(-half_h, -half_w, 0.0), egui::pos2(1.0, 1.0)),
        (glam::Vec3::new(-half_h, half_w, 0.0), egui::pos2(0.0, 1.0)),
    ];
    let mut mesh = egui::Mesh::with_texture(tex);
    for (p, uv) in corners {
        mesh.vertices.push(egui::epaint::Vertex { pos: at(q * p), uv, color: tint });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// The Edge-turn zone: every nose direction `zone` radians off neutral, as a ring on the
/// sphere of directions.
fn zone_ring(painter: &egui::Painter, at: &impl Fn(glam::Vec3) -> egui::Pos2, zone: f32, col: egui::Color32) {
    let a = any_perp(glam::Vec3::X);
    let b = glam::Vec3::X.cross(a);
    let (c, s) = (zone.cos(), zone.sin());
    let pts: Vec<egui::Pos2> = (0..=40)
        .map(|k| {
            let t = k as f32 / 40.0 * std::f32::consts::TAU;
            at(glam::Vec3::X * c + (a * t.cos() + b * t.sin()) * s)
        })
        .collect();
    painter.extend(egui::Shape::dashed_line(
        &pts,
        egui::Stroke::new(1.2_f32, col.gamma_multiply(0.85)),
        4.0,
        1.1,
    ));
}

/// A gamepad's outline in the frame `q` puts it in: long across the grips, short nose to
/// back, drawn as a polygon so it reads at thirty points across. The two sticks and the
/// nose mark are there for one reason - a plain blob looks the same upside down.
fn pad_outline(painter: &egui::Painter, at: &impl Fn(glam::Vec3) -> egui::Pos2, q: glam::Quat, col: egui::Color32, w: f32) {
    // Half-extents in pad units, and an exponent under 1 to square the outline off into a
    // rounded rectangle rather than an ellipse: a controller's silhouette is corners and
    // straight runs, and an ellipse reads as a plate.
    const X: f32 = 0.46;
    const Y: f32 = 0.62;
    let corner = |t: f32| t.abs().powf(0.62) * t.signum();
    let pts: Vec<egui::Pos2> = (0..=48)
        .map(|k| {
            let t = k as f32 / 48.0 * std::f32::consts::TAU;
            at(q * glam::Vec3::new(X * corner(t.cos()), Y * corner(t.sin()), 0.0))
        })
        .collect();
    painter.add(egui::Shape::line(pts, egui::Stroke::new(w, col)));

    let dot = |px: f32, py: f32, r: f32| {
        painter.circle_filled(at(q * glam::Vec3::new(px, py, 0.0)), r, col);
    };
    // Sticks a little in from the corners, and a mark on the far edge: with two round dots
    // and one notch the outline can't be read turned round.
    dot(0.20, 0.26, 2.2);
    dot(0.20, -0.26, 2.2);
    painter.line_segment(
        [at(q * glam::Vec3::new(X - 0.06, 0.10, 0.0)), at(q * glam::Vec3::new(X - 0.06, -0.10, 0.0))],
        egui::Stroke::new(w, col),
    );
}

/// Any unit vector at right angles to `v`, for building a circle's basis.
fn any_perp(v: glam::Vec3) -> glam::Vec3 {
    let a = if v.dot(glam::Vec3::X).abs() < 0.9 { glam::Vec3::X } else { glam::Vec3::Y };
    v.cross(a).normalize_or_zero()
}

/// The pad model, at the module's pose: the same mesh, palette and view basis as the 3D
/// viewer's, since the two are looking at one quaternion.
fn draw_model(ui: &mut egui::Ui, node: NodeId, rect: egui::Rect, v: &GyroView, model: &str, c: Cam) {
    // The renderer's camera sits straight behind the pad; turn the pad by how far round the
    // orbit is from there, so the model and the sphere drawn over it agree.
    let q = glam::Quat::from_rotation_z(-(c.yaw - CAM_BEHIND)) * v.pose.unwrap_or(glam::Quat::IDENTITY);
    let (_, outline, outline_w, _) = controller3d_style(None);
    let bg = egui::Color32::TRANSPARENT;
    let rgb = crate::model::default_scheme(model);
    let mut scheme = [[0.0f32; 4]; crate::model::N_GROUPS];
    for (o, i) in scheme.iter_mut().zip(rgb.iter()) {
        *o = [i[0] as f32 / 255.0, i[1] as f32 / 255.0, i[2] as f32 / 255.0, i[3] as f32 / 255.0];
    }
    render_controller3d_core(
        ui,
        // The callback's buffers are shared by every callback in a frame, so this has to
        // differ from the 3D viewer node's own id when both are on the canvas.
        (node.0 as u64) << 8 | 1,
        rect,
        model,
        to_view_basis(q.normalize()),
        bg,
        outline,
        outline_w,
        scheme,
        0.5,
        c.pitch,
        crate::model::ControllerLive::default(),
        0.0,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(axis: &'static str, roll_share: f32, inv: [bool; 3]) -> GyroProjection {
        GyroProjection { axis, roll_share, inv }
    }

    #[test]
    fn each_mode_reads_the_rates_it_projects() {
        assert!(proj("pitch_yaw", 0.0, [false; 3]).reads(Rate::Yaw));
        assert!(!proj("pitch_yaw", 0.0, [false; 3]).reads(Rate::Roll), "no roll share, no roll");
        assert!(proj("pitch_yaw", 0.5, [false; 3]).reads(Rate::Roll));
        assert!(proj("pitch_roll", 0.0, [false; 3]).reads(Rate::Roll));
        assert!(!proj("pitch_roll", 0.0, [false; 3]).reads(Rate::Yaw), "Pitch+Roll reads no yaw");
        for axis in ["player", "world"] {
            assert!(!proj(axis, 0.5, [false; 3]).reads(Rate::Roll), "{axis}");
        }
    }

    // The camera's screen-right is the pad's right when looking from behind it: a turn to the
    // right moves the pointer right on the screen. The basis was mirrored once.
    #[test]
    fn from_behind_the_pad_its_right_is_the_screens_right() {
        let (right, up) = view_basis(Cam { yaw: CAM_BEHIND, pitch: 0.0 });
        assert!((right - glam::Vec3::NEG_Y).length() < 1e-4, "{right:?}");
        assert!((up - glam::Vec3::Z).length() < 1e-4, "{up:?}");
        let turned_right = on_heading(30.0);
        assert!(turned_right.dot(right) > 0.0);
    }

    // The pointer's reading: right, up and right-grip-down positive, and roll reaching X as
    // the engine lets it - its share subtracted in Pitch+Yaw, all of X in Pitch+Roll.
    #[test]
    fn the_heading_reads_the_pose_and_lets_roll_in_as_the_mode_does() {
        let near = |a: f32, b: f32| (a - b).abs() < 0.05;
        let level = heading_of(glam::Quat::IDENTITY, &proj("pitch_yaw", 0.0, [false; 3]));
        assert!(near(level.yaw, 0.0) && near(level.pitch, 0.0) && near(level.roll, 0.0) && near(level.effective, 0.0));
        // Nose 30° right: a turn about up, the negative way in a (forward, left, up) frame.
        let right = glam::Quat::from_rotation_z(-30f32.to_radians());
        let h = heading_of(right, &proj("pitch_yaw", 0.0, [false; 3]));
        assert!(near(h.yaw, 30.0) && near(h.effective, 30.0), "{h:?}");
        // Nose 20° up: about left, the negative way.
        let h = heading_of(glam::Quat::from_rotation_y(-20f32.to_radians()), &proj("pitch_yaw", 0.0, [false; 3]));
        assert!(near(h.pitch, 20.0), "{h:?}");
        // Right grip 40° down, half of it let into X: the heading the output takes is off
        // the nose by half the roll.
        let rolled = glam::Quat::from_rotation_x(40f32.to_radians());
        let h = heading_of(rolled, &proj("pitch_yaw", 0.5, [false; 3]));
        assert!(near(h.roll, 40.0) && near(h.yaw, 0.0) && near(h.effective, -20.0), "{h:?}");
        let h = heading_of(rolled, &proj("pitch_roll", 0.0, [false; 3]));
        assert!(near(h.effective, 40.0), "Pitch+Roll's X is the roll: {h:?}");
        // An inverted output turns the pointer round with it, the no-roll dot too.
        let h = heading_of(right * rolled, &proj("pitch_yaw", 0.5, [true, false, false]));
        assert!(near(h.out_x, -h.effective) && near(h.base_x, -h.yaw), "{h:?}");
    }

}
