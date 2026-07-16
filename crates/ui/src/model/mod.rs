/// 3D controller model loader + WGPU renderer for FlexInput.
///
/// This module provides:
/// - OBJ file parsing (`obj.rs`) — loads .obj mesh data and info.txt transforms
/// - WGPU render pipeline (`pipeline.rs`) — creates GPU buffers and render pipelines
/// - egui PaintCallback integration (`callback.rs`) — renders 3D models inside egui nodes

mod obj;
pub mod controller_wgsl;
pub mod material;
pub mod pipeline;
pub mod callback;

// Re-export OBJ loader types/functions for use by other crates.
pub use obj::{parse_obj, parse_info_txt, load_controller_model, load_controller_model_with, part_transform};
pub use obj::{Mesh, Part, ControllerModel, PartTransform, ObjError};

// Runtime model cache + render-widget builder + device→model mapping.
pub use callback::{
    available_models, load_model_cached, model_for_device, models_base_dir,
    paint_controller_model, set_target_format, set_user_models_dir,
    ControllerLive, LoadedModel,
};
pub use material::{default_scheme, MaterialGroup, Scheme, MIC_COLOR, N_GROUPS};
