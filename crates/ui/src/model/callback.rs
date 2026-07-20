//! egui_wgpu paint-callback integration for the 3D controller model viewer,
//! plus the runtime model loader/cache and device→model mapping.
//!
//! A [`MeshRenderState`] implements [`CallbackTrait`] so a controller model
//! renders inside an egui node body. The GPU pipeline and per-part buffers are
//! created lazily on the first `prepare()` (and rebuilt when the model changes),
//! then cached in the egui-wgpu `CallbackResources` across frames.
//!
//! The camera auto-frames the model from its bounds, so a model renders
//! regardless of its OBJ units. Orientation (a quaternion from the Gyro 3DOF
//! module's `Orientation` output) rotates the whole assembly about its centre.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use egui_wgpu::{Callback, CallbackResources, CallbackTrait};
use glam::{Mat4, Quat, Vec3};
use wgpu::{CommandEncoder, Queue};

use crate::canvas::remapper_icons::{skin_from_device_id, Skin};
use crate::model::pipeline::*;
use crate::model::part_transform;

// ── Loaded model (CPU side, cached) ───────────────────────────────────────────

/// A controller model prepared for rendering: per-part interleaved vertices +
/// precomputed transform, plus the assembly's bounding centre/radius (used to
/// frame the camera). Built once per model name and shared via `Arc`.
pub struct LoadedModel {
    pub name: String,
    pub parts: Vec<PartData>,
    /// Centre of the assembled model in model space (rotation pivot + look-at).
    pub center: Vec3,
    /// Bounding radius from `center` (camera distance is derived from this).
    pub radius: f32,
    /// Touchpad surface for mapping normalized touch input onto the model, if
    /// the model has `touch_point*` parts with extents in `info.txt`.
    pub touch_surface: Option<TouchSurface>,
    /// Part indices of `touch_point1` / `touch_point2` (the movable finger dots),
    /// or `None` when the model has no such part.
    pub touch_point_parts: [Option<usize>; 2],
}

/// The touchpad's flat surface in model space: finger dots rest at `center` and
/// move within `± half` on the X (width) and Z (depth) axes.
#[derive(Clone, Copy)]
pub struct TouchSurface {
    pub center: Vec3,
    /// Half-extents `(half_x, half_z)`.
    pub half: glam::Vec2,
}

/// Live input state that animates the model: touch dots, stick tilt, trigger
/// pull and button presses. Built each frame from the resolved device's live
/// signals (see `controller3d_live` in the viewer).
#[derive(Clone, Default)]
pub struct ControllerLive {
    /// Normalized `[0,1]` touch positions (x = left→right, y = top→bottom);
    /// `None` = that finger is up.
    pub touch: [Option<glam::Vec2>; 2],
    /// Stick deflections, each component `-1..1`.
    pub left_stick: glam::Vec2,
    pub right_stick: glam::Vec2,
    /// Analog trigger pulls, `0..1`.
    pub left_trigger: f32,
    pub right_trigger: f32,
    /// Stick click (L3/R3) press amounts, `0..1` — the whole stick sinks and
    /// the cap highlights on press.
    pub left_stick_press: f32,
    pub right_stick_press: f32,
    /// Button press amounts keyed by the mesh part name (e.g. `"a_button"`,
    /// `"dpad_up"`, `"left_bumper"`), `0..1`.
    pub buttons: std::collections::HashMap<String, f32>,
    /// Live LED / lightbar colour (0..1 RGB) relayed onto the `Led` material
    /// group, when the device exposes it on the AutoMap bus. `None` = keep the
    /// group's scheme colour.
    pub led: Option<[f32; 3]>,
    /// Per-part highlight intensity (`0..1`, already time-smoothed with the
    /// node's tail-off) keyed by mesh part name — lights active inputs and fades
    /// them out. Empty = nothing highlighted.
    pub glow: std::collections::HashMap<String, f32>,
    /// Highlight colour (0..1 RGB) for active inputs — from the pinned item's
    /// style `accent` (or the default accent on the node body).
    pub highlight: [f32; 3],
}

impl ControllerLive {
    /// Extra model-space transform for a part given its rest transform, or
    /// identity when the part isn't animated. Rotations pivot about the part's
    /// own placement so sticks/triggers hinge in place. Touch points are handled
    /// separately in `prepare` (they also change colour / visibility).
    fn part_xform(&self, name: &str, part_tf: &Mat4, footprint: f32) -> Mat4 {
        let n = name.to_ascii_lowercase();
        // Pivot = the part's placed origin (translation column of its transform).
        let pivot = part_tf.w_axis.truncate();
        let about = |r: Quat| {
            Mat4::from_translation(pivot) * Mat4::from_quat(r) * Mat4::from_translation(-pivot)
        };
        // Buttons: depress while held. Bumpers hinge back into the shell (+Z);
        // the rest indent into their top face (−Y). Travel scales with the
        // button's horizontal footprint so the tiny Home/Capture/± caps sink
        // barely at all instead of disappearing into the shell — the highlight
        // glow carries most of the visual feedback anyway.
        if let Some(&press) = self.buttons.get(n.as_str()) {
            if press <= 0.001 {
                return Mat4::IDENTITY;
            }
            if n.contains("bumper") {
                return Mat4::from_translation(Vec3::new(0.0, 0.0, 0.04 * press));
            }
            let travel = if footprint > 1e-4 {
                (footprint * 0.12).clamp(0.002, 0.012)
            } else {
                0.012
            };
            return Mat4::from_translation(Vec3::new(0.0, -travel * press, 0.0));
        }
        // Sticks: the dome + cap + rim tilt together about the stick base, and
        // the whole assembly sinks on an L3/R3 click.
        let stick = if n == "left_stick" || n == "left_cap" || n == "left_ring" {
            Some((self.left_stick, self.left_stick_press))
        } else if n == "right_stick" || n == "right_cap" || n == "right_ring" {
            Some((self.right_stick, self.right_stick_press))
        } else {
            None
        };
        if let Some((v, press)) = stick {
            const MAX_TILT: f32 = 0.35; // radians at full deflection
            let r = Quat::from_rotation_x(-v.y * MAX_TILT) * Quat::from_rotation_z(-v.x * MAX_TILT);
            let sink = Mat4::from_translation(Vec3::new(0.0, -0.035 * press, 0.0));
            return sink * about(r);
        }
        // Triggers: rotate inward about the hinge (approximated at the part origin).
        let trig = if n == "left_trigger" {
            Some(self.left_trigger)
        } else if n == "right_trigger" {
            Some(self.right_trigger)
        } else {
            None
        };
        if let Some(t) = trig {
            return about(Quat::from_rotation_x(-0.4 * t));
        }
        Mat4::IDENTITY
    }
}

/// Per-part mesh data carried from model loading into the callback.
pub struct PartData {
    /// Part name (`.obj` stem, e.g. `"a_button"`) — used to dispatch animations.
    pub name: String,
    /// Interleaved `[pos.x, pos.y, pos.z, norm.x, norm.y, norm.z]`.
    pub vertices: Vec<f32>,
    pub tri_count: usize,
    /// Static model-space transform (position + rotation from `info.txt`).
    pub transform: Mat4,
    /// Material group index (`MaterialGroup as usize`) — selects the part's
    /// colour from the viewer's per-group scheme.
    pub group: usize,
    /// Assembled-space centroid of the part's vertices — with `avg_normal`,
    /// estimates whether the part faces the camera (x-ray gating).
    pub centroid: Vec3,
    /// Assembled-space average outward normal (normalized; ZERO if degenerate).
    pub avg_normal: Vec3,
    /// Horizontal footprint (max of assembled-space X/Z extents) — scales the
    /// press-travel so small buttons don't vanish into the shell.
    pub footprint: f32,
    /// Evenly-strided `(position, normal)` samples in LOCAL space (the same
    /// space as `vertices`, i.e. before `transform`), capped at
    /// `VIS_SAMPLES_PER_PART`. The x-ray visibility test projects these against
    /// a depth prepass on the CPU; keeping them local means the exact same
    /// `model_m` the renderer builds for the part applies unchanged, animation
    /// included.
    pub samples: Vec<(Vec3, Vec3)>,
}

/// How many surface points represent a part in the visibility test. The measure
/// is a ratio, so precision comes from spread rather than count — a couple of
/// hundred points put the sampling error well under the gap between the
/// hysteresis thresholds, and the whole model then costs a few thousand point
/// transforms per measurement.
const VIS_SAMPLES_PER_PART: usize = 192;

// ── Render target format ──────────────────────────────────────────────────────

/// The surface/target format eframe actually negotiated, captured once at app
/// startup from the `RenderState`. The controller pipeline must be built with
/// this exact format or wgpu rejects the draw ("Render pipeline targets are
/// incompatible with render pass"). Backends differ — DX12 gave `Bgra8Unorm`
/// here, not the sRGB variant we assumed — so we never hardcode it.
static TARGET_FORMAT: OnceLock<wgpu::TextureFormat> = OnceLock::new();

/// Record the render target format (call once, from `FlexInputApp::new`).
pub fn set_target_format(fmt: wgpu::TextureFormat) {
    let _ = TARGET_FORMAT.set(fmt);
}

/// The captured target format, defaulting to BGRA8 sRGB if it was never set
/// (e.g. a non-wgpu render backend), which is the common desktop surface.
fn target_format() -> wgpu::TextureFormat {
    TARGET_FORMAT
        .get()
        .copied()
        .unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb)
}

// ── Model cache + loader ──────────────────────────────────────────────────────

/// Process-wide cache of parsed models. `None` marks a name that failed to load
/// so we don't retry the disk read every frame.
static MODEL_CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<LoadedModel>>>>> = OnceLock::new();

fn model_cache() -> &'static Mutex<HashMap<String, Option<Arc<LoadedModel>>>> {
    MODEL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve the `app/assets/models` directory at runtime. Tries next to the exe
/// (shipped layout) and the working directory (dev / `cargo run` from the
/// workspace root); first existing wins.
pub fn models_base_dir() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("assets").join("models"));
            // Walk up from the exe so `target/debug/flexinput.exe` (run
            // directly, not via `cargo run`) still finds the workspace's
            // `app/assets/models` two levels up.
            let mut anc = Some(dir);
            for _ in 0..5 {
                let Some(d) = anc else { break };
                cands.push(d.join("app").join("assets").join("models"));
                cands.push(d.join("assets").join("models"));
                anc = d.parent();
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        cands.push(cwd.join("app").join("assets").join("models"));
        cands.push(cwd.join("assets").join("models"));
    }
    cands.into_iter().find(|p| p.is_dir())
}

/// Optional user-provided models directory (global setting). Folders inside it
/// follow the same structure as the bundled ones (`<Name>/info.txt` + `.obj`s
/// + optional `colors.fxcol`); a same-named folder OVERRIDES the bundled model.
static USER_MODELS_DIR: OnceLock<std::sync::RwLock<Option<PathBuf>>> = OnceLock::new();

fn user_models_lock() -> &'static std::sync::RwLock<Option<PathBuf>> {
    USER_MODELS_DIR.get_or_init(|| std::sync::RwLock::new(None))
}

/// Set (or clear) the user models directory. Clears the model + scheme caches
/// so newly added models appear without an app restart.
pub fn set_user_models_dir(dir: Option<PathBuf>) {
    if let Ok(mut w) = user_models_lock().write() {
        *w = dir.filter(|d| d.is_dir());
    }
    if let Ok(mut c) = model_cache().lock() {
        c.clear();
    }
    crate::model::material::clear_scheme_cache();
}

fn user_models_dir() -> Option<PathBuf> {
    user_models_lock().read().ok()?.clone()
}

/// All model roots, user directory first (so user models override bundled).
fn model_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(u) = user_models_dir() {
        out.push(u);
    }
    if let Some(b) = models_base_dir() {
        out.push(b);
    }
    out
}

/// The whole `app/assets/models` tree embedded at COMPILE time — the binary
/// always carries the models it was built with (like the SVG assets). No
/// model names are hardcoded: whatever folders exist get bundled. On-disk
/// sources (user dir, then the dev assets folder) override the embedded copy,
/// so models stay editable during development without a rebuild... of assets.
static EMBEDDED_MODELS: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../app/assets/models");

/// Where a model's files come from, in priority order: user dir → dev assets
/// on disk → embedded copy.
pub(crate) enum ModelSrc {
    Dir(PathBuf),
    Embedded,
}

/// Resolve the source tier for model `name`.
pub(crate) fn model_src_for(name: &str) -> Option<ModelSrc> {
    for d in model_dirs() {
        let p = d.join(name);
        if p.join("info.txt").is_file() {
            return Some(ModelSrc::Dir(p));
        }
    }
    if EMBEDDED_MODELS
        .get_file(format!("{name}/info.txt"))
        .is_some()
    {
        return Some(ModelSrc::Embedded);
    }
    None
}

/// Read a text file belonging to model `name` through the source tiers.
pub(crate) fn model_file(name: &str, file: &str) -> Option<String> {
    match model_src_for(name)? {
        ModelSrc::Dir(p) => std::fs::read_to_string(p.join(file)).ok(),
        ModelSrc::Embedded => EMBEDDED_MODELS
            .get_file(format!("{name}/{file}"))?
            .contents_utf8()
            .map(str::to_string),
    }
}

/// Names of every controller model folder available (those containing an
/// `info.txt`), across the user + disk + embedded roots, sorted and
/// de-duplicated. Drives the node's model-override dropdown.
pub fn available_models() -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for base in model_dirs() {
        if let Ok(rd) = std::fs::read_dir(&base) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && p.join("info.txt").is_file() {
                    if let Some(n) = e.file_name().to_str() {
                        set.insert(n.to_string());
                    }
                }
            }
        }
    }
    for d in EMBEDDED_MODELS.dirs() {
        let has_info = d
            .files()
            .any(|f| f.path().file_name().is_some_and(|n| n == "info.txt"));
        if has_info {
            if let Some(n) = d.path().file_name().and_then(|n| n.to_str()) {
                set.insert(n.to_string());
            }
        }
    }
    set.into_iter().collect()
}

/// Load a model by folder name, caching the result (including failures).
pub fn load_model_cached(name: &str) -> Option<Arc<LoadedModel>> {
    if name.is_empty() {
        return None;
    }
    if let Some(hit) = model_cache().lock().ok()?.get(name) {
        return hit.clone();
    }
    let loaded = build_loaded_model(name);
    if let Ok(mut c) = model_cache().lock() {
        c.insert(name.to_string(), loaded.clone());
    }
    loaded
}

fn build_loaded_model(name: &str) -> Option<Arc<LoadedModel>> {
    model_src_for(name)?; // no source at all → cache the failure
    let read = |f: &str| model_file(name, f);
    let model = crate::model::obj::load_controller_model_with(&read, name.to_string()).ok()?;

    let mut parts = Vec::with_capacity(model.parts.len());
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    // Bounding box of the touchpad mesh itself — the true surface the finger
    // dots must stay within (far more reliable than baked-in numbers).
    let mut pad_min = Vec3::splat(f32::INFINITY);
    let mut pad_max = Vec3::splat(f32::NEG_INFINITY);
    let mut pad_extent_fallback: Option<TouchSurface> = None;
    let mut touch_point_parts: [Option<usize>; 2] = [None, None];
    for (idx, p) in model.parts.iter().enumerate() {
        let tf = part_transform(p.pos, p.rot);
        let lname = p.name.to_ascii_lowercase();
        let is_touchpad = lname == "touchpad" || lname.starts_with("touchpad");
        let v = &p.mesh.vertices;
        let mut i = 0;
        let mut pos_sum = Vec3::ZERO;
        let mut nrm_sum = Vec3::ZERO;
        let mut n_verts = 0u32;
        let mut p_min = Vec3::splat(f32::INFINITY);
        let mut p_max = Vec3::splat(f32::NEG_INFINITY);
        while i + 5 < v.len() {
            let world = tf.transform_point3(Vec3::new(v[i], v[i + 1], v[i + 2]));
            min = min.min(world);
            max = max.max(world);
            p_min = p_min.min(world);
            p_max = p_max.max(world);
            if is_touchpad {
                pad_min = pad_min.min(world);
                pad_max = pad_max.max(world);
            }
            pos_sum += world;
            nrm_sum += tf.transform_vector3(Vec3::new(v[i + 3], v[i + 4], v[i + 5]));
            n_verts += 1;
            i += 6;
        }
        // Horizontal footprint (assembled space, Y up) — scales the press
        // travel so tiny buttons (Home/Capture/±) barely sink while the big
        // face buttons keep their full travel.
        let footprint = if n_verts > 0 {
            (p_max.x - p_min.x).max(p_max.z - p_min.z)
        } else {
            0.0
        };
        let centroid = if n_verts > 0 { pos_sum / n_verts as f32 } else { Vec3::ZERO };
        let avg_normal = if nrm_sum.length_squared() > 1e-8 {
            nrm_sum.normalize()
        } else {
            Vec3::ZERO
        };
        // Record the movable touch-point dots (+ a fallback surface from the
        // extents baked into their info.txt block, used only if the model has
        // no touchpad mesh to measure).
        if lname.starts_with("touch_point") {
            let slot = if lname.ends_with('2') { 1 } else { 0 };
            touch_point_parts[slot] = Some(idx);
            if pad_extent_fallback.is_none() {
                if let Some(half) = p.extent {
                    pad_extent_fallback = Some(TouchSurface { center: p.pos, half });
                }
            }
        }
        // Surface samples for the x-ray visibility test: stride the vertex list
        // so the points spread over the whole part instead of clustering in
        // whichever region the exporter happened to emit first.
        let samples = {
            let n_v = v.len() / 6;
            let stride = (n_v / VIS_SAMPLES_PER_PART).max(1);
            (0..n_v)
                .step_by(stride)
                .take(VIS_SAMPLES_PER_PART)
                .map(|k| {
                    let b = k * 6;
                    (
                        Vec3::new(v[b], v[b + 1], v[b + 2]),
                        Vec3::new(v[b + 3], v[b + 4], v[b + 5]),
                    )
                })
                .collect()
        };
        parts.push(PartData {
            name: p.name.clone(),
            vertices: p.mesh.vertices.clone(),
            tri_count: p.mesh.tri_count,
            transform: tf,
            group: crate::model::material::group_for_part(&p.name) as usize,
            centroid,
            avg_normal,
            footprint,
            samples,
        });
    }

    // Prefer the measured touchpad box; fall back to the info.txt extents.
    let touch_surface = if pad_min.x.is_finite() && pad_max.x.is_finite() {
        Some(TouchSurface {
            center: (pad_min + pad_max) * 0.5,
            half: glam::Vec2::new((pad_max.x - pad_min.x) * 0.5, (pad_max.z - pad_min.z) * 0.5),
        })
    } else {
        pad_extent_fallback
    };

    let (center, radius) = if min.x.is_finite() && max.x.is_finite() {
        let c = (min + max) * 0.5;
        ((c), (max - c).length().max(1e-3))
    } else {
        (Vec3::ZERO, 1.0)
    };

    Some(Arc::new(LoadedModel {
        name: name.to_string(),
        parts,
        center,
        radius,
        touch_surface,
        touch_point_parts,
    }))
}

/// Best-guess model folder for a connected device id, used when the node's
/// model override is left on "auto". Falls back to any available model when the
/// preferred one isn't present.
pub fn model_for_device(dev_id: &str) -> String {
    let want = match skin_from_device_id(dev_id) {
        Skin::Playstation => "DualSense",
        Skin::SwitchPro => "Switch Pro",
        _ => "Xbox One",
    };
    let avail = available_models();
    if avail.iter().any(|m| m == want) {
        want.to_string()
    } else {
        avail.into_iter().next().unwrap_or_else(|| want.to_string())
    }
}

// ── Render callback ───────────────────────────────────────────────────────────

/// Marks which model the cached GPU buffers belong to, so buffers are rebuilt
/// when the node switches models.
struct BuffersKey(String);

/// A complete 3D mesh render callback built from a loaded model + orientation.
pub struct MeshRenderState {
    pub model: Arc<LoadedModel>,
    /// Whole-assembly orientation (from gyro quaternion integration).
    pub orientation: Quat,
    /// The node's FULL intended rect in egui points. Camera aspect + framing use
    /// this, so the model keeps the size it would have if fully on-screen.
    pub full_rect: egui::Rect,
    /// The VISIBLE sub-rect actually painted into (full ∩ clip). Rendering into
    /// this sub-region with the full-rect projection makes the model CROP at the
    /// frame edge (like any other UI element) instead of shrinking to fit. Equal
    /// to `full_rect` when the node is entirely on-screen.
    pub vis_rect: egui::Rect,
    /// Per-group base colours (linear-ish 0..1 RGBA — alpha < 1 renders the
    /// group as translucent plastic), indexed by `PartData.group`.
    pub scheme: [[f32; 4]; crate::model::material::N_GROUPS],
    /// Whole-model opacity (0..1) for the 2D composite (overlay transparency).
    pub global_alpha: f32,
    /// Camera elevation above the horizontal, in radians (0 = level/front view,
    /// larger = more overhead). Set from the viewer's `cam_pitch` param.
    pub cam_pitch: f32,
    /// Live input state: touch dots, stick tilt, trigger pull, button presses.
    pub live: ControllerLive,
    /// Widget composite alpha (0..1): fades the whole rendered controller as a
    /// 2D image (overlay style), independent of `global_alpha` see-through.
    pub composite: f32,
    /// Shared render pipeline — created lazily on first prepare().
    pub pipeline: Option<Arc<ControllerPipeline>>,
}

/// X-ray draw lists, computed in `prepare` and consumed in `paint` via the
/// shared `CallbackResources` (the same single-model slot the GPU buffers
/// use). `ghosts` are the off-view active parts in painter's order (farthest
/// first — the ghost pass has no depth writes). `restore` are ALL highlighted
/// parts, re-drawn after the ghosts with depth LessEqual so their visible
/// surfaces reclaim their pixels — a ghost pierces the inert shell but never
/// covers a nearer highlighted input (z-order among highlighted objects).
/// `translucent` are the parts whose material alpha < 1, excluded from the
/// opaque pass and drawn far→near with blending (depth-tested, no writes).
struct XrayOrder {
    ghosts: Vec<usize>,
    restore: Vec<usize>,
    translucent: Vec<usize>,
}

/// Offscreen target for the widget matte: when composite alpha < 1 the
/// controller renders into this texture in `prepare`, and `paint` composites
/// the finished 2D image at the matte alpha (a true image fade — no
/// see-through layering artifacts).
struct MatteTarget {
    color_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    size: (u32, u32),
    alpha_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// Whether this frame's `paint` should composite from the matte target.
struct MatteActive(bool);

/// Ghost-gate threshold on the measured `visible / total` sample ratio. The
/// X-ray ghost gating with hysteresis (kills the strobe when a part sits at the
/// edge of occlusion): an active part ENTERS x-ray only once its smoothed
/// visibility falls below LOW, and LEAVES it once back above HIGH.
///
/// The measured fraction counts only CAMERA-FACING surface samples, so a fully
/// unobstructed part reads ≈ 1.0 and the numbers mean what the spec says
/// literally: ghost below ~5% of the part's facing surface visible, un-ghost
/// once back over ~12%. (The older occlusion-query measure counted front and
/// back faces alike and so topped out near 0.5, which is why its thresholds
/// looked half this size.)
const GHOST_VIS_LOW: f32 = 0.05;
const GHOST_VIS_HIGH: f32 = 0.12;
/// EMA weight applied to each raw visibility readback — damps the measurement
/// noise (already ~3 frames behind) that made the ghost flicker in and out.
const VIS_SMOOTH: f32 = 0.35;

/// Async-readback state for the visibility measurement.
#[derive(Clone, Copy, PartialEq)]
enum VisMapState {
    /// No measurement in flight — record one this frame.
    Idle,
    /// Depth prepass copied to the staging buffer (submits with egui's frame);
    /// map it next frame.
    Copied,
    /// `map_async` registered; waiting for the device poll to complete it.
    Mapping,
    /// Staging buffer mapped — read the counts, then back to `Idle`.
    Ready,
}

/// Side of the offscreen visibility target, in pixels. The measure is a ratio
/// of sample counts, so it is resolution-independent; this only has to be fine
/// enough that a small part still covers several pixels. 256 keeps the readback
/// at 256 KB and its row pitch (1024 B) already a multiple of wgpu's 256-byte
/// `bytes_per_row` alignment, so no padding arithmetic is needed.
const VIS_RES: u32 = 256;

/// Everything the CPU-side visibility test needs from the frame whose depth
/// prepass is being read back.
struct VisPose {
    /// View-projection at record time (`crop * proj * view`).
    view_proj: Mat4,
    /// Per-part model matrix at record time — the animated `model_m` the
    /// renderer used, so a depressed button is tested where it was drawn.
    part_model: Vec<Mat4>,
    /// Camera position, for the camera-facing test.
    cam: Vec3,
    /// Depth tolerance in NDC units, derived from the projection rather than
    /// guessed: the depth delta a small world-space step toward the camera
    /// produces near the model centre. A part's own samples sit exactly on the
    /// surface the prepass recorded, so they need only survive float rounding,
    /// while anything genuinely behind the shell is further away by far more
    /// than this.
    depth_eps: f32,
}

/// `(unobstructed, camera-facing-and-in-frame)` sample counts for one part,
/// judged against `depth` — the prepass image, row-major and `VIS_RES` square.
///
/// Raw counts rather than a ratio, because parts are judged in occlusion
/// GROUPS and the group's verdict has to weigh each mesh by how much surface it
/// actually presents. See `object_visibility_fractions`.
fn part_visibility_counts(
    part: &PartData,
    model: Mat4,
    pose: &VisPose,
    depth: &[f32],
) -> (u32, u32) {
    let res = VIS_RES as usize;
    let mvp = pose.view_proj * model;
    let (mut visible, mut total) = (0u32, 0u32);
    for (p_local, n_local) in &part.samples {
        let world = model.transform_point3(*p_local);
        // Away-facing samples describe the part's far side, which its own body
        // hides wherever the camera stands. Counting them would peg every
        // fraction near one half and make the thresholds meaningless.
        if model.transform_vector3(*n_local).dot(pose.cam - world) <= 0.0 {
            continue;
        }
        let clip = mvp * p_local.extend(1.0);
        if clip.w <= 1e-6 {
            continue; // at or behind the eye
        }
        let ndc = clip.truncate() / clip.w;
        if !(-1.0..=1.0).contains(&ndc.x) || !(-1.0..=1.0).contains(&ndc.y) {
            continue; // outside the frame
        }
        total += 1;
        let px = (((ndc.x * 0.5 + 0.5) * res as f32) as usize).min(res - 1);
        // NDC y points up; the depth image's rows run down.
        let py = (((0.5 - ndc.y * 0.5) * res as f32) as usize).min(res - 1);
        if ndc.z <= depth[py * res + px] + pose.depth_eps {
            visible += 1;
        }
    }
    (visible, total)
}

/// Visibility fraction per part, aggregated over its occlusion OBJECT (`obj`,
/// from `material::xray_object_for_part`).
///
/// Parts sharing an id — a stick's dome, cap and rim — are judged as ONE solid,
/// so the cap covering its own dome leaves cap+rim visible and the stick never
/// ghosts itself; only the whole assembly going behind the shell drops below
/// threshold. Everything else gets a unique id and measures alone.
///
/// The group's counts are SUMMED, not its fractions averaged. Averaging lets a
/// mesh that measured nothing at all (turned away, off-screen — reported as
/// fully visible, since there is no facing surface to reveal) count as an equal
/// vote against the meshes that did measure, and a stick rotated away from the
/// camera always has such a mesh. Summing weighs each mesh by the surface it
/// actually presents, so an unmeasurable one contributes nothing either way.
///
/// An object with no facing samples anywhere reports 1.0, which also makes the
/// measurement fail SAFE: if it ever stops producing data the model renders
/// normally, rather than every input turning permanently to glass the way a
/// silently-dead occlusion query did.
fn object_visibility_fractions(
    parts: &[PartData],
    obj: &[u32],
    pose: &VisPose,
    depth: &[f32],
) -> Vec<f32> {
    let key_of = |i: usize| obj.get(i).copied().unwrap_or(i as u32);
    let mut sums: std::collections::HashMap<u32, (u32, u32)> = std::collections::HashMap::new();
    for (i, part) in parts.iter().enumerate() {
        let model = pose.part_model.get(i).copied().unwrap_or(Mat4::IDENTITY);
        let (vis, tot) = part_visibility_counts(part, model, pose, depth);
        let e = sums.entry(key_of(i)).or_insert((0, 0));
        e.0 += vis;
        e.1 += tot;
    }
    (0..parts.len())
        .map(|i| match sums.get(&key_of(i)).copied().unwrap_or((0, 0)) {
            (_, 0) => 1.0,
            (vis, tot) => vis as f32 / tot as f32,
        })
        .collect()
}

/// Per-part visibility measurement: is this input in the camera's line of
/// sight, or hidden behind the controller body?
///
/// The GPU renders a depth prepass of the whole model into a small offscreen
/// target; that depth image is read back and every part's surface samples are
/// tested against it on the CPU. A sample counts toward the total if it faces
/// the camera and lands inside the frame, and counts as visible if its own
/// depth is no further than the depth already recorded at that pixel — i.e.
/// nothing else got there first. The ratio is the REAL visibility fraction,
/// part-vs-part occlusion included, and drives the "<10% visible → x-ray
/// ghost" rule.
///
/// This deliberately does NOT use occlusion queries, which is what the pass
/// originally did. They ask the driver the same question far more cheaply, but
/// when a driver stops answering it returns zero samples rather than an error —
/// indistinguishable at the call site from "completely hidden", so every input
/// silently ghosts forever. That happened on AMD across both DX12 and Vulkan
/// (see `tests/occlusion_query.rs`). Projecting points against a depth image
/// costs a readback and a few thousand transforms per measurement, and it
/// behaves identically on every backend.
///
/// Readback is async (~3 frames behind, invisible at highlight timescales).
struct VisMeasure {
    depth_tex: wgpu::Texture,
    depth_view: wgpu::TextureView,
    n_parts: usize,
    staging: wgpu::Buffer,
    /// Camera + per-part transforms captured when the prepass was RECORDED.
    /// The readback lands frames later, by which time the live matrices have
    /// moved on; testing against the current pose would compare sample
    /// positions to a depth image of a different one.
    pose: Arc<Mutex<Option<VisPose>>>,
    state: Arc<Mutex<VisMapState>>,
    /// Smoothed `visible / total` per part index (EMA over readbacks; empty
    /// until the first readback completes). Feeds the hysteresis latch below.
    fractions: Arc<Mutex<Vec<f32>>>,
    /// Latched "hidden" per part (hysteresis on `fractions`): true = currently
    /// x-ray-ghosted. Prevents strobing at the LOW/HIGH visibility boundary.
    ghost: Arc<Mutex<Vec<bool>>>,
    /// Occlusion object id per part (`material::xray_object_for_part`). Parts
    /// sharing an id (the three-mesh sticks) are judged as ONE solid: their
    /// visible/total counts are summed before the fraction + latch, so a cap
    /// covering its own dome never ghosts the stick. Length == `n_parts`.
    obj: Vec<u32>,
    /// Model the samples and `obj` ids were built for — the buffers rebuild on
    /// a model swap and this must follow, or parts get judged against another
    /// controller's geometry.
    model: String,
    /// Consecutive frames spent in `Mapping` — the map_async callback only
    /// fires on device maintenance, and if it's ever lost the machine would
    /// wedge and the ghost gating would keep judging visibility from a STALE
    /// pose (x-ray firing for a camera angle the model left long ago). The
    /// watchdog cancels and restarts the readback instead.
    stalled_frames: std::sync::atomic::AtomicU32,
}

impl CallbackTrait for MeshRenderState {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &Queue,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // ── Pipeline (once) ────────────────────────────────────────────────
        let pipeline = match callback_resources.get::<Arc<ControllerPipeline>>() {
            Some(p) => p.clone(),
            None => {
                // Use the format eframe actually negotiated (captured at
                // startup). Hardcoding it desyncs from the render pass on
                // backends whose surface isn't sRGB (e.g. DX12 → Bgra8Unorm).
                let pipeline = Arc::new(ControllerPipeline::new(
                    device,
                    target_format(),
                    1,
                ));
                callback_resources.insert(pipeline.clone());
                pipeline
            }
        };

        // ── Per-part GPU buffers (rebuilt when the model changes) ──────────
        let need_rebuild = callback_resources
            .get::<BuffersKey>()
            .map(|k| k.0 != self.model.name)
            .unwrap_or(true);
        if need_rebuild {
            let defaults = Uniforms::default_uniform();
            let parts: Vec<PartBuffers> = self
                .model
                .parts
                .iter()
                .map(|pd| PartBuffers::new(device, &pipeline, &pd.vertices, &defaults))
                .collect();
            callback_resources.insert(parts);
            callback_resources.insert(BuffersKey(self.model.name.clone()));
        }

        let gpu_parts = match callback_resources.get::<Vec<PartBuffers>>() {
            Some(parts) => parts.as_slice(),
            None => return Vec::new(),
        };
        if gpu_parts.is_empty() {
            return Vec::new();
        }

        // ── Camera: frame the model's bounding sphere ──────────────────────
        // Aspect + framing use the FULL intended rect, so the model keeps a
        // stable size regardless of how much is currently visible.
        let fw = self.full_rect.width().max(1.0);
        let fh = self.full_rect.height().max(1.0);
        let aspect = (fw / fh).max(1e-3);

        let fov_y: f32 = 45.0_f32.to_radians();
        // The limiting FOV is the smaller of vertical/horizontal, so the model
        // fits a tall OR wide node body without clipping.
        let fov_x = 2.0 * ((fov_y * 0.5).tan() * aspect).atan();
        let limiting = fov_y.min(fov_x).max(1e-3);

        let center = self.model.center;
        let radius = self.model.radius;
        let dist = (radius / (limiting * 0.5).sin()) * 1.3; // 1.3 = breathing room
        let near = (dist - radius).max(0.01);
        let proj = Mat4::perspective_infinite_rh(fov_y, aspect, near);

        // Camera orbits the model centre at `cam_pitch` above the horizontal
        // (0 = level/front, larger = more overhead). Clamped below 90° so the
        // up-vector never becomes parallel to the view direction.
        let pitch = self.cam_pitch.clamp(-1.4, 1.4);
        let cam = center + dist * Vec3::new(0.0, pitch.sin(), pitch.cos());
        let view = Mat4::look_at_rh(cam, center, Vec3::Y);

        // Crop matrix: we paint into `vis_rect` (the visible sub-rect), but the
        // projection above is framed for `full_rect`. Map the full-rect render so
        // that only the `vis_rect` sub-region fills the viewport — i.e. the model
        // CROPS at the frame edge at full size, instead of shrinking to fit.
        // Fractions come from vis-within-full (both in the SAME egui-point space,
        // so the snarl's pan/zoom layer transform cancels out — no screen coords
        // needed). Identity when the node is fully visible.
        let fx0 = ((self.vis_rect.min.x - self.full_rect.min.x) / fw).clamp(0.0, 1.0);
        let fx1 = ((self.vis_rect.max.x - self.full_rect.min.x) / fw).clamp(0.0, 1.0);
        let fy0 = ((self.vis_rect.min.y - self.full_rect.min.y) / fh).clamp(0.0, 1.0);
        let fy1 = ((self.vis_rect.max.y - self.full_rect.min.y) / fh).clamp(0.0, 1.0);
        let hx = (fx1 - fx0).max(1e-4);
        let hy = (fy1 - fy0).max(1e-4);
        // Affine on clip coords (clip'.x = sx*clip.x + tx*clip.w): zoom the
        // visible band [2fx0-1, 2fx1-1] → [-1, 1]; y is pixel-down / NDC-up so
        // its band is [1-2fy1, 1-2fy0] → [-1, 1].
        let sx = 1.0 / hx;
        let tx = -(fx0 + fx1 - 1.0) / hx;
        let sy = 1.0 / hy;
        let ty = (fy0 + fy1 - 1.0) / hy;
        let crop = Mat4::from_cols(
            glam::Vec4::new(sx, 0.0, 0.0, 0.0),
            glam::Vec4::new(0.0, sy, 0.0, 0.0),
            glam::Vec4::new(0.0, 0.0, 1.0, 0.0),
            glam::Vec4::new(tx, ty, 0.0, 1.0),
        );

        // The shader computes clip = mvp * model * pos, so `mvp` is view-proj
        // ONLY — the per-part model matrix carries the object transform.
        let view_proj = crop * proj * view;

        // Orientation rotates the whole assembly about its centre.
        let orient = Mat4::from_translation(center)
            * Mat4::from_quat(self.orientation)
            * Mat4::from_translation(-center);

        let tp = self.model.touch_point_parts;
        let surf = self.model.touch_surface;
        let hl = self.live.highlight;
        let hl4 = [hl[0], hl[1], hl[2], 1.0];
        let cam_pos4 = [cam.x, cam.y, cam.z, 1.0]; // w unused (matte is a blit pass)
        // Latest measured per-part visibility (occlusion queries, ~3 frames
        // behind). Missing data (first frames / new model) = fully visible,
        // so nothing ghosts until real measurements arrive.
        // Latched per-part "hidden" flags (smoothing + hysteresis applied at
        // readback) — an active part that's hidden shows the x-ray ghost.
        let ghost_hidden: Vec<bool> = callback_resources
            .get::<VisMeasure>()
            .and_then(|v| v.ghost.lock().ok().map(|g| g.clone()))
            .unwrap_or_default();
        let center_radius4 = [center.x, center.y, center.z, radius];
        let mut ghosts: Vec<(usize, f32)> = Vec::new(); // (part idx, camera distance)
        let mut restore: Vec<usize> = Vec::new(); // highlighted parts (re-drawn after ghosts)
        // Parts with a translucent material (scheme alpha < 1): excluded from
        // the opaque pass and re-drawn sorted far→near with blending.
        let mut translucent: Vec<(usize, f32)> = Vec::new();
        // Per-part model matrices, in draw order — handed to the visibility
        // measurement below so it tests the pose that was actually rendered.
        let mut part_model: Vec<Mat4> = Vec::with_capacity(gpu_parts.len());
        for (i, gpu_part) in gpu_parts.iter().enumerate() {
            let part = self.model.parts.get(i);
            let part_tf = part.map(|p| p.transform).unwrap_or(Mat4::IDENTITY);
            let g = part.map(|p| p.group).unwrap_or(0);
            let base = self.scheme.get(g).copied().unwrap_or([0.5, 0.5, 0.5, 1.0]);
            let name = part.map(|p| p.name.as_str()).unwrap_or("");

            // Touch-point dots: hidden when the finger is up; otherwise slid to
            // the mapped position on the touchpad surface and highlighted.
            let touch_slot = if Some(i) == tp[0] {
                Some(0)
            } else if Some(i) == tp[1] {
                Some(1)
            } else {
                None
            };
            let (model_m, base_color, glow_color, glow) = match touch_slot {
                Some(k) => match (self.live.touch[k], surf) {
                    (Some(uv), Some(s)) => {
                        // `uv` is already normalized [0,1] (x: left→right,
                        // y: top→bottom). Map onto the measured pad box, pulled in
                        // ~10% so a full-scale touch stays on the pad. If Y still
                        // reads inverted, negate the `dz` term.
                        const PAD_INSET: f32 = 0.9;
                        let dx = (uv.x * 2.0 - 1.0) * s.half.x * PAD_INSET;
                        let dz = (uv.y * 2.0 - 1.0) * s.half.y * PAD_INSET;
                        let m = orient * Mat4::from_translation(Vec3::new(dx, 0.0, dz)) * part_tf;
                        (m, hl4, hl4, 0.6)
                    }
                    // Hidden: collapse to a zero-area point (draws nothing).
                    _ => (Mat4::from_scale(Vec3::splat(0.0)), [0.0; 4], [0.0; 4], 0.0),
                },
                None => {
                    // Animate presses/tilts/pulls via the part's extra transform.
                    let fp = part.map(|p| p.footprint).unwrap_or(0.0);
                    let model_m = orient * self.live.part_xform(name, &part_tf, fp) * part_tf;
                    // Relay the live LED colour onto the LED-strip group (emissive).
                    let is_led = g == crate::model::material::MaterialGroup::Led as usize;
                    match (is_led, self.live.led) {
                        // The live LED blends ADDITIVELY into the base strip
                        // colour (cloudy grey plastic lit from within), so an
                        // unlit relay (0,0,0) shows the base colour — not black.
                        (true, Some(c)) => {
                            let b = [
                                (base[0] + c[0]).min(1.0),
                                (base[1] + c[1]).min(1.0),
                                (base[2] + c[2]).min(1.0),
                            ];
                            let inten = c[0].max(c[1]).max(c[2]).clamp(0.0, 1.0);
                            (
                                model_m,
                                [b[0], b[1], b[2], base[3]],
                                [c[0], c[1], c[2], 1.0],
                                0.85 * inten,
                            )
                        }
                        _ => {
                            // Highlight active inputs (albedo shifts toward the
                            // style accent; shading is preserved in the shader).
                            let g = self.live.glow.get(name).copied().unwrap_or(0.0);
                            (model_m, base, hl4, g)
                        }
                    }
                }
            };
            let uniforms = Uniforms {
                mvp: view_proj.to_cols_array_2d(),
                model: model_m.to_cols_array_2d(),
                base_color,
                glow_color,
                glow,
                global_alpha: self.global_alpha,
                _pad0: [0.0; 2],
                cam_pos: cam_pos4,
                center_radius: center_radius4,
            };
            gpu_part.update_uniforms(queue, &uniforms);
            // Same matrix the draw uses, kept for the visibility test so its
            // sample points land exactly where the part is actually rendered.
            part_model.push(model_m);

            // X-ray ghost: an ACTIVE part that is measurably out of view —
            // under ~10% of its facing surface visible per the depth-prepass
            // measurement (real per-pixel occlusion, so a camera-facing
            // d-pad hidden behind the trigger counts as hidden) — is re-drawn
            // where OCCLUDED (depth Greater) as a strong accent ghost. Parts
            // that are meaningfully visible keep only the normal highlight.
            let xg = match touch_slot {
                // Touch dots aren't in the glow map (it's keyed by button/stick
                // pins) — an active finger counts as fully highlighted, so a
                // dot on an away-facing touchpad ghosts like any other input.
                Some(k) => {
                    if self.live.touch[k].is_some() {
                        1.0
                    } else {
                        0.0
                    }
                }
                None => part
                    .map(|p| self.live.glow.get(&p.name).copied().unwrap_or(0.0))
                    .unwrap_or(0.0),
            };
            let ghosted = xg > 0.02 && ghost_hidden.get(i).copied().unwrap_or(false);
            if xg > 0.02 {
                restore.push(i);
            }
            let dist = part
                .map(|p| {
                    let c = self.orientation * (p.centroid - center) + center;
                    (cam - c).length()
                })
                .unwrap_or(0.0);
            if touch_slot.is_none() && base_color[3] < 0.999 {
                translucent.push((i, dist));
            }
            if ghosted {
                let ghost = Uniforms {
                    mvp: view_proj.to_cols_array_2d(),
                    model: model_m.to_cols_array_2d(),
                    // Strong ghost: high alpha + full emissive so it cuts
                    // through the shell colour instead of blending dirty.
                    base_color: [hl[0], hl[1], hl[2], 0.9 * xg],
                    glow_color: hl4,
                    glow: 0.85,
                    global_alpha: 1.0,
                    _pad0: [0.0; 2],
                    cam_pos: cam_pos4,
                    center_radius: center_radius4,
                };
                gpu_part.update_xray_uniforms(queue, &ghost);
                ghosts.push((i, dist));
            }
        }
        // Painter's order for the ghost + translucent passes: far → near.
        ghosts.sort_by(|a, b| b.1.total_cmp(&a.1));
        translucent.sort_by(|a, b| b.1.total_cmp(&a.1));
        let trans_order: Vec<usize> = translucent.into_iter().map(|(i, _)| i).collect();
        let is_trans: Vec<bool> = {
            let mut v = vec![false; gpu_parts.len()];
            for &i in &trans_order {
                if let Some(s) = v.get_mut(i) {
                    *s = true;
                }
            }
            v
        };
        let ghost_order: Vec<usize> = ghosts.into_iter().map(|(i, _)| i).collect();

        // ── Widget matte (composite alpha) ─────────────────────────────────
        // Render the whole controller into an offscreen texture now; `paint`
        // composites the finished image at the matte alpha. Skipped entirely
        // (direct draw) when fully opaque. Mutable resource setup happens
        // FIRST (ends the `gpu_parts` borrow), then the pass re-borrows.
        let use_matte = self.composite < 0.999;
        if use_matte {
            if callback_resources.get::<Arc<BlitPipeline>>().is_none() {
                let b = Arc::new(BlitPipeline::new(device, target_format(), 1));
                callback_resources.insert(b);
            }
            let blit = callback_resources
                .get::<Arc<BlitPipeline>>()
                .expect("just inserted")
                .clone();
            let ppp = screen_descriptor.pixels_per_point;
            let w = ((self.vis_rect.width() * ppp).round() as u32).max(1);
            let h = ((self.vis_rect.height() * ppp).round() as u32).max(1);
            let rebuild = callback_resources
                .get::<MatteTarget>()
                .map(|m| m.size != (w, h))
                .unwrap_or(true);
            if rebuild {
                let mk_tex = |label: &str, format, usage| {
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage,
                        view_formats: &[],
                    })
                };
                let color = mk_tex(
                    "c3d_matte_color",
                    target_format(),
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                );
                let depth = mk_tex(
                    "c3d_matte_depth",
                    crate::model::pipeline::CONTROLLER_DEPTH_FORMAT,
                    wgpu::TextureUsages::RENDER_ATTACHMENT,
                );
                let color_view = color.create_view(&Default::default());
                let depth_view = depth.create_view(&Default::default());
                let alpha_buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("c3d_matte_alpha"),
                    size: 16,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("c3d_matte_bg"),
                    layout: &blit.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&color_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&blit.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: alpha_buf.as_entire_binding(),
                        },
                    ],
                });
                callback_resources.insert(MatteTarget {
                    color_view,
                    depth_view,
                    size: (w, h),
                    alpha_buf,
                    bind_group,
                });
            }
            // Immutable phase: re-borrow the parts + target and record the pass.
            let gpu_parts = match callback_resources.get::<Vec<PartBuffers>>() {
                Some(parts) => parts.as_slice(),
                None => return Vec::new(),
            };
            if let Some(mt) = callback_resources.get::<MatteTarget>() {
                queue.write_buffer(
                    &mt.alpha_buf,
                    0,
                    bytemuck::bytes_of(&[self.composite.clamp(0.0, 1.0), 0.0, 0.0, 0.0]),
                );
                let mut pass = _egui_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("c3d_matte_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &mt.color_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &mt.depth_view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Discard,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&pipeline.pipeline);
                for (i, gpu_part) in gpu_parts.iter().enumerate() {
                    if is_trans.get(i).copied().unwrap_or(false) {
                        continue; // drawn blended below
                    }
                    gpu_part.draw(&mut pass);
                }
                if !trans_order.is_empty() {
                    // Translucent materials: far → near over the opaque depth.
                    pass.set_pipeline(&pipeline.translucent);
                    for &i in &trans_order {
                        if let Some(gp) = gpu_parts.get(i) {
                            gp.draw(&mut pass);
                        }
                    }
                }
                if !ghost_order.is_empty() {
                    pass.set_pipeline(&pipeline.xray);
                    for &i in &ghost_order {
                        if let Some(gp) = gpu_parts.get(i) {
                            gp.draw_xray(&mut pass);
                        }
                    }
                    // Highlighted visible surfaces reclaim their pixels.
                    pass.set_pipeline(&pipeline.restore);
                    for &i in &restore {
                        if let Some(gp) = gpu_parts.get(i) {
                            gp.draw(&mut pass);
                        }
                    }
                }
            }
        }
        // ── Visibility measurement (depth prepass + CPU sample test) ───────
        // One measurement in flight at a time: record → (egui submits) → map →
        // read → record again. Runs at a fraction of the frame rate, which is
        // plenty — visibility changes with orientation, not per frame.
        let n_parts = self.model.parts.len();
        let vis_rebuild = callback_resources
            .get::<VisMeasure>()
            .map(|v| v.n_parts != n_parts || v.model != self.model.name)
            .unwrap_or(true);
        if vis_rebuild && n_parts > 0 {
            let depth = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("c3d_vis_depth"),
                size: wgpu::Extent3d {
                    width: VIS_RES,
                    height: VIS_RES,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: crate::model::pipeline::CONTROLLER_DEPTH_FORMAT,
                // COPY_SRC: the depth image itself is the measurement, so it
                // has to come back to the CPU.
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("c3d_vis_staging"),
                size: (VIS_RES * VIS_RES * 4) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            callback_resources.insert(VisMeasure {
                depth_view: depth.create_view(&Default::default()),
                depth_tex: depth,
                n_parts,
                staging,
                model: self.model.name.clone(),
                pose: Arc::new(Mutex::new(None)),
                state: Arc::new(Mutex::new(VisMapState::Idle)),
                fractions: Arc::new(Mutex::new(Vec::new())),
                ghost: Arc::new(Mutex::new(Vec::new())),
                obj: self
                    .model
                    .parts
                    .iter()
                    .enumerate()
                    .map(|(i, p)| crate::model::material::xray_object_for_part(&p.name, i))
                    .collect(),
                stalled_frames: std::sync::atomic::AtomicU32::new(0),
            });
        }
        if n_parts > 0 {
            // Immutable phase: advance the readback state machine / record.
            let gpu_parts = match callback_resources.get::<Vec<PartBuffers>>() {
                Some(parts) => parts.as_slice(),
                None => return Vec::new(),
            };
            if let Some(vm) = callback_resources.get::<VisMeasure>() {
                let st = *vm.state.lock().unwrap();
                match st {
                    VisMapState::Ready => {
                        // The pose is only missing if a model swap landed
                        // between record and readback — the depth image then
                        // belongs to different geometry, so drop it.
                        if let Some(pose) = vm.pose.lock().unwrap().take() {
                            let data = vm.staging.slice(..).get_mapped_range();
                            let depth: &[f32] = bytemuck::cast_slice(&data);
                            let measured = object_visibility_fractions(
                                &self.model.parts,
                                &vm.obj,
                                &pose,
                                depth,
                            );
                            let mut fr = vm.fractions.lock().unwrap();
                            let mut gh = vm.ghost.lock().unwrap();
                            fr.resize(vm.n_parts, 1.0);
                            gh.resize(vm.n_parts, false);
                            for i in 0..vm.n_parts {
                                let frac = measured.get(i).copied().unwrap_or(1.0);
                                // Smooth the raw fraction, then latch hidden/visible
                                // with hysteresis so an edge-of-occlusion part holds.
                                fr[i] = fr[i] * (1.0 - VIS_SMOOTH) + frac * VIS_SMOOTH;
                                gh[i] = if gh[i] { fr[i] < GHOST_VIS_HIGH } else { fr[i] < GHOST_VIS_LOW };
                            }
                        }
                        vm.staging.unmap();
                        *vm.state.lock().unwrap() = VisMapState::Idle;
                    }
                    VisMapState::Copied => {
                        // Mark Mapping BEFORE registering, so a synchronous
                        // completion can't be overwritten.
                        *vm.state.lock().unwrap() = VisMapState::Mapping;
                        vm.stalled_frames.store(0, std::sync::atomic::Ordering::Relaxed);
                        let state = vm.state.clone();
                        vm.staging.slice(..).map_async(wgpu::MapMode::Read, move |res| {
                            let mut s = state.lock().unwrap();
                            *s = if res.is_ok() { VisMapState::Ready } else { VisMapState::Idle };
                        });
                    }
                    VisMapState::Mapping => {
                        // The callback only runs on device maintenance — pump
                        // it explicitly so the readback can't depend on the
                        // frame loop happening to poll for us.
                        let _ = device.poll(wgpu::PollType::Poll);
                        // Watchdog: a lost callback would freeze the fractions
                        // at a stale pose forever (x-ray judged from a camera
                        // angle the model left minutes ago). Cancel + restart.
                        let stalled = vm.stalled_frames
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if stalled > 180 {
                            vm.staging.unmap(); // cancels the pending map_async
                            *vm.state.lock().unwrap() = VisMapState::Idle;
                            vm.stalled_frames.store(0, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    VisMapState::Idle => {
                        // Depth prepass: the whole model into the small target.
                        {
                            let mut pass =
                                _egui_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                    label: Some("c3d_vis_prepass"),
                                    color_attachments: &[],
                                    depth_stencil_attachment: Some(
                                        wgpu::RenderPassDepthStencilAttachment {
                                            view: &vm.depth_view,
                                            depth_ops: Some(wgpu::Operations {
                                                load: wgpu::LoadOp::Clear(1.0),
                                                store: wgpu::StoreOp::Store,
                                            }),
                                            stencil_ops: None,
                                        },
                                    ),
                                    timestamp_writes: None,
                                    occlusion_query_set: None,
                                });
                            pass.set_pipeline(&pipeline.vis_prepass);
                            for gpu_part in gpu_parts {
                                gpu_part.draw(&mut pass);
                            }
                        }
                        // Pull the finished depth image back for the CPU test.
                        _egui_encoder.copy_texture_to_buffer(
                            wgpu::TexelCopyTextureInfo {
                                texture: &vm.depth_tex,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::DepthOnly,
                            },
                            wgpu::TexelCopyBufferInfo {
                                buffer: &vm.staging,
                                layout: wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    // VIS_RES * 4 bytes: already 256-aligned.
                                    bytes_per_row: Some(VIS_RES * 4),
                                    rows_per_image: Some(VIS_RES),
                                },
                            },
                            wgpu::Extent3d {
                                width: VIS_RES,
                                height: VIS_RES,
                                depth_or_array_layers: 1,
                            },
                        );
                        // Freeze the pose this image was rendered from — the
                        // readback lands frames later against moved matrices.
                        let toward_cam = (cam - center).normalize_or_zero();
                        let d0 = view_proj.project_point3(center).z;
                        let d1 = view_proj.project_point3(center + toward_cam * radius * 0.02).z;
                        *vm.pose.lock().unwrap() = Some(VisPose {
                            view_proj,
                            part_model: part_model.clone(),
                            cam,
                            depth_eps: (d0 - d1).abs().max(1e-6),
                        });
                        *vm.state.lock().unwrap() = VisMapState::Copied;
                    }
                }
            }
        }

        callback_resources.insert(MatteActive(use_matte));
        callback_resources.insert(XrayOrder {
            ghosts: ghost_order,
            restore,
            translucent: trans_order,
        });

        Vec::new() // write_buffer is immediate; no command buffers to submit
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    ) {
        let pipeline = match callback_resources.get::<Arc<ControllerPipeline>>() {
            Some(p) => p.clone(),
            None => return,
        };
        let gpu_parts = match callback_resources.get::<Vec<PartBuffers>>() {
            Some(parts) => parts.as_slice(),
            None => return,
        };

        let vp = info.viewport;
        if !vp.is_finite() || vp.width() <= 0.0 || vp.height() <= 0.0 {
            return;
        }

        // Widget matte path: the controller was already rendered offscreen in
        // `prepare`; composite that image at the matte alpha and stop.
        if callback_resources
            .get::<MatteActive>()
            .map(|m| m.0)
            .unwrap_or(false)
        {
            if let (Some(blit), Some(mt)) = (
                callback_resources.get::<Arc<BlitPipeline>>(),
                callback_resources.get::<MatteTarget>(),
            ) {
                render_pass.set_pipeline(&blit.pipeline);
                render_pass.set_bind_group(0, &mt.bind_group, &[]);
                render_pass.draw(0..3, 0..1);
                return;
            }
        }

        let xo = callback_resources.get::<XrayOrder>();
        let is_trans: Vec<bool> = {
            let mut v = vec![false; gpu_parts.len()];
            if let Some(xo) = xo {
                for &i in &xo.translucent {
                    if let Some(s) = v.get_mut(i) {
                        *s = true;
                    }
                }
            }
            v
        };
        render_pass.set_pipeline(&pipeline.pipeline);
        for (i, gpu_part) in gpu_parts.iter().enumerate() {
            if is_trans.get(i).copied().unwrap_or(false) {
                continue; // drawn blended below
            }
            gpu_part.draw(render_pass);
        }

        if let Some(xo) = xo {
            // Translucent materials: far → near over the opaque depth.
            if !xo.translucent.is_empty() {
                render_pass.set_pipeline(&pipeline.translucent);
                for &i in &xo.translucent {
                    if let Some(gp) = gpu_parts.get(i) {
                        gp.draw(render_pass);
                    }
                }
            }
            // X-ray pass: re-draw active off-view parts where they are OCCLUDED
            // (depth Greater) as accent ghosts, far → near. Then the highlighted
            // VISIBLE parts re-draw their front surfaces (LessEqual + bias), so a
            // ghost pierces the inert shell but never covers a nearer highlighted
            // input.
            if !xo.ghosts.is_empty() {
                render_pass.set_pipeline(&pipeline.xray);
                for &i in &xo.ghosts {
                    if let Some(gp) = gpu_parts.get(i) {
                        gp.draw_xray(render_pass);
                    }
                }
                render_pass.set_pipeline(&pipeline.restore);
                for &i in &xo.restore {
                    if let Some(gp) = gpu_parts.get(i) {
                        gp.draw(render_pass);
                    }
                }
            }
        }
    }
}

// ── Public paint API ──────────────────────────────────────────────────────────

/// Paint a controller `model` at `orientation`, tinted by `tint`, into
/// `vis_rect` while framing the camera for `full_rect` (so the model crops at
/// the frame edge rather than shrinking when partly scrolled off). Pass
/// `vis_rect == full_rect` when fully visible.
///
/// This does NOT allocate layout space — the caller reserves its own rect first
/// (matching the pinned-render pattern), so the same function serves the node
/// body and pinned/overlay instances. Model data is shared (`Arc`); GPU buffers
/// are cached across frames in the callback resources.
pub fn paint_controller_model(
    ui: &egui::Ui,
    vis_rect: egui::Rect,
    full_rect: egui::Rect,
    model: Arc<LoadedModel>,
    orientation: Quat,
    scheme: [[f32; 4]; crate::model::material::N_GROUPS],
    global_alpha: f32,
    cam_pitch: f32,
    live: ControllerLive,
    composite: f32,
) {
    let state = MeshRenderState {
        model,
        orientation,
        full_rect,
        vis_rect,
        scheme,
        global_alpha,
        cam_pitch,
        live,
        composite,
        pipeline: None,
    };
    let paint_callback = Callback::new_paint_callback(vis_rect, state);
    ui.painter_at(vis_rect)
        .add(egui::epaint::Shape::Callback(paint_callback));
}

#[cfg(test)]
mod vis_tests {
    use super::*;

    /// A flat 1×1 quad in the z=0 plane facing +Z, sampled on an 8×8 grid.
    fn facing_quad(flip_normals: bool) -> PartData {
        let n = if flip_normals { -Vec3::Z } else { Vec3::Z };
        let mut samples = Vec::new();
        for iy in 0..8 {
            for ix in 0..8 {
                let x = -0.5 + ix as f32 / 7.0;
                let y = -0.5 + iy as f32 / 7.0;
                samples.push((Vec3::new(x, y, 0.0), n));
            }
        }
        PartData {
            name: "quad".into(),
            vertices: Vec::new(),
            tri_count: 0,
            transform: Mat4::IDENTITY,
            group: 0,
            centroid: Vec3::ZERO,
            avg_normal: n,
            footprint: 1.0,
            samples,
        }
    }

    /// Camera on +Z looking at the origin — the quad faces it head on.
    fn pose(part_model: Mat4) -> VisPose {
        let cam = Vec3::new(0.0, 0.0, 3.0);
        let view = Mat4::look_at_rh(cam, Vec3::ZERO, Vec3::Y);
        let view_proj = Mat4::perspective_infinite_rh(45.0_f32.to_radians(), 1.0, 0.1) * view;
        // Same derivation as the renderer: the depth delta of a small step
        // toward the camera near the model centre.
        let d0 = view_proj.project_point3(Vec3::ZERO).z;
        let d1 = view_proj.project_point3(Vec3::Z * 0.01).z;
        VisPose {
            view_proj,
            part_model: vec![part_model],
            cam,
            depth_eps: (d0 - d1).abs().max(1e-6),
        }
    }

    /// The depth image a prepass of this part alone would produce: each sample's
    /// own depth at the pixel it lands on, far plane everywhere else.
    fn self_depth(part: &PartData, pose: &VisPose) -> Vec<f32> {
        let res = VIS_RES as usize;
        let mut depth = vec![1.0f32; res * res];
        let mvp = pose.view_proj * pose.part_model[0];
        for (p, _) in &part.samples {
            let clip = mvp * p.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            let px = (((ndc.x * 0.5 + 0.5) * res as f32) as usize).min(res - 1);
            let py = (((0.5 - ndc.y * 0.5) * res as f32) as usize).min(res - 1);
            depth[py * res + px] = depth[py * res + px].min(ndc.z);
        }
        depth
    }

    #[test]
    fn unobstructed_part_reads_fully_visible() {
        let part = facing_quad(false);
        let pose = pose(Mat4::IDENTITY);
        let depth = self_depth(&part, &pose);
        let f = object_visibility_fractions(std::slice::from_ref(&part), &[0], &pose, &depth);
        assert_eq!(f[0], 1.0, "a part alone in front of the camera is fully visible");
    }

    #[test]
    fn part_behind_a_nearer_surface_reads_hidden() {
        let part = facing_quad(false);
        let pose = pose(Mat4::IDENTITY);
        // Something solid much closer to the camera covers the whole frame.
        let depth = vec![0.0f32; (VIS_RES * VIS_RES) as usize];
        let f = object_visibility_fractions(std::slice::from_ref(&part), &[0], &pose, &depth);
        assert_eq!(f[0], 0.0, "fully covered part must read as hidden");
        assert!(f[0] < GHOST_VIS_LOW, "and must cross the ghost threshold");
    }

    #[test]
    fn half_covered_part_reads_about_half() {
        let part = facing_quad(false);
        let pose = pose(Mat4::IDENTITY);
        let res = VIS_RES as usize;
        let mut depth = self_depth(&part, &pose);
        // Occlude the right half of the frame.
        for y in 0..res {
            for x in res / 2..res {
                depth[y * res + x] = 0.0;
            }
        }
        let f = object_visibility_fractions(std::slice::from_ref(&part), &[0], &pose, &depth);
        assert!(
            (f[0] - 0.5).abs() < 0.1,
            "half-occluded part should read near 0.5, got {}",
            f[0]
        );
    }

    #[test]
    fn part_outside_the_frame_fails_safe_to_visible() {
        let part = facing_quad(false);
        // Push it far to the side: nothing projects inside the viewport.
        let pose = pose(Mat4::from_translation(Vec3::new(50.0, 0.0, 0.0)));
        let depth = vec![0.0f32; (VIS_RES * VIS_RES) as usize];
        let f = object_visibility_fractions(std::slice::from_ref(&part), &[0], &pose, &depth);
        assert_eq!(
            f[0], 1.0,
            "off-screen parts have nothing to reveal — ghosting them is noise"
        );
    }

    #[test]
    fn part_turned_away_fails_safe_to_visible() {
        // Every sample faces away from the camera, so none counts toward the
        // total. That must not read as "hidden" — there is no facing surface to
        // x-ray through in the first place.
        let part = facing_quad(true);
        let pose = pose(Mat4::IDENTITY);
        let depth = vec![0.0f32; (VIS_RES * VIS_RES) as usize];
        let f = object_visibility_fractions(std::slice::from_ref(&part), &[0], &pose, &depth);
        assert_eq!(f[0], 1.0);
    }

    /// The stick regression: a multi-mesh occlusion object whose meshes are all
    /// hidden, but where one presents no camera-facing surface at all and so
    /// measures nothing. Its "nothing to reveal -> visible" default must not
    /// vote against the meshes that did measure — averaging the two fractions
    /// gives 0.5, and the stick then never ghosts however far it rotates away.
    #[test]
    fn an_unmeasurable_mesh_does_not_rescue_its_hidden_group() {
        let hidden = facing_quad(false); // faces the camera, fully covered below
        let turned_away = facing_quad(true); // no facing samples: measures nothing
        let parts = [hidden, turned_away];
        let pose = VisPose {
            part_model: vec![Mat4::IDENTITY; 2],
            ..pose(Mat4::IDENTITY)
        };
        let depth = vec![0.0f32; (VIS_RES * VIS_RES) as usize];

        // Both meshes in ONE object, the way a stick's dome/cap/rim are.
        let grouped = object_visibility_fractions(&parts, &[7, 7], &pose, &depth);
        assert_eq!(
            grouped[0], 0.0,
            "group is entirely hidden; the mesh that measured nothing must not lift it"
        );
        assert!(grouped[0] < GHOST_VIS_LOW, "so the stick actually ghosts");

        // The same meshes as independent objects: the turned-away one still
        // reads visible alone, since there is genuinely nothing to x-ray.
        let separate = object_visibility_fractions(&parts, &[0, 1], &pose, &depth);
        assert_eq!(separate[0], 0.0);
        assert_eq!(separate[1], 1.0);
    }

    /// Grouping must weigh meshes by the surface they present, not one vote
    /// each: a large visible mesh should outweigh a small hidden one.
    #[test]
    fn group_fraction_weighs_meshes_by_presented_surface() {
        let big = facing_quad(false);
        // Far fewer samples, so it presents far less surface to the camera.
        let small = PartData {
            samples: big.samples.iter().take(6).copied().collect(),
            ..facing_quad(false)
        };
        let parts = [big, small];
        let pose = VisPose {
            part_model: vec![Mat4::IDENTITY; 2],
            ..pose(Mat4::IDENTITY)
        };
        // Depth showing the big mesh, with the small mesh's pixels covered.
        let res = VIS_RES as usize;
        let mut depth = self_depth(&parts[0], &pose);
        for (p, _) in &parts[1].samples {
            let clip = pose.view_proj * p.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            let px = (((ndc.x * 0.5 + 0.5) * res as f32) as usize).min(res - 1);
            let py = (((0.5 - ndc.y * 0.5) * res as f32) as usize).min(res - 1);
            depth[py * res + px] = -1.0; // something much nearer
        }
        let f = object_visibility_fractions(&parts, &[3, 3], &pose, &depth);
        assert!(
            f[0] > 0.8,
            "group dominated by its large visible mesh should read visible, got {}",
            f[0]
        );
        assert!(f[0] < 1.0, "the covered mesh must still count against it");
    }

    #[test]
    fn a_part_never_occludes_itself() {
        // The prepass contains this part's own depth, and the renderer animates
        // it with the very same matrix the test uses. Rotating it must not make
        // its own recorded surface read as an occluder.
        for deg in [0.0f32, 15.0, 30.0, 45.0, 60.0] {
            let part = facing_quad(false);
            let m = Mat4::from_rotation_y(deg.to_radians());
            let pose = pose(m);
            let depth = self_depth(&part, &pose);
            let f = object_visibility_fractions(std::slice::from_ref(&part), &[0], &pose, &depth);
            assert_eq!(f[0], 1.0, "self-occlusion at {deg}° — depth epsilon too tight");
        }
    }
}
