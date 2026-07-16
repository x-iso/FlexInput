// controller.wgsl — Shader for FlexInput 3D Controller Model Viewer
// Flat-shaded meshes with Lambertian diffuse lighting, a shading-preserving
// highlight glow with rim/contour emphasis, and depth-aware transparency.

struct Uniforms {
    mvp: mat4x4<f32>,
    model: mat4x4<f32>,
    base_color: vec4<f32>,
    glow_color: vec4<f32>,
    glow: f32,
    global_alpha: f32, // whole-model opacity for the 2D composite
    _pad0: vec2<f32>,  // padding to 16-byte alignment (uniforms must be 16-aligned)
    cam_pos: vec4<f32>,       // xyz = camera position in model space
    center_radius: vec4<f32>, // xyz = model centre, w = bounding radius
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
    @location(2) world_pos: vec3<f32>,
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
    out.world_pos = world_pos.xyz;
    return out;
}

@fragment
fn fs_main(in: FragmentInput) -> @location(0) vec4<f32> {
    // Fixed light direction in world space (normalized)
    let light_dir = normalize(vec3<f32>(0.4, 0.8, 0.45));
    let normal = normalize(in.world_normal);

    // Lambertian diffuse. The glow blends the ALBEDO toward the highlight
    // colour before lighting (so highlighted parts keep their 3D shading),
    // with a small emissive lift on top so the glow reads as light.
    let ndotl = max(dot(normal, light_dir), 0.0);
    let ambient = vec3<f32>(0.35);
    let albedo = mix(in.base_color.rgb, uniforms.glow_color.rgb, uniforms.glow);
    var lit_color = (ambient + ndotl) * albedo
        + uniforms.glow_color.rgb * uniforms.glow * 0.22;

    // Rim / contour glow: fragments whose normal is near-perpendicular to the
    // view direction (the part's silhouette) get an extra emissive ring, so a
    // highlighted input has a glowing outline rather than a flat tint.
    let view_dir = normalize(uniforms.cam_pos.xyz - in.world_pos);
    let rim = pow(1.0 - clamp(abs(dot(normal, view_dir)), 0.0, 1.0), 2.0);
    lit_color = lit_color + uniforms.glow_color.rgb * rim * uniforms.glow * 0.9;

    // Model opacity: a clean uniform fade of every drawn surface (depth
    // testing still hides interior parts; the x-ray ghosts carry the
    // "active input behind the shell" job). The widget composite/matte fade
    // happens in a separate blit pass over the finished 2D render.
    let a = clamp(uniforms.global_alpha, 0.0, 1.0) * in.base_color.a;

    // Premultiplied output: the pipeline blends src One / dst 1-srcA, so colour
    // must be pre-scaled by the (global) alpha for correct overlay transparency.
    return vec4<f32>(lit_color * a, a);
}
