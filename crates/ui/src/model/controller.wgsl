// controller.wgsl — Shader for FlexInput 3D Controller Model Viewer
// Renders flat-shaded meshes with Lambertian diffuse lighting + glow effect.

struct Uniforms {
    mvp: mat4x4<f32>,
    model: mat4x4<f32>,
    base_color: vec4<f32>,
    glow_color: vec4<f32>,
    glow: f32,
    _pad0: f32, // padding to 16-byte alignment (uniforms must be 16-aligned)
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct FragmentInput {
    @builtin(position) frag_pos: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) base_color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> FragmentInput {
    let world_pos = uniforms.model * vec4<f32>(in.position, 1.0);
    let clip_pos = uniforms.mvp * world_pos;

    var out: FragmentInput;
    out.frag_pos = clip_pos;
    // Rotate normal by model (assumes no non-uniform scaling)
    out.world_normal = (uniforms.model * vec4<f32>(in.normal, 0.0)).xyz;
    out.base_color = uniforms.base_color;
    return out;
}

@fragment
fn fs_main(in: FragmentInput) -> @location(0) vec4<f32> {
    // Fixed light direction in world space (normalized)
    let light_dir = normalize(vec3<f32>(0.4, 0.8, 0.45));
    let normal = normalize(in.world_normal);

    // Lambertian diffuse
    let ndotl = max(dot(normal, light_dir), 0.0);
    let ambient = vec3<f32>(0.35);
    let diffuse = (ambient + ndotl) * in.base_color.rgb;

    // Lerp toward glow color based on glow factor
    let lit_color = mix(diffuse, uniforms.glow_color.rgb, uniforms.glow);

    return vec4<f32>(lit_color, in.base_color.a);
}
