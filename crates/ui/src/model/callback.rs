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
use crate::model::{load_controller_model, part_transform};

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
}

/// Per-part mesh data carried from model loading into the callback.
pub struct PartData {
    /// Interleaved `[pos.x, pos.y, pos.z, norm.x, norm.y, norm.z]`.
    pub vertices: Vec<f32>,
    pub tri_count: usize,
    /// Static model-space transform (position + rotation from `info.txt`).
    pub transform: Mat4,
}

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
            cands.push(dir.join("app").join("assets").join("models"));
            if let Some(up) = dir.parent() {
                cands.push(up.join("app").join("assets").join("models"));
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        cands.push(cwd.join("app").join("assets").join("models"));
        cands.push(cwd.join("assets").join("models"));
    }
    cands.into_iter().find(|p| p.is_dir())
}

/// Names of every controller model folder available (those containing an
/// `info.txt`), sorted. Drives the node's model-override dropdown.
pub fn available_models() -> Vec<String> {
    let Some(base) = models_base_dir() else { return Vec::new(); };
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && p.join("info.txt").is_file() {
                if let Some(n) = e.file_name().to_str() {
                    out.push(n.to_string());
                }
            }
        }
    }
    out.sort();
    out
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
    let base = models_base_dir()?;
    let model = load_controller_model(&base.join(name)).ok()?;

    let mut parts = Vec::with_capacity(model.parts.len());
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for p in &model.parts {
        let tf = part_transform(p.pos, p.rot);
        let v = &p.mesh.vertices;
        let mut i = 0;
        while i + 5 < v.len() {
            let world = tf.transform_point3(Vec3::new(v[i], v[i + 1], v[i + 2]));
            min = min.min(world);
            max = max.max(world);
            i += 6;
        }
        parts.push(PartData {
            vertices: p.mesh.vertices.clone(),
            tri_count: p.mesh.tri_count,
            transform: tf,
        });
    }

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
    /// Shared render pipeline — created lazily on first prepare().
    pub pipeline: Option<Arc<ControllerPipeline>>,
}

impl CallbackTrait for MeshRenderState {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
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

        // View from +Z, slightly raised, looking at the model centre.
        let cam = center + Vec3::new(0.0, radius * 0.15, dist);
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

        for (i, gpu_part) in gpu_parts.iter().enumerate() {
            let part_tf = self.model.parts.get(i).map(|p| p.transform).unwrap_or(Mat4::IDENTITY);
            let model_m = orient * part_tf;
            let uniforms = Uniforms {
                mvp: view_proj.to_cols_array_2d(),
                model: model_m.to_cols_array_2d(),
                base_color: mesh_part_color(i),
                glow_color: [1.0, 0.3, 0.2, 1.0],
                glow: 0.0,
                _pad0: [0.0; 3],
            };
            gpu_part.update_uniforms(queue, &uniforms);
        }

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

        render_pass.set_pipeline(&pipeline.pipeline);
        for gpu_part in gpu_parts {
            gpu_part.draw(render_pass);
        }
    }
}

// ── Color palette helper ──────────────────────────────────────────────────────

/// A subtle per-part color so the assembly reads as distinct pieces.
fn mesh_part_color(index: usize) -> [f32; 4] {
    const PALETTE: [[f32; 4]; 6] = [
        [0.18, 0.18, 0.20, 1.0], // dark shell gray
        [0.15, 0.15, 0.17, 1.0], // darker inner parts
        [0.22, 0.22, 0.24, 1.0], // light gray plastic
        [0.30, 0.30, 0.32, 1.0], // mid-gray buttons
        [0.12, 0.12, 0.15, 1.0], // very dark rubber
        [0.25, 0.24, 0.26, 1.0], // soft gray
    ];
    PALETTE[index % PALETTE.len()]
}

// ── Public widget API ─────────────────────────────────────────────────────────

/// egui widget that renders a controller model (via a wgpu paint callback). It
/// paints into `vis_rect` (the visible sub-rect) but frames the camera for
/// `full_rect`, so the model crops at the frame edge rather than shrinking.
pub struct Controller3DWidget {
    /// Sub-rect actually painted into (the paint-callback / viewport rect).
    vis_rect: egui::Rect,
    state: MeshRenderState,
}

impl egui::Widget for Controller3DWidget {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        // Reserve the FULL rect for layout so the node body keeps a stable size
        // regardless of how much is scrolled into view (allocating only the
        // visible sub-rect made the whole module appear to resize as you
        // scrolled, and lag when restoring). Only the paint callback is confined
        // to the visible sub-rect.
        let full_rect = self.state.full_rect;
        let vis_rect = self.vis_rect;
        let paint_callback = Callback::new_paint_callback(vis_rect, self.state);
        let response = ui.allocate_rect(full_rect, egui::Sense::hover());
        ui.painter_at(vis_rect)
            .add(egui::epaint::Shape::Callback(paint_callback));
        response
    }
}

/// Build the controller-viewer widget for `model`, oriented by `orientation`.
/// `vis_rect` is the visible sub-rect painted into; `full_rect` is the node's
/// full intended rect (drives camera framing so the model keeps a stable size
/// and simply crops when partly scrolled off). Pass `vis_rect == full_rect` when
/// fully on-screen. Model data is shared (`Arc`), so per-frame construction is
/// cheap; GPU buffers are cached across frames in the callback resources.
pub fn build_controller_widget(
    vis_rect: egui::Rect,
    full_rect: egui::Rect,
    model: Arc<LoadedModel>,
    orientation: Quat,
) -> impl egui::Widget {
    Controller3DWidget {
        vis_rect,
        state: MeshRenderState {
            model,
            orientation,
            full_rect,
            vis_rect,
            pipeline: None,
        },
    }
}
