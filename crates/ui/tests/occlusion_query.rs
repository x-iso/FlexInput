//! Does this machine's GPU stack actually return occlusion-query counts?
//!
//! The 3D viewer's x-ray effect decides whether an input is in line of sight by
//! rasterizing each part twice into a depth-only offscreen pass — once tested
//! against the model's depth prepass ("visible samples"), once with depth
//! compare Always ("total samples") — and ghosting the part when the ratio
//! falls near zero. If the queries return no counts at all, every part measures
//! as 0/0 and the whole model ghosts permanently, which is indistinguishable
//! from "everything is genuinely occluded" at the call site.
//!
//! That failure was observed on DX12 with renderer code that had previously
//! worked, and nothing in the renderer, its dependencies, or the device setup
//! had changed — so the question "are occlusion queries alive here?" needs an
//! answer independent of the app. These tests draw a full-screen triangle that
//! cannot fail to rasterize and assert the query counts it.
//!
//! The `no_color_attachment` variant mirrors the viewer's real pass exactly
//! (depth-only, `fragment: None`, `color_attachments: &[]`); the `with_color`
//! variant adds a colour target and a fragment stage. If only the latter
//! reports samples, the driver is declining to count in depth-only passes and
//! the viewer's measurement pass needs a dummy colour target.
//!
//! Ignored by default: these need a real adapter, so they are not part of the
//! normal test gate. Run them explicitly:
//!
//! ```text
//! cargo test -p flexinput-ui --test occlusion_query -- --ignored --nocapture
//! ```

/// Full-screen triangle. Covers the whole target from three vertices, so the
/// sample count can never legitimately be zero.
const SHADER: &str = r#"
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 3.0,  1.0),
    );
    return vec4<f32>(p[i], 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
"#;

const RES: u32 = 256;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Renders one full-screen triangle under an occlusion query and returns the
/// sample count the driver reports. `None` = no adapter for this backend (the
/// machine simply doesn't support it — not a failure).
fn occlusion_samples(backends: wgpu::Backends, with_color: bool) -> Option<u64> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .ok()?;
    let info = adapter.get_info();
    println!("  adapter: {} ({:?}, {:?})", info.name, info.backend, info.device_type);

    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("oq_shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("oq_layout"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });

    let color_targets = [Some(wgpu::ColorTargetState {
        format: COLOR_FORMAT,
        blend: None,
        write_mask: wgpu::ColorWrites::ALL,
    })];
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("oq_pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        // The viewer's measurement pipelines have no fragment stage at all;
        // keep that shape unless we're deliberately testing the colour variant.
        fragment: with_color.then(|| wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &color_targets,
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            // Always: every sample passes, so the count equals the covered area.
            depth_compare: wgpu::CompareFunction::Always,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview: None,
        cache: None,
    });

    let tex = |label, format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: RES,
                height: RES,
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
    let depth = tex("oq_depth", DEPTH_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let depth_view = depth.create_view(&Default::default());
    let color = tex("oq_color", COLOR_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let color_view = color.create_view(&Default::default());

    let qs = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("oq_queries"),
        ty: wgpu::QueryType::Occlusion,
        count: 1,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("oq_resolve"),
        size: 8,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("oq_staging"),
        size: 8,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut enc = device.create_command_encoder(&Default::default());
    {
        let attachments = [Some(wgpu::RenderPassColorAttachment {
            view: &color_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })];
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("oq_pass"),
            color_attachments: if with_color { &attachments } else { &[] },
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: Some(&qs),
        });
        pass.set_pipeline(&pipeline);
        pass.begin_occlusion_query(0);
        pass.draw(0..3, 0..1);
        pass.end_occlusion_query();
    }
    enc.resolve_query_set(&qs, 0..1, &resolve, 0);
    enc.copy_buffer_to_buffer(&resolve, 0, &staging, 0, 8);
    queue.submit(Some(enc.finish()));

    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).expect("device poll");
    rx.recv().expect("map callback").expect("buffer map");

    let count = {
        let data = staging.slice(..).get_mapped_range();
        u64::from_le_bytes(data[..8].try_into().unwrap())
    };
    staging.unmap();
    Some(count)
}

fn report(backend: wgpu::Backends, name: &str, with_color: bool) {
    println!("{name} (color attachment: {with_color}):");
    match occlusion_samples(backend, with_color) {
        None => println!("  no adapter — skipped"),
        Some(0) => panic!(
            "{name}: occlusion query returned 0 samples for a full-screen triangle \
             (color attachment: {with_color}). The driver is not counting samples; \
             the viewer's x-ray measurement cannot work in this configuration."
        ),
        Some(n) => {
            println!("  samples = {n} (expected ~{})", RES * RES);
            assert!(n > 0);
        }
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn dx12_depth_only_no_color_attachment() {
    report(wgpu::Backends::DX12, "DX12", false);
}

#[test]
#[ignore = "requires a GPU adapter"]
fn dx12_with_color_attachment() {
    report(wgpu::Backends::DX12, "DX12", true);
}

#[test]
#[ignore = "requires a GPU adapter"]
fn vulkan_depth_only_no_color_attachment() {
    report(wgpu::Backends::VULKAN, "Vulkan", false);
}

#[test]
#[ignore = "requires a GPU adapter"]
fn vulkan_with_color_attachment() {
    report(wgpu::Backends::VULKAN, "Vulkan", true);
}
