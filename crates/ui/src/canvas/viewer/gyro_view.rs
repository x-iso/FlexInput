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
    /// Whether the pose is the device's own, which is physical: the output turns it by the
    /// inverts. A pose the module computes already turns with them - the engine integrates
    /// the inverted rates - so reading the output off it applies them again only to roll's
    /// share.
    pub physical: bool,
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
    /// Where the X/Y outputs have got to, added up by the module itself as it puts them out
    /// (the engine's `GYRO_AIM_OUT`): x°, y°, x° without roll's share, and the pose's kind.
    /// The pointer. `None` without a running engine, which points it where the nose is.
    pub aim: Option<glam::Vec4>,
    /// The pad as the output has it (the engine's `GYRO_VIEW_OUT`): its nose on the aim,
    /// rolled as gravity says, in the hold's frame. The pad is drawn from it, so it and the
    /// pointer it drives agree in every mode - the camera's heading and pitch are the
    /// output's, whatever path the pad took and wherever the Orientation output, integrated
    /// on its own, has wandered. `None` without a running engine: drawn from the pose.
    pub view: Option<glam::Quat>,
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
    /// The module's Edge-turn settings (its `edge_*` params) while it is in Pointer with Edge
    /// turn on, for running Edge turn the way the module does ([`EdgeTrace`]).
    pub edge: Option<std::collections::HashMap<String, serde_json::Value>>,
    /// The Orientation output's rotation scale: the pose drawn is the pad's turn times this,
    /// and Edge turn measures the turn itself.
    pub orient_scale: f32,
    /// The frame's fill and outline, when a pin sets its own (`None` = the pane's own dark
    /// fill, no outline).
    pub frame: Option<(egui::Color32, egui::Stroke)>,
    /// How opaque the 3D model is (0..1); its neutral-hold ghost fainter in proportion.
    pub model_alpha: f32,
}

/// How wide the pad is drawn - its icon or its model, the same either way - as a share of
/// the sphere's radius, grip to grip: between the icon's old size and the model's.
const PAD_HALF_W: f32 = 0.5;
/// How long the output's trail on the sphere lasts.
const TRAIL_S: f64 = 0.6;
/// How long the Edge-turn world sphere takes to fade once the pad is back inside the zone.
const EDGE_FADE_S: f32 = 0.8;

/// What the preview remembers between frames, one per module (its body and its pins share
/// it): Edge turn's state as the module keeps it, how far the output has run on past where
/// the pad points, and the output's recent path for its trail.
#[derive(Clone, Default)]
struct EdgeTrace {
    st: [f32; 9],
    d_prev: Option<glam::Vec2>,
    /// Degrees of turn the output has taken beyond the pad's own: Edge turn's, for this
    /// stretch of it.
    offset: glam::Vec2,
    /// How much the world sphere shows (0..1): full while the pad is past the zone, fading
    /// out after it comes back, and when it is gone the turn starts from nothing again.
    shown: f32,
    time: f64,
    /// Where the output has been, as (time, x, y) on the world sphere - the pad's direction
    /// plus the turn Edge turn had taken then - so it is drawn turned with that sphere: still
    /// as the pad, streaming off the pointer while Edge turn turns the world past it.
    trail: std::collections::VecDeque<(f64, f32, f32)>,
    /// The world's turn the trail was last written in.
    noted: glam::Vec2,
}

impl EdgeTrace {
    /// One frame: run Edge turn on the pad's deflection `d` (degrees, the outputs' signs, the
    /// turn itself), `past` whether the pad is beyond the zone. Inside the zone the output
    /// follows the pad; past it the gyro's share scales the pad's motion and Edge turn adds
    /// its own, and that difference is the turn the world sphere shows. Once the pad is back
    /// and the sphere has faded, it starts from nothing; a jump (a Reset of the pose) starts
    /// it over at once.
    fn step(&mut self, now: f64, d: glam::Vec2, past: bool, edge: Option<&std::collections::HashMap<String, serde_json::Value>>) {
        let dt = if self.time > 0.0 { (now - self.time).clamp(0.0, 0.1) as f32 } else { 0.0 };
        self.time = now;
        let delta = self.d_prev.map_or(glam::Vec2::ZERO, |p| d - p);
        self.d_prev = Some(d);
        let Some(params) = edge else {
            self.offset = glam::Vec2::ZERO;
            self.shown = 0.0;
            self.st = [0.0; 9];
            return;
        };
        if delta.length() > 25.0 {
            self.offset = glam::Vec2::ZERO;
            self.shown = 0.0;
            self.st = [0.0; 9];
            self.trail.clear();
        }
        self.shown = if past { 1.0 } else { (self.shown - dt / EDGE_FADE_S).max(0.0) };
        if self.shown <= 0.0 {
            self.offset = glam::Vec2::ZERO;
        }
        match flexinput_engine::eval::edge_turn_preview(params, &mut self.st, d, dt) {
            Some((gyro, add)) => {
                self.offset += (gyro - glam::Vec2::ONE) * delta + add * dt;
                // Once round is once round.
                self.offset.x = (self.offset.x + 180.0).rem_euclid(360.0) - 180.0;
                self.offset.y = self.offset.y.clamp(-180.0, 180.0);
            }
            None => self.offset = glam::Vec2::ZERO,
        }
    }

    /// Note the output at `x`, `y` (where the pad points) with the world turned by `world`.
    fn note(&mut self, now: f64, x: f32, y: f32, world: glam::Vec2) {
        // The world's turn starting over (faded out): the trail written in the old turn is
        // moved into the new one, so it doesn't jump.
        if world == glam::Vec2::ZERO && self.noted != glam::Vec2::ZERO {
            for e in self.trail.iter_mut() {
                e.1 -= self.noted.x;
                e.2 -= self.noted.y;
            }
        }
        self.noted = world;
        let (x, y) = (x + world.x, y + world.y);
        if self.trail.back().is_none_or(|&(_, px, py)| (px - x).abs() > 0.05 || (py - y).abs() > 0.05) {
            self.trail.push_back((now, x, y));
        }
        while self.trail.front().is_some_and(|&(t, _, _)| now - t > TRAIL_S) {
            self.trail.pop_front();
        }
    }
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
    let fill = v.frame.map_or(visuals.extreme_bg_color, |(fill, _)| fill);
    painter.rect_filled(rect, 4.0, fill);

    // The whole pane is the sphere: the output's own numbers are a wire away, on any scope.
    // With a model to draw, it stands where the pad icon would, half see-through, the
    // sphere's marks over it.
    let c = cam(ui.ctx(), node);
    // Edge turn, run as the module runs it, the module's projection, and the output's trail -
    // one for the module, whichever views draw it: the body and a pin each kept their own, a
    // pin drawn less often added up the turn differently, and the two pointers parted.
    let key = egui::Id::new(("gyro_view_trace", node.0));
    let now = ui.input(|i| i.time);
    let mut trace: EdgeTrace = ui.ctx().data(|d| d.get_temp(key)).unwrap_or_default();
    let h = heading_of(v.pose.unwrap_or(glam::Quat::IDENTITY), &v.proj);
    // A computed pose turns by the 3D rot x, so Edge turn divides it back out of the pose and
    // its own turn is drawn turned by it; a device's own pose is physical.
    let scale = if v.proj.physical { 1.0 } else { v.orient_scale.abs().max(1e-3) };
    {
        let d = edge_deflection(&h, &v.proj, scale);
        let nose = v.pose.unwrap_or(glam::Quat::IDENTITY) * glam::Vec3::X;
        let past = v.zone.is_some_and(|z| nose.angle_between(glam::Vec3::X).to_degrees() > z);
        trace.step(now, d, past, v.edge.as_ref().filter(|_| v.pose.is_some()));
    }
    // The pointer is where the module has added its outputs up to (`aim`); Edge turn's own
    // turn is the world turning past it (`Output::world`).
    let world = (egui::vec2(trace.offset.x, trace.offset.y) * scale, trace.shown);
    let (out, base_x) = match v.aim.filter(|_| v.pose.is_some()) {
        Some(a) => (egui::vec2(a.x, a.y), a.z),
        None => (egui::vec2(h.out_x, h.out_y), h.base_x),
    };
    trace.note(now, out.x, out.y, glam::Vec2::new(world.0.x, world.0.y));
    let trail: Vec<(f32, f32, f32)> =
        trace.trail.iter().map(|&(t, x, y)| (((now - t) / TRAIL_S) as f32, x, y)).collect();
    ui.ctx().data_mut(|d| d.insert_temp(key, trace));
    // The far half of the sphere, then the pad, then the near half: the pad sits inside the
    // sphere, so what is behind it is drawn under it. (Drawn in one go, every ring behind the
    // model was painted across it.)
    let o = Output { at: out, base_x, world, trail: &trail };
    draw_gimbal(ui.ctx(), &painter, rect, v, c, &visuals, o, Pass::Back);
    if let Some(model) = v.model {
        // The neutral hold first, faint and dark grey, then the pad as it is.
        let r = sphere_radius(rect) * model_frame(model);
        let frame = egui::Rect::from_center_size(rect.center(), egui::vec2(r, r) * 2.0);
        let hold = hold_canonical(v.hold);
        let alpha = v.model_alpha.clamp(0.05, 1.0);
        draw_model(ui, (node.0 as u64) << 8 | 2, frame, hold, model, c, Some([0.20, 0.21, 0.23, 1.0]), alpha * MODEL_HOLD_SEE_THROUGH / MODEL_SEE_THROUGH);
        draw_model(ui, (node.0 as u64) << 8 | 1, frame, shown_pad(v), model, c, None, alpha);
    }
    draw_gimbal(ui.ctx(), &painter, rect, v, c, &visuals, o, Pass::Front);
    if let Some((_, stroke)) = v.frame.filter(|(_, s)| s.width > 0.0) {
        painter.rect_stroke(rect, 4.0, stroke, egui::StrokeKind::Inside);
    }

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
        // And the output back onto the pad: whatever Edge turn ran up starts over.
        ui.ctx().data_mut(|d| d.remove::<EdgeTrace>(key));
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
         dashed ring is where Edge turn takes over, the band fading out past it is the\n\
         blend (the gyro handing over), and a dotted ring is where Ramp reaches full\n\
         speed. The pointer follows the module's own projection - yawing a tilted pad\n\
         moves it straight across, as the output does. Past the zone a yellow sphere turns against\n\
         the pointer - the world going by as Edge turn keeps the camera turning - and fades\n\
         once the pad is back inside.",
    );
    if moved || v.pose.is_some() {
        // A live pose means the picture follows the pad, so the frames have to keep coming.
        ui.ctx().request_repaint();
    }
}

/// Where the output is this frame: on the sphere (`at`, x and y degrees), how far Edge turn
/// has turned the world past it and how much of that sphere shows (`world`), and its trail as
/// (age 0..1, x, y) on the world sphere, oldest first.
#[derive(Clone, Copy)]
struct Output<'a> {
    at: egui::Vec2,
    /// Where it would be without roll's share.
    base_x: f32,
    world: (egui::Vec2, f32),
    trail: &'a [(f32, f32, f32)],
}

/// Which half of the sphere a [`draw_gimbal`] call draws: the half beyond the pad's centre
/// (`Back`), drawn before the pad, or the half toward the camera with the flat marks over
/// everything (`Front`), after it.
#[derive(Clone, Copy, PartialEq)]
enum Pass {
    Back,
    Front,
}

/// The ring of nose directions `ang` radians off neutral, round forward, as `n` + 1 points.
fn ring_points(ang: f32, n: usize) -> Vec<glam::Vec3> {
    let a = any_perp(glam::Vec3::X);
    let b = glam::Vec3::X.cross(a);
    let (c, s) = (ang.cos(), ang.sin());
    (0..=n)
        .map(|k| {
            let t = k as f32 / n as f32 * std::f32::consts::TAU;
            glam::Vec3::X * c + (a * t.cos() + b * t.sin()) * s
        })
        .collect()
}

/// The neutral hold in the canonical frame the preview draws in. It is stored as a rotation in
/// the gyro's (forward, right, down) basis, which is the canonical (forward, left, up) with two
/// axes flipped - itself a half turn about forward, hence the sandwich.
fn hold_canonical(hold: glam::Quat) -> glam::Quat {
    let flip = glam::Quat::from_rotation_x(std::f32::consts::PI);
    flip * hold * flip
}

/// The pad as it is held: the Orientation output is its turn from the neutral hold, so the pad
/// itself is the hold turned by it. At a Reset it sits on the hold's ghost, tilted as the hold
/// is, while the pointer - the output - points straight ahead.
fn held(hold: glam::Quat, pose: glam::Quat) -> glam::Quat {
    (hold * pose).normalize()
}

/// The pad the preview draws: as the output has it (`GyroView::view`) - the hold turned by
/// it, so at a Reset it sits on the hold's ghost - or, without one, as the Orientation output
/// has it.
fn shown_pad(v: &GyroView) -> glam::Quat {
    let hold = hold_canonical(v.hold);
    match v.view.filter(|_| v.pose.is_some()) {
        Some(view) => (view * hold).normalize(),
        None => held(hold, v.pose.unwrap_or(glam::Quat::IDENTITY)),
    }
}

/// The model renderer's frame for `model`, as a multiple of the sphere's radius, that draws
/// it [`PAD_HALF_W`] of the radius from its centre to its furthest side, across the grips or
/// nose to back - the icon's size. The renderer frames a model's bounding sphere with room
/// round it; how big the pad is within that sphere is its own (a Steam Controller is wider for
/// its depth than a Joy-Con), so it is measured off the mesh, once per model. Measured across
/// the grips alone, a single Joy-Con - narrow across, long nose to back - came out three and a
/// half times a Pro Controller's size, nearly the sphere's.
fn model_frame(model: &str) -> f32 {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, f32>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(f) = cache.lock().ok().and_then(|c| c.get(model).copied()) {
        return f;
    }
    let Some(m) = crate::model::load_model_cached(model) else { return 1.0 };
    // The model's x is across the grips, its z nose to back (y is its thickness).
    let mut half_x = 0.0f32;
    for part in &m.parts {
        for v in part.vertices.chunks_exact(6) {
            let p = part.transform.transform_point3(glam::Vec3::new(v[0], v[1], v[2]));
            half_x = half_x.max((p.x - m.center.x).abs()).max((p.z - m.center.z).abs());
        }
    }
    // The renderer stands back 1.3 bounding radii's worth past a 45° view: a point `x` across
    // at the centre's depth lands `x · cos 22.5° / (1.3 · radius)` of the frame's half-width
    // out.
    let across = half_x * 22.5_f32.to_radians().cos() / (1.3 * m.radius.max(1e-3));
    let f = if across > 1e-3 { PAD_HALF_W / across } else { 1.0 };
    if let Ok(mut c) = cache.lock() {
        c.insert(model.to_string(), f);
    }
    f
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

/// Out of the screen, toward the camera: what is on this side of the sphere's centre is drawn
/// over the pad, the rest under it. (It was `up × right`, the camera's look - into the
/// screen - so every mark the pad hid was drawn over it and every one in front of it under,
/// the far side of the sphere bright and the near side faint.)
fn toward_camera(c: Cam) -> glam::Vec3 {
    let (right, up) = view_basis(c);
    right.cross(up)
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
    let effective = match proj.axis {
        "pitch_roll" => roll,
        "pitch_yaw" => yaw - roll * proj.roll_share,
        _ => yaw,
    };
    // The engine's X is `(yaw - roll * share) * inv_yaw` (Pitch+Roll's `roll * inv_yaw`), its Y
    // `pitch * inv_pitch`, and its roll comes in already turned by `inv_roll`. A computed pose
    // has all of that in it but the yaw invert on roll; a device's has none of it.
    let sign = |b: bool| if b { -1.0 } else { 1.0 };
    let (sx, sy, sr) = (sign(proj.inv[0]), sign(proj.inv[1]), sign(proj.inv[2]));
    let (oyaw, opitch, oroll) = if proj.physical { (yaw * sx, pitch * sy, roll * sr * sx) } else { (yaw, pitch, roll * sx) };
    let (out_x, base_x) = match proj.axis {
        "pitch_roll" => (oroll, 0.0),
        "pitch_yaw" => (oyaw - oroll * proj.roll_share, oyaw),
        _ => (oyaw, oyaw),
    };
    Heading { yaw, pitch, roll, effective, out_x, out_y: opitch, base_x }
}

/// The pose as Edge turn measures it - a stick-like deflection, X how far the pad is yawed
/// (rolled in Pitch+Roll), Y how far it is pitched, in the outputs' signs - exactly as the
/// engine does: a computed pose already turned by the inverts and by the 3D rot x (`scale`,
/// divided back out), a device's own physical and turned by them.
fn edge_deflection(h: &Heading, proj: &GyroProjection, scale: f32) -> glam::Vec2 {
    let sign = |b: bool| if b { -1.0 } else { 1.0 };
    let (sx, sy, sr) = (sign(proj.inv[0]), sign(proj.inv[1]), sign(proj.inv[2]));
    let pr = proj.axis == "pitch_roll";
    if proj.physical {
        let x = if pr { h.roll * sr } else { h.yaw };
        glam::Vec2::new(x * sx, h.pitch * sy)
    } else {
        let x = if pr { h.roll * sx } else { h.yaw };
        glam::Vec2::new(x, h.pitch) / scale.abs().max(1e-3)
    }
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
#[allow(clippy::too_many_arguments)]
fn draw_gimbal(ctx: &egui::Context, painter: &egui::Painter, rect: egui::Rect, v: &GyroView, c: Cam, visuals: &egui::Visuals, o: Output<'_>, pass: Pass) {
    let weak = visuals.weak_text_color();
    let text = visuals.text_color();
    let strong = visuals.strong_text_color();
    let (right, up) = view_basis(c);
    let towards = toward_camera(c);
    let centre = rect.center();
    let radius = sphere_radius(rect);
    let at = |p: glam::Vec3| egui::pos2(centre.x + p.dot(right) * radius, centre.y - p.dot(up) * radius);
    let back = pass == Pass::Back;
    // Whether a mark this deep - its reach toward the camera from the sphere's centre - is
    // this pass's.
    let mine = |depth: f32| if back { depth < 0.0 } else { depth >= 0.0 };
    // The sphere's own lines are fainter on its far side, so it reads as a ball. What the
    // module does - the arcs and the pointer - is drawn at full strength wherever it is: from
    // behind the pad, forward is the far side, and that is where everything happens.
    let grid_path = |pts: &[glam::Vec3], col: egui::Color32, w: f32| {
        for pair in pts.windows(2) {
            let depth = (pair[0] + pair[1]).dot(towards);
            if !mine(depth) {
                continue;
            }
            let c = if depth >= 0.0 { col } else { col.gamma_multiply(0.5) };
            painter.line_segment([at(pair[0]), at(pair[1])], egui::Stroke::new(w, c));
        }
    };
    // A line along the sphere, this pass's stretches of it, solid or dashed.
    let line_along = |pts: &[glam::Vec3], stroke: egui::Stroke, dash: Option<(f32, f32)>| {
        let mut run: Vec<egui::Pos2> = Vec::new();
        let flush = |run: &mut Vec<egui::Pos2>| {
            if run.len() > 1 {
                match dash {
                    Some((d, g)) => painter.extend(egui::Shape::dashed_line(run, stroke, d, g)),
                    None => {
                        painter.add(egui::Shape::line(std::mem::take(run), stroke));
                    }
                }
            }
            run.clear();
        };
        for pair in pts.windows(2) {
            if mine((pair[0] + pair[1]).dot(towards)) {
                if run.is_empty() {
                    run.push(at(pair[0]));
                }
                run.push(at(pair[1]));
            } else {
                flush(&mut run);
            }
        }
        flush(&mut run);
    };
    let path = |pts: &[glam::Vec3], col: egui::Color32, w: f32| line_along(pts, egui::Stroke::new(w, col), None);
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
    let small = crate::settings::ui_font(9.0);

    // ── The sphere: its outline, latitudes every 30°, meridians every 45°, and the heading
    //    ring (the equator) marked every 15° and numbered every 45°.
    if back {
        painter.circle_stroke(centre, radius, egui::Stroke::new(0.8_f32, weak.gamma_multiply(0.35)));
    }
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
        if !mine(d.dot(towards)) {
            continue;
        }
        let near = d.dot(towards) >= -0.1;
        let col = if near { weak } else { weak.gamma_multiply(0.7) };
        painter.line_segment([at(d), at(d * if major { 1.09 } else { 1.05 })], egui::Stroke::new(1.0_f32, col));
        // Numbered round the front, where the output lands, and at the back straight behind.
        if major && radius > 40.0 && (deg <= 135.0 || deg >= 225.0 || deg == 180.0) {
            let shown = if deg > 180.0 { deg - 360.0 } else { deg };
            painter.text(at(d * 1.2), egui::Align2::CENTER_CENTER, format!("{shown:+.0}"), small.clone(), col);
        }
    }

    // ── The Edge-turn zone, on the sphere round forward.
    if let Some(zone) = v.zone {
        // How far past the zone the gyro hands over to Edge turn (the blend) as a band that
        // fades out across it, eased as the module eases it; and where Ramp reaches its full
        // speed, dotted.
        let edge = v.edge.as_ref();
        let num = |k: &str, d: f32| edge.and_then(|e| e.get(k)).and_then(|v| v.as_f64()).map_or(d, |v| v as f32);
        let mode = edge.and_then(|e| e.get("edge_mode")).and_then(|v| v.as_str()).unwrap_or("off");
        let blend = num("edge_blend_deg", 15.0).clamp(0.0, 170.0);
        if blend > 0.5 {
            const STEPS: usize = 12;
            for k in 1..=STEPS {
                let t = k as f32 / STEPS as f32;
                let ease = t * t * (3.0 - 2.0 * t);
                let ang = (zone + blend * t).min(175.0).to_radians();
                line_along(&ring_points(ang, 48), egui::Stroke::new(1.6_f32, roll_col.gamma_multiply(0.42 * (1.0 - ease))), None);
            }
        }
        if mode == "ramp" || mode == "both" {
            let ramp = num("edge_ramp_deg", 30.0).clamp(0.0, 170.0);
            line_along(&ring_points((zone + ramp).min(175.0).to_radians(), 48), egui::Stroke::new(1.2_f32, roll_col.gamma_multiply(0.7)), Some((1.5, 4.0)));
        }
        line_along(&ring_points(zone.to_radians(), 40), egui::Stroke::new(1.2_f32, roll_col.gamma_multiply(0.85)), Some((4.0, 1.1)));
    }

    // Where the output is, by the module's own projection.
    let (out_x, out_y) = (o.at.x, o.at.y);
    let base_x = o.base_x;

    // ── Edge turn: while it turns the camera, the world turns past the pointer - a yellow
    //    sphere, turned against the pointer by the turn Edge turn has taken, fading once the
    //    pad is back inside the zone.
    let (world, world_shown) = o.world;
    let edge_shown = v.zone.is_some() && world_shown > 0.01;
    // The world's turn: none without Edge turn.
    let turn = glam::Quat::from_rotation_y(world.y.to_radians()) * glam::Quat::from_rotation_z(world.x.to_radians());
    if edge_shown {
        let col = roll_col.gamma_multiply(0.38 * world_shown);
        let turned: Vec<Vec<glam::Vec3>> = [-60.0_f32, -30.0, 0.0, 30.0, 60.0]
            .into_iter()
            .map(|lat| along(lat, 0.0, 360.0))
            .chain((0..4).map(|m| circle(on_heading(m as f32 * 45.0), glam::Vec3::Z, 0.0, 360.0, 72)))
            .map(|pts| pts.into_iter().map(|p| turn * p).collect())
            .collect();
        for pts in &turned {
            grid_path(pts, col, 0.9);
        }
    }

    // ── Its trail: where it has been this last moment, fading out, as a vectorscope draws it -
    //    on the world sphere, so while Edge turn turns the world the trail streams off the
    //    pointer with it: the motion the 2D output is given beyond the pad's own.
    for pair in o.trail.windows(2) {
        let ((_, x0, y0), (age, x1, y1)) = (pair[0], pair[1]);
        if (x1 - x0).abs() > 90.0 {
            continue; // across the back of the sphere: not a path it took
        }
        let a = (1.0 - age).clamp(0.0, 1.0);
        let n = (((x1 - x0).abs().max((y1 - y0).abs()) / 4.0).ceil() as usize).clamp(1, 16);
        let pts: Vec<glam::Vec3> = (0..=n).map(|k| {
            let t = k as f32 / n as f32;
            turn * dir(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)
        }).collect();
        path(&pts, strong.gamma_multiply(0.75 * a), 1.0 + 1.6 * a);
    }

    // ── Where the output lands, on the sphere: along the heading ring from forward (X),
    //    and up the meridian from there (Y).
    path(&along(0.0, 0.0, out_x), yaw_col, 2.2);
    if v.proj.reads(Rate::Pitch) {
        let foot = on_heading(out_x);
        path(&circle(foot, glam::Vec3::Z, 0.0, out_y, 32), pitch_col, 2.2);
    }

    // ── Roll into X: the arc the pointer pivots along, behind it, from where it would be
    //    without roll (the yellow dot) to where it lands.
    let roll_in = (v.proj.axis == "pitch_yaw" && v.proj.roll_share.abs() > 1e-3) || v.proj.axis == "pitch_roll";
    if roll_in && (out_x - base_x).abs() > 0.3 {
        path(&along(out_y, base_x, out_x), roll_col, 2.4);
    }
    if roll_in && mine(dir(base_x, out_y).dot(towards)) {
        painter.circle_filled(at(dir(base_x, out_y)), 3.6, roll_col);
    }

    // ── The pointer: from the pad to where the output lands - over the pad when it points
    //    toward the camera, under it when it points away.
    let tip = dir(out_x, out_y);
    if mine(tip.dot(towards)) {
        painter.line_segment([centre, at(tip)], egui::Stroke::new(1.6_f32, strong));
        painter.circle_filled(at(tip), 3.6, strong);
    }

    // ── The pad in the middle, between the sphere's halves: the neutral hold as a ghost, and
    //    where it is now - a gamepad lying in its own frame. With a model, the model is the
    //    pad, drawn between the two passes by the caller: no icon at all, the hold's ghost
    //    included. Everything after this is the front pass's: the flat marks over the lot.
    if back {
        if v.model.is_none() {
            let hold = hold_canonical(v.hold);
            // Grip to grip `PAD_HALF_W` of the radius: the icon is 1.24 units across.
            let at_pad = |p: glam::Vec3| at(p * (PAD_HALF_W / 0.62));
            // A pad nothing names is drawn as the Xbox pad, as its model is.
            let svg = v.pad_svg.unwrap_or_else(crate::canvas::remapper_icons::default_pad_svg);
            pad_icon(ctx, painter, &at_pad, hold, weak.gamma_multiply(0.35), svg, v.outline);
            pad_icon(ctx, painter, &at_pad, shown_pad(v), text.gamma_multiply(0.9), svg, v.outline);
        }
        return;
    }

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
        if v.zone.is_some() {
            row(&line(roll_col.gamma_multiply(0.45)), "edge zone, blend");
        }
        if edge_shown {
            row(&line(roll_col.gamma_multiply(0.7)), "edge turn: the world");
        }
        row(&line(lean_col), "lean");
    }

    // The angles, along the bottom.
    if radius > 40.0 {
        let label = if roll_in {
            format!("yaw {:+.0}°  pitch {:+.0}°  roll {:+.0}°  →  x {:+.0}°  y {:+.0}°", h.yaw, h.pitch, h.roll, out_x, out_y)
        } else {
            format!("yaw {:+.0}°  pitch {:+.0}°  →  x {:+.0}°  y {:+.0}°", h.yaw, h.pitch, out_x, out_y)
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

    if v.pose.is_none() {
        painter.text(
            egui::pos2(centre.x, rect.top() + 8.0),
            egui::Align2::CENTER_TOP,
            "no Orientation reading",
            crate::settings::ui_font(10.0),
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
    // The pad is 1.24 units along its longer side - across the grips for most, nose to back
    // for a Joy-Con's tall icon - and the other side follows the icon's proportions. Across
    // the grips alone, a tall icon came out over twice the sphere's size.
    let aspect = aspect.max(0.1);
    let (half_w, half_h) = if aspect >= 1.0 { (0.62, 0.62 / aspect) } else { (0.62 * aspect, 0.62) };
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

/// The pad model at `pose` (canonical frame): the same mesh, palette and view basis as the
/// 3D viewer's, since the two are looking at one quaternion. `grey` paints every part one
/// colour (the neutral hold's ghost); `see_through` is how opaque the finished picture is.
/// `instance` keys the renderer's buffers, which every callback in a frame shares, so it has
/// to differ from the 3D viewer node's and between the two models here.
#[allow(clippy::too_many_arguments)]
fn draw_model(
    ui: &mut egui::Ui,
    instance: u64,
    rect: egui::Rect,
    pose: glam::Quat,
    model: &str,
    c: Cam,
    grey: Option<[f32; 4]>,
    see_through: f32,
) {
    // The renderer's camera sits straight behind the pad; turn the pad by how far round the
    // orbit is from there, so the model and the sphere drawn over it agree.
    let q = glam::Quat::from_rotation_z(-(c.yaw - CAM_BEHIND)) * pose;
    let (_, outline, outline_w, _) = controller3d_style(None);
    let bg = egui::Color32::TRANSPARENT;
    let rgb = crate::model::default_scheme(model);
    let mut scheme = [[0.0f32; 4]; crate::model::N_GROUPS];
    for (o, i) in scheme.iter_mut().zip(rgb.iter()) {
        *o = match grey {
            Some(g) => [g[0], g[1], g[2], i[3] as f32 / 255.0],
            None => [i[0] as f32 / 255.0, i[1] as f32 / 255.0, i[2] as f32 / 255.0, i[3] as f32 / 255.0],
        };
    }
    render_controller3d_core(
        ui,
        instance,
        rect,
        model,
        to_view_basis(q.normalize()),
        bg,
        outline,
        outline_w,
        scheme,
        // Drawn solid, then the finished picture laid over the sphere half see-through:
        // the renderer's last argument is the opacity its offscreen image is composited at.
        // It was 0 here, so the model was rendered and then composited away - nothing.
        1.0,
        c.pitch,
        crate::model::ControllerLive::default(),
        see_through,
    );
}

/// How opaque the pad model is over the sphere's marks.
const MODEL_SEE_THROUGH: f32 = 0.5;
/// And the neutral hold's dark grey ghost of it, fainter.
const MODEL_HOLD_SEE_THROUGH: f32 = 0.28;

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(axis: &'static str, roll_share: f32, inv: [bool; 3]) -> GyroProjection {
        GyroProjection { axis, roll_share, inv, physical: false }
    }

    // The model's composite opacity: 0 renders it and throws the picture away (the model
    // "never showed"), 1 hides the sphere's back marks behind it.
    #[test]
    fn the_model_is_composited_half_see_through_not_away() {
        assert!(MODEL_SEE_THROUGH > 0.1 && MODEL_SEE_THROUGH < 1.0, "{MODEL_SEE_THROUGH}");
    }

    // Edge turn in the preview runs as the module does: inside the zone there is no turn of its
    // own; held past it on a ramp, it turns the way the pad is turned; back inside, the world
    // sphere holds that turn while it fades and then starts from nothing; a jump (a Reset of
    // the pose) starts it over at once.
    #[test]
    fn the_preview_runs_edge_turn_and_keeps_the_turn() {
        let params: std::collections::HashMap<String, serde_json::Value> = [
            ("edge_mode", serde_json::json!("ramp")),
            ("edge_zone_deg", serde_json::json!(30.0)),
            ("edge_ramp_deg", serde_json::json!(10.0)),
            ("edge_max_dps", serde_json::json!(180.0)),
            ("edge_gyro_keep", serde_json::json!(1.0)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let mut t = EdgeTrace::default();
        let mut now = 1.0;
        let mut run = |t: &mut EdgeTrace, d: glam::Vec2, frames: usize| {
            for _ in 0..frames {
                now += 1.0 / 60.0;
                t.step(now, d, d.length() > 30.0, Some(&params));
            }
        };
        run(&mut t, glam::Vec2::new(20.0, 0.0), 30);
        assert!(t.offset.length() < 1e-3, "inside the zone it follows the pad: {:?}", t.offset);
        run(&mut t, glam::Vec2::new(24.0, 0.0), 1);
        run(&mut t, glam::Vec2::new(28.0, 0.0), 1);
        run(&mut t, glam::Vec2::new(32.0, 0.0), 1);
        run(&mut t, glam::Vec2::new(36.0, 0.0), 1);
        run(&mut t, glam::Vec2::new(40.0, 0.0), 30);
        assert!(t.offset.x > 60.0 && t.offset.x < 120.0, "half a second past the ramp turns about 90°: {:?}", t.offset);
        let kept = t.offset.x;
        for x in [35.0, 30.0, 25.0, 20.0] {
            run(&mut t, glam::Vec2::new(x, 0.0), 1);
        }
        run(&mut t, glam::Vec2::new(20.0, 0.0), 20);
        assert!((t.offset.x - kept).abs() < 15.0 && t.shown > 0.0, "back inside, the turn shows while it fades: {kept} -> {:?}", t.offset);
        run(&mut t, glam::Vec2::new(20.0, 0.0), 60);
        assert!(t.offset.length() < 1e-3 && t.shown == 0.0, "faded, it starts from nothing: {:?}", t.offset);
        run(&mut t, glam::Vec2::new(40.0, 0.0), 30);
        run(&mut t, glam::Vec2::new(-20.0, 0.0), 1);
        assert!(t.offset.length() < 1e-3, "a jump starts it over: {:?}", t.offset);
        t.step(now + 0.1, glam::Vec2::new(60.0, 0.0), true, None);
        assert_eq!(t.offset, glam::Vec2::ZERO, "Edge turn off: the output is the pad's");
    }

    // Held still past the edge, the pointer doesn't move but the output does: its trail is
    // written on the turning world, so it spreads out behind the pointer while its newest
    // point stays on it - and when the turn starts over, the trail moves with it, unjumped.
    #[test]
    fn the_trail_streams_off_the_pointer_while_edge_turn_turns() {
        let params: std::collections::HashMap<String, serde_json::Value> = [
            ("edge_mode", serde_json::json!("ramp")),
            ("edge_zone_deg", serde_json::json!(30.0)),
            ("edge_ramp_deg", serde_json::json!(5.0)),
            ("edge_max_dps", serde_json::json!(180.0)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let mut t = EdgeTrace::default();
        let mut now = 1.0;
        let d = glam::Vec2::new(40.0, 0.0);
        for _ in 0..24 {
            now += 1.0 / 60.0;
            t.step(now, d, true, Some(&params));
            t.note(now, 40.0, 0.0, t.offset);
        }
        let (first, last) = (t.trail.front().copied().unwrap(), t.trail.back().copied().unwrap());
        assert!(last.1 - first.1 > 30.0, "the trail spreads along the world: {first:?} .. {last:?}");
        let on_pointer = last.1 - t.offset.x;
        assert!((on_pointer - 40.0).abs() < 0.5, "its newest point is on the pointer: {on_pointer}");
        // Back inside and faded: the turn starts over, and the trail is moved with it.
        let before: Vec<f32> = t.trail.iter().map(|e| e.1 - t.offset.x).collect();
        let held = t.offset;
        t.offset = glam::Vec2::ZERO;
        t.note(now, 40.0, 0.0, glam::Vec2::ZERO);
        let after: Vec<f32> = t.trail.iter().map(|e| e.1).collect();
        assert!(held.x > 0.0);
        for (b, a) in before.iter().zip(&after) {
            assert!((b - a).abs() < 1e-3, "the trail jumped when the turn started over: {b} -> {a}");
        }
    }

    // With a neutral hold set, the pad at a Reset is drawn as held - on the hold's ghost - and
    // the pointer it drives points straight ahead; turning it turns the pad from the hold.
    #[test]
    fn the_pad_is_drawn_as_held_and_the_pointer_from_its_turn() {
        let hold_gyro = glam::Quat::from_rotation_y(0.5); // the pad pitched in the gyro's basis
        let hold = hold_canonical(hold_gyro);
        let at_reset = held(hold, glam::Quat::IDENTITY);
        assert!(at_reset.angle_between(hold) < 1e-4, "at a Reset the pad sits on the hold's ghost");
        let h = heading_of(glam::Quat::IDENTITY, &proj("pitch_yaw", 0.0, [false; 3]));
        assert!(h.out_x.abs() < 1e-3 && h.out_y.abs() < 1e-3, "and the pointer points ahead: {h:?}");
        let turned = glam::Quat::from_rotation_z(-0.4);
        assert!((held(hold, turned).angle_between(hold) - 0.4).abs() < 1e-3, "turning it turns the pad from the hold");
    }

    #[test]
    fn each_mode_reads_the_rates_it_projects() {
        assert!(proj("pitch_yaw", 0.0, [false; 3]).reads(Rate::Yaw));
        assert!(!proj("pitch_yaw", 0.0, [false; 3]).reads(Rate::Roll), "no roll share, no roll");
        assert!(proj("pitch_yaw", 0.5, [false; 3]).reads(Rate::Roll));
        assert!(proj("pitch_roll", 0.0, [false; 3]).reads(Rate::Roll));
        assert!(!proj("pitch_roll", 0.0, [false; 3]).reads(Rate::Yaw), "Pitch+Roll reads no yaw");
        for axis in ["player", "world", "orient"] {
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
        // And out of the screen is behind the pad, where the camera is: what's drawn over the
        // model is what's on the camera's side of it (it was the far side once).
        let t = toward_camera(Cam { yaw: CAM_BEHIND, pitch: 0.0 });
        assert!((t - glam::Vec3::NEG_X).length() < 1e-4, "{t:?}");
        let above = toward_camera(Cam { yaw: CAM_BEHIND, pitch: 1.2 });
        assert!(above.z > 0.9, "looking down from above, the camera is up: {above:?}");
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
        // On a device's own pose - physical - an inverted output turns the pointer round with
        // it, the no-roll dot too.
        let physical = GyroProjection { physical: true, ..proj("pitch_yaw", 0.5, [true, false, false]) };
        let h = heading_of(right * rolled, &physical);
        assert!(near(h.out_x, -h.effective) && near(h.base_x, -h.yaw), "{h:?}");
        // A computed pose already turns with the inverts: the nose is the output's, and only
        // roll's share takes the yaw invert (the engine's X is `(yaw - roll * share) * inv_yaw`
        // with the invert already in the pose's yaw).
        let h = heading_of(right * rolled, &proj("pitch_yaw", 0.5, [true, false, false]));
        assert!(near(h.out_x, h.yaw + 0.5 * h.roll) && near(h.base_x, h.yaw), "{h:?}");
        // And Edge turn measures it as the engine does: a computed pose as it is, less its 3D
        // rot x; a device's turned by the inverts.
        let d = edge_deflection(&h, &proj("pitch_yaw", 0.5, [true, true, false]), 2.0);
        assert!(near(d.x, h.yaw / 2.0) && near(d.y, h.pitch / 2.0), "{d:?}");
        let ph = heading_of(right * rolled, &GyroProjection { physical: true, ..proj("pitch_yaw", 0.5, [true, true, false]) });
        let d = edge_deflection(&ph, &GyroProjection { physical: true, ..proj("pitch_yaw", 0.5, [true, true, false]) }, 1.0);
        assert!(near(d.x, -ph.yaw) && near(d.y, -ph.pitch), "{d:?}");
    }

    // Every bundled pad draws the same size in the preview: its furthest side - across the
    // grips or nose to back - at PAD_HALF_W of the sphere's radius. A single Joy-Con, narrow
    // across and long nose to back, was sized by its width alone and came out nearly as big as
    // the sphere.
    #[test]
    fn every_pad_model_fits_the_same_footprint() {
        let models = crate::model::available_models();
        assert!(models.iter().any(|m| m == "Left Joycon"), "{models:?}");
        for name in models {
            let Some(m) = crate::model::load_model_cached(&name) else { continue };
            let (mut hx, mut hz) = (0.0f32, 0.0f32);
            for part in &m.parts {
                for v in part.vertices.chunks_exact(6) {
                    let p = part.transform.transform_point3(glam::Vec3::new(v[0], v[1], v[2]));
                    hx = hx.max((p.x - m.center.x).abs());
                    hz = hz.max((p.z - m.center.z).abs());
                }
            }
            let drawn = |half: f32| half * 22.5_f32.to_radians().cos() / (1.3 * m.radius.max(1e-3)) * model_frame(&name);
            let (across, length) = (drawn(hx), drawn(hz));
            assert!(across <= PAD_HALF_W + 1e-3 && length <= PAD_HALF_W + 1e-3, "{name}: {across} across, {length} long");
            assert!((across.max(length) - PAD_HALF_W).abs() < 1e-3, "{name}: {across} across, {length} long");
        }
    }
}
