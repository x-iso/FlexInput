/// WGPU render pipeline for FlexInput 3D controller models.
/// Creates the render pipeline, bind group layouts, and GPU buffers (vertex + uniforms).

use std::sync::Arc;
use wgpu::{util::DeviceExt as _, *};

use crate::model::controller_wgsl;

/// Depth attachment format for the 3D controller pass. MUST match eframe's
/// `depth_buffer` setting in `app/src/main.rs` (32 bits → Depth32Float); egui's
/// shared render pass owns the actual depth texture at this format.
pub const CONTROLLER_DEPTH_FORMAT: TextureFormat = TextureFormat::Depth32Float;

// ── Uniforms layout ───────────────────────────────────────────────────────────
/// CPU-side uniform buffer. Must match WGSL struct exactly (208 bytes, 16-aligned).
#[repr(C)]
#[derive(bytemuck::Pod, bytemuck::Zeroable, Clone, Copy, Default)]
pub struct Uniforms {
    pub mvp: [[f32; 4]; 4],
    pub model: [[f32; 4]; 4],
    pub base_color: [f32; 4],
    pub glow_color: [f32; 4],
    pub glow: f32,
    /// Whole-model opacity (0..1) for the 2D composite (overlay transparency).
    pub global_alpha: f32,
    // WGSL uniform structs have 16-byte alignment; `glow`+`global_alpha` occupy
    // offsets 160..168, padded to 176 before the trailing vec4s. Keep the Rust
    // and WGSL layouts byte-identical or wgpu rejects the draw.
    pub _pad0: [f32; 2],
    /// Camera position in model space (`xyz`; `w` unused) — rim/contour glow +
    /// depth-aware transparency.
    pub cam_pos: [f32; 4],
    /// Model bounding sphere: `xyz` = centre, `w` = radius (depth fade range).
    pub center_radius: [f32; 4],
}

impl Uniforms {
    /// Total size in bytes (208 = 16 * 13), matching the WGSL struct's
    /// 16-byte-aligned layout.
    pub const SIZE: usize = 208;

    /// Create default uniforms (identity matrices, white base color, no glow).
    pub fn default_uniform() -> Self {
        let identity = [[1.0, 0.0, 0.0, 0.0],
                        [0.0, 1.0, 0.0, 0.0],
                        [0.0, 0.0, 1.0, 0.0],
                        [0.0, 0.0, 0.0, 1.0]];
        Uniforms {
            mvp: identity,
            model: identity,
            base_color: [1.0, 1.0, 1.0, 1.0],
            glow_color: [1.0, 0.2, 0.2, 1.0],
            glow: 0.0,
            global_alpha: 1.0,
            _pad0: [0.0; 2],
            cam_pos: [0.0, 0.0, 1.0, 1.0], // w = widget composite alpha
            center_radius: [0.0, 0.0, 0.0, 1.0],
        }
    }

    /// Serialize to raw bytes for wgpu buffer write.
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

/// The WGPU render pipelines for controller meshes (two depth-state variants
/// sharing one shader + layout). Created once per device, then reused.
pub struct ControllerPipeline {
    /// Normal opaque pass: depth test Less + depth writes on.
    pub(crate) pipeline: RenderPipeline,
    /// X-ray pass for active-but-occluded inputs: depth test **Greater** (draw
    /// only where something already covers the part) + no depth writes — the
    /// classic "show through walls" trick.
    pub(crate) xray: RenderPipeline,
    /// Post-ghost re-draw of highlighted VISIBLE parts (LessEqual + bias, no
    /// depth write): reclaims their pixels from ghosts so x-ray never covers a
    /// nearer highlighted input.
    pub(crate) restore: RenderPipeline,
    /// Depth-only pipelines for the per-part VISIBILITY measurement (occlusion
    /// queries): a prepass laying down the whole model's depth, then per part
    /// a "visible samples" draw (LessEqual vs that depth) and a "total
    /// footprint" draw (Always). No fragment stage / colour output.
    pub(crate) vis_prepass: RenderPipeline,
    pub(crate) vis_measure_visible: RenderPipeline,
    pub(crate) vis_measure_total: RenderPipeline,
    pub(crate) bind_group_layout: BindGroupLayout,
}

impl ControllerPipeline {
    /// Create a new pipeline from the WGPU device and surface texture format.
    pub fn new(
        device: &Device,
        target_format: TextureFormat,
        sample_count: u32,
    ) -> Self {
        // Load shader module from embedded WGSL source.
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("controller"),
            source: wgpu::ShaderSource::Wgsl(controller_wgsl::SHADER.into()),
        });

        // Single uniform buffer binding for MVP + model + colors.
        let bind_group_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("controller_uniforms"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::VERTEX | ShaderStages::FRAGMENT,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        // Pipeline layout references the bind group layout.
        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("controller_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // Shared builder: the colour variants differ only in depth state.
        let build = |label: &str,
                     depth_write: bool,
                     depth_compare: CompareFunction,
                     bias_constant: i32| {
            device.create_render_pipeline(&RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: VertexState {
                    module: &shader_module,
                    entry_point: Some("vs_main"),
                    buffers: &[VertexBufferLayout {
                        array_stride: 24, // vec3<f32> pos + vec3<f32> normal = 6 * 4 bytes
                        step_mode: VertexStepMode::Vertex,
                        attributes: &vertex_buffer_layout(),
                    }],
                    compilation_options: Default::default(),
                },
                fragment: Some(FragmentState {
                    module: &shader_module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(ColorTargetState {
                        format: target_format,
                        blend: Some(BlendState {
                            color: BlendComponent {
                                src_factor: BlendFactor::One,
                                dst_factor: BlendFactor::OneMinusSrcAlpha,
                                operation: BlendOperation::Add,
                            },
                            alpha: BlendComponent {
                                src_factor: BlendFactor::One,
                                dst_factor: BlendFactor::OneMinusSrcAlpha,
                                operation: BlendOperation::Add,
                            },
                        }),
                        write_mask: ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: PrimitiveState {
                    topology: PrimitiveTopology::TriangleList,
                    // No backface culling: the OBJ winding order isn't guaranteed
                    // to match wgpu's default front-face. With the depth buffer
                    // below, occlusion is resolved per-fragment regardless of
                    // winding, so culling isn't needed for correctness.
                    cull_mode: None,
                    ..Default::default()
                },
                // Depth testing against egui's shared depth attachment (enabled
                // via eframe `depth_buffer: 32` → Depth32Float, cleared to 1.0
                // each frame). `perspective_infinite_rh` yields [0,1] depth with
                // near→0, so nearer fragments (smaller depth) win under `Less`.
                // egui's own UI draws with compare-Always so it's unaffected.
                depth_stencil: Some(DepthStencilState {
                    format: CONTROLLER_DEPTH_FORMAT,
                    depth_write_enabled: depth_write,
                    depth_compare,
                    stencil: StencilState::default(),
                    bias: DepthBiasState {
                        constant: bias_constant,
                        ..DepthBiasState::default()
                    },
                }),
                multisample: MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview: None,
                cache: None,
            })
        };

        let pipeline = build("controller_pipeline", true, CompareFunction::Less, 0);
        let xray = build("controller_pipeline_xray", false, CompareFunction::Greater, 0);
        // Re-draws a highlighted part's VISIBLE surface after the ghost pass,
        // so a ghost never overpaints a nearer highlighted input (z-order
        // among highlighted objects). LessEqual + a small negative bias makes
        // the part's own fragments reliably reclaim their pixels.
        let restore = build(
            "controller_pipeline_restore",
            false,
            CompareFunction::LessEqual,
            -2,
        );

        // Depth-only builder (no fragment stage) for the visibility queries.
        let build_depth = |label: &str, depth_write: bool, depth_compare: CompareFunction| {
            device.create_render_pipeline(&RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: VertexState {
                    module: &shader_module,
                    entry_point: Some("vs_main"),
                    buffers: &[VertexBufferLayout {
                        array_stride: 24,
                        step_mode: VertexStepMode::Vertex,
                        attributes: &vertex_buffer_layout(),
                    }],
                    compilation_options: Default::default(),
                },
                fragment: None,
                primitive: PrimitiveState {
                    topology: PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(DepthStencilState {
                    format: CONTROLLER_DEPTH_FORMAT,
                    depth_write_enabled: depth_write,
                    depth_compare,
                    stencil: StencilState::default(),
                    bias: DepthBiasState::default(),
                }),
                multisample: MultisampleState {
                    count: 1,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview: None,
                cache: None,
            })
        };
        // LessEqual (not Less): the prepass wrote this part's own depth, so its
        // visible front surface must still pass its own values.
        let vis_prepass = build_depth("c3d_vis_prepass", true, CompareFunction::Less);
        let vis_measure_visible =
            build_depth("c3d_vis_measure_visible", false, CompareFunction::LessEqual);
        let vis_measure_total = build_depth("c3d_vis_measure_total", false, CompareFunction::Always);

        Self {
            pipeline,
            xray,
            restore,
            vis_prepass,
            vis_measure_visible,
            vis_measure_total,
            bind_group_layout,
        }
    }
}

// ── Matte blit pipeline ───────────────────────────────────────────────────────

/// Fullscreen-triangle shader for the widget matte: samples the offscreen
/// controller render (premultiplied) and scales colour+alpha by the matte
/// factor — a true 2D fade of the finished image.
const BLIT_WGSL: &str = r#"
struct BlitUniforms { alpha: vec4<f32> }; // x = matte alpha, rest padding
@group(0) @binding(0) var t_src: texture_2d<f32>;
@group(0) @binding(1) var s_src: sampler;
@group(0) @binding(2) var<uniform> u: BlitUniforms;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
    // Fullscreen triangle: (0,0) (2,0) (0,2) in uv space.
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VOut;
    out.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    return textureSample(t_src, s_src, in.uv) * clamp(u.alpha.x, 0.0, 1.0);
}
"#;

/// Pipeline that composites the offscreen controller render into the egui pass
/// at a matte alpha (the pinned widget's 2D fade). Depth is declared to match
/// the egui pass but never tested/written.
pub struct BlitPipeline {
    pub(crate) pipeline: RenderPipeline,
    pub(crate) bind_group_layout: BindGroupLayout,
    pub(crate) sampler: Sampler,
}

impl BlitPipeline {
    pub fn new(device: &Device, target_format: TextureFormat, sample_count: u32) -> Self {
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("c3d_blit"),
            source: ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("c3d_blit_bgl"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: true },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 2,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Buffer {
                        ty: BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("c3d_blit_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("c3d_blit_pipeline"),
            layout: Some(&layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(ColorTargetState {
                    format: target_format,
                    blend: Some(BlendState {
                        color: BlendComponent {
                            src_factor: BlendFactor::One,
                            dst_factor: BlendFactor::OneMinusSrcAlpha,
                            operation: BlendOperation::Add,
                        },
                        alpha: BlendComponent {
                            src_factor: BlendFactor::One,
                            dst_factor: BlendFactor::OneMinusSrcAlpha,
                            operation: BlendOperation::Add,
                        },
                    }),
                    write_mask: ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: Some(DepthStencilState {
                format: CONTROLLER_DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: CompareFunction::Always,
                stencil: StencilState::default(),
                bias: DepthBiasState::default(),
            }),
            multisample: MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("c3d_blit_sampler"),
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            bind_group_layout,
            sampler,
        }
    }
}

/// Vertex buffer attribute layout matching the WGSL vertex shader.
fn vertex_buffer_layout() -> [VertexAttribute; 2] {
    [
        VertexAttribute {
            format: VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0, // position
        },
        VertexAttribute {
            format: VertexFormat::Float32x3,
            offset: 12,
            shader_location: 1, // normal
        },
    ]
}

// ── Per-part GPU buffers ──────────────────────────────────────────────────────

/// GPU-side buffers for a single controller part (one mesh). Each part carries
/// TWO uniform sets: the main draw and the x-ray ghost draw (active-but-occluded
/// inputs re-drawn through the shell with their own colour/alpha).
#[derive(Clone)]
pub struct PartBuffers {
    pub(crate) vertex_buf: Buffer,
    pub(crate) vert_count: u32,
    uniform_buf: Arc<Buffer>,
    bind_group: BindGroup,
    xray_uniform_buf: Arc<Buffer>,
    xray_bind_group: BindGroup,
}

impl PartBuffers {
    /// Create GPU buffers from interleaved vertex data.
    /// `vertices` must have length divisible by 6 (vec3 pos + vec3 normal per vertex).
    pub fn new(
        device: &Device,
        pipeline: &ControllerPipeline,
        vertices: &[f32],
        uniforms: &Uniforms,
    ) -> Self {
        assert_eq!(vertices.len() % 6, 0);
        let vert_count = (vertices.len() / 6) as u32;

        // Upload vertex data to GPU.
        let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("controller_vertex_buffer"),
            contents: bytemuck::cast_slice(vertices),
            usage: BufferUsages::VERTEX,
        });

        // Main + x-ray uniform buffers with matching bind groups.
        let mk = |label: &str| -> (Arc<Buffer>, BindGroup) {
            let buf = Arc::new(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: uniforms.as_bytes(),
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            }));
            let bg = device.create_bind_group(&BindGroupDescriptor {
                label: Some(label),
                layout: &pipeline.bind_group_layout,
                entries: &[BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                }],
            });
            (buf, bg)
        };
        let (uniform_buf, bind_group) = mk("controller_part_uniforms");
        let (xray_uniform_buf, xray_bind_group) = mk("controller_part_xray_uniforms");

        Self {
            vertex_buf,
            vert_count,
            uniform_buf,
            bind_group,
            xray_uniform_buf,
            xray_bind_group,
        }
    }

    /// Update the uniform buffer with new transform/color data.
    pub fn update_uniforms(&self, queue: &Queue, uniforms: &Uniforms) {
        queue.write_buffer(&self.uniform_buf, 0, uniforms.as_bytes());
    }

    /// Update the x-ray-pass uniform buffer (ghost colour/alpha for this part).
    pub fn update_xray_uniforms(&self, queue: &Queue, uniforms: &Uniforms) {
        queue.write_buffer(&self.xray_uniform_buf, 0, uniforms.as_bytes());
    }

    /// Draw this part into the render pass.
    pub fn draw(&self, pass: &mut RenderPass<'_>) {
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
        pass.draw(0..self.vert_count, 0..1);
    }

    /// Draw this part with its x-ray uniforms (caller sets the xray pipeline).
    pub fn draw_xray(&self, pass: &mut RenderPass<'_>) {
        pass.set_bind_group(0, &self.xray_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
        pass.draw(0..self.vert_count, 0..1);
    }
}
