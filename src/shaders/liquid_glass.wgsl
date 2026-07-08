// =============================================================================
// Liquid Glass — Apple-style background refraction shader
// Ported from liquid-glass-react (https://github.com/nicepkg/liquid-glass-react)
//
// Pure displacement-based refraction with chromatic aberration and Fresnel.
// No background blur — the glass is fully transparent with edge-only effects.
// =============================================================================

struct Params {
    max_displacement: f32,           // offset 0
    blur_sigma: f32,                 // offset 4 (unused, kept for compat)
    blur_center_falloff: f32,        // offset 8 (unused, kept for compat)
    fresnel_power: f32,              // offset 12
    fresnel_intensity: f32,          // offset 16
    glass_opacity: f32,              // offset 20 (unused, kept for compat)
    _pad0: vec2<f32>,                // offset 24
    tint: vec4<f32>,                 // offset 32
    curvature_strength: f32,         // offset 48
    margin_x: f32,                   // offset 52
    margin_y: f32,                   // offset 56
    _pad1: f32,                      // offset 60
}

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var displacement_tex: texture_2d<f32>;

fn sample_bilinear(tex: texture_2d<f32>, uv: vec2<f32>, dims: vec2<u32>) -> vec4<f32> {
    let dims_f = vec2<f32>(dims);
    let clamped_uv = clamp(uv, vec2(0.0), vec2(1.0));
    let coord = clamped_uv * dims_f - 0.5;
    let base = floor(coord);
    let frac = coord - base;
    let c00 = clamp(vec2<i32>(base) + vec2(0, 0), vec2(0), vec2<i32>(dims) - 1);
    let c10 = clamp(vec2<i32>(base) + vec2(1, 0), vec2(0), vec2<i32>(dims) - 1);
    let c01 = clamp(vec2<i32>(base) + vec2(0, 1), vec2(0), vec2<i32>(dims) - 1);
    let c11 = clamp(vec2<i32>(base) + vec2(1, 1), vec2(0), vec2<i32>(dims) - 1);
    return mix(
        mix(textureLoad(tex, c00, 0), textureLoad(tex, c10, 0), frac.x),
        mix(textureLoad(tex, c01, 0), textureLoad(tex, c11, 0), frac.x),
        frac.y,
    );
}

fn decode_displacement(uv: vec2<f32>, dims: vec2<u32>) -> vec3<f32> {
    let dm = sample_bilinear(displacement_tex, uv, dims);
    let dx = (dm.r - 0.5) * 2.0 * params.max_displacement;
    let dy = (dm.g - 0.5) * 2.0 * params.max_displacement;
    return vec3(dx, dy, dm.b);
}

fn apply_curvature(uv: vec2<f32>, edge_factor: f32) -> vec2<f32> {
    let center = vec2(0.5);
    let to_center = center - uv;
    let dist = length(to_center);
    if dist < 0.001 { return uv; }
    let curvature = params.curvature_strength * edge_factor * dist * dist;
    return uv + normalize(to_center) * curvature * 0.15;
}

fn schlick_fresnel(edge_factor: f32) -> f32 {
    let F0 = 0.04;
    let cos_theta = 1.0 - edge_factor;
    let primary = F0 + (1.0 - F0) * pow(1.0 - cos_theta, params.fresnel_power);
    let rim = pow(edge_factor, 1.5) * 0.3;
    return (primary + rim) * params.fresnel_intensity;
}

fn ambient_diffuse(uv: vec2<f32>, dims: vec2<u32>, edge_factor: f32) -> f32 {
    let dims_f = vec2<f32>(dims);
    let eps = 1.0 / dims_f;
    let e_x = sample_bilinear(displacement_tex, uv + vec2(eps.x, 0.0), dims).b
            - sample_bilinear(displacement_tex, uv - vec2(eps.x, 0.0), dims).b;
    let e_y = sample_bilinear(displacement_tex, uv + vec2(0.0, eps.y), dims).b
            - sample_bilinear(displacement_tex, uv - vec2(0.0, eps.y), dims).b;
    let nx = -e_x * 4.0;
    let ny = -e_y * 4.0;
    let nz = 1.0;
    let n_len = sqrt(nx * nx + ny * ny + nz * nz);
    let light = vec3(-0.4, -0.5, 0.76);
    let ndotl = max((nx * light.x + ny * light.y + nz * light.z) / n_len, 0.0);
    return ndotl * edge_factor * 0.15;
}

@compute @workgroup_size(8, 8)
fn liquid_glass_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let disp_dims = textureDimensions(displacement_tex);
    let input_dims = textureDimensions(input_tex);
    if gid.x >= disp_dims.x || gid.y >= disp_dims.y { return; }
    let disp_dims_f = vec2<f32>(disp_dims);
    let input_dims_f = vec2<f32>(input_dims);
    let uv = vec2<f32>(gid.xy) / disp_dims_f;
    let input_px = vec2<f32>(gid.xy) + vec2(params.margin_x, params.margin_y);

    let disp = decode_displacement(uv, disp_dims);
    let dx = disp.x;
    let dy = disp.y;
    let edge_factor = disp.z;

    let curved_uv = apply_curvature(uv, edge_factor);
    let curvature_px = (curved_uv - uv) * disp_dims_f;

    // --- Per-channel chromatic aberration (from React impl) ---
    // Each channel gets slightly different displacement scale
    let aberration = params.max_displacement * 0.10 * edge_factor;
    let r_uv = (input_px + curvature_px + vec2(dx, dy) * 1.0 + vec2(aberration, 0.0)) / input_dims_f;
    let g_uv = (input_px + curvature_px + vec2(dx, dy) * (1.0 - edge_factor * 0.10)) / input_dims_f;
    let b_uv = (input_px + curvature_px + vec2(dx, dy) * (1.0 - edge_factor * 0.20) - vec2(aberration, 0.0)) / input_dims_f;

    let r_val = sample_bilinear(input_tex, r_uv, input_dims).r;
    let g_val = sample_bilinear(input_tex, g_uv, input_dims).g;
    let b_val = sample_bilinear(input_tex, b_uv, input_dims).b;

    var result = vec4(r_val, g_val, b_val, 1.0);

    // --- Fresnel edge highlight ---
    let fresnel = schlick_fresnel(edge_factor);
    result = result + vec4(vec3(fresnel), 0.0);

    // --- Ambient diffuse ---
    let diffuse = ambient_diffuse(uv, disp_dims, edge_factor);
    result = result + vec4(vec3(diffuse * 0.8, diffuse * 0.85, diffuse), 0.0);

    // --- Inner contour line ---
    let inner_line = smoothstep(0.7, 0.85, edge_factor) * (1.0 - smoothstep(0.85, 1.0, edge_factor));
    result = result - vec4(vec3(inner_line * 0.03), 0.0);

    // --- Curvature center highlight ---
    let center_dist = length(uv - vec2(0.5));
    let curvature_highlight = params.curvature_strength * exp(-center_dist * center_dist * 8.0) * 0.02;
    result = result + vec4(vec3(curvature_highlight), 0.0);

    result = clamp(result, vec4(0.0), vec4(1.0));
    result.a = 1.0;

    textureStore(output_tex, vec2<i32>(gid.xy), result);
}
