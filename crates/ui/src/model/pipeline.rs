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
/// CPU-side uniform buffer. Must match WGSL struct exactly (160 bytes, 16-aligned).
#[repr(C)]
#[derive(bytemuck::Pod, bytemuck::Zeroable, Clone, Copy, Default)]
pub struct Uniforms {
    pub mvp: [[f32; 4]; 4],
    pub model: [[f32; 4]; 4],
    pub base_color: [f32; 4],
    pub glow_color: [f32; 4],
    pub glow: f32,
    // WGSL uniform structs have 16-byte alignment, so the struct size rounds
    // up to a multiple of 16. `glow` sits at offset 160; the tail must pad to
    // 176 (not 168) or wgpu rejects the draw ("bound with size 168 where the
    // shader expects 176"). Three f32s of padding fill the final 16-byte slot.
    pub _pad0: [f32; 3],
}

impl Uniforms {
    /// Total size in bytes (176 = 16 * 11), matching the WGSL struct's
    /// 16-byte-aligned layout.
    pub const SIZE: usize = 176;

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
            _pad0: [0.0; 3],
        }
    }

    /// Serialize to raw bytes for wgpu buffer write.
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

/// The WGPU render pipeline for controller meshes. Created once per device, then reused.
pub struct ControllerPipeline {
    pub(crate) pipeline: RenderPipeline,
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

        // Build the render pipeline descriptor.
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("controller_pipeline"),
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
                // No backface culling: the OBJ winding order isn't guaranteed to
                // match wgpu's default front-face. With the depth buffer below,
                // occlusion is resolved per-fragment regardless of winding, so
                // culling isn't needed for correctness.
                cull_mode: None,
                ..Default::default()
            },
            // Depth testing against egui's shared depth attachment (enabled via
            // eframe `depth_buffer: 32` → Depth32Float, cleared to 1.0 each
            // frame). `perspective_infinite_rh` yields [0,1] depth with near→0,
            // so nearer fragments (smaller depth) win under `Less`. We write
            // depth so our own parts occlude each other correctly; egui's UI
            // draws with compare-Always so it's unaffected.
            depth_stencil: Some(DepthStencilState {
                format: CONTROLLER_DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: CompareFunction::Less,
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

        Self {
            pipeline,
            bind_group_layout,
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

/// GPU-side buffers for a single controller part (one mesh).
#[derive(Clone)]
pub struct PartBuffers {
    pub(crate) vertex_buf: Buffer,
    pub(crate) vert_count: u32,
    uniform_buf: Arc<Buffer>,
    bind_group: BindGroup,
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

        // Uniform buffer (160 bytes).
        let uniform_buf = Arc::new(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("controller_uniform_buffer"),
            contents: uniforms.as_bytes(),
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        }));

        // Bind group tying the uniform buffer to the pipeline's layout.
        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("controller_part_bind_group"),
            layout: &pipeline.bind_group_layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });

        Self {
            vertex_buf,
            vert_count,
            uniform_buf,
            bind_group,
        }
    }

    /// Update the uniform buffer with new transform/color data.
    pub fn update_uniforms(&self, queue: &Queue, uniforms: &Uniforms) {
        queue.write_buffer(&self.uniform_buf, 0, uniforms.as_bytes());
    }

    /// Draw this part into the render pass.
    pub fn draw(&self, pass: &mut RenderPass<'_>) {
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
        pass.draw(0..self.vert_count, 0..1);
    }
}
