// =============================================================================
// Liquid Glass Overlay — edge-only effects, zero capture
//
// Layer 1: The window itself is fully transparent (desktop shows through).
// Layer 2: This shader renders edge glow / Fresnel / curvature / contour
//          on top, with alpha blending. Center pixels are fully transparent.
//
// No background texture needed — displacement map drives everything.
// =============================================================================

struct Params {
    fresnel_power: f32,
    fresnel_intensity: f32,
    curvature_strength: f32,
    tint: vec4<f32>,
    time: f32,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var output_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var displacement_tex: texture_2d<f32>;

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
    return vec3(0.0, 0.0, dm.b); // only edge_factor (b channel) is used
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

fn smoothstep_custom(a: f32, b: f32, t: f32) -> f32 {
    let t_clamped = clamp((t - a) / (b - a), 0.0, 1.0);
    return t_clamped * t_clamped * (3.0 - 2.0 * t_clamped);
}

@compute @workgroup_size(8, 8)
fn liquid_glass_overlay_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(displacement_tex);
    if gid.x >= dims.x || gid.y >= dims.y { return; }
    let dims_f = vec2<f32>(dims);
    let uv = vec2<f32>(gid.xy) / dims_f;

    let disp = decode_displacement(uv, dims);
    let edge_factor = disp.z;

    let glow_color = vec3(1.0, 0.98, 0.94);
    let fresnel = schlick_fresnel(edge_factor);
    let diffuse = ambient_diffuse(uv, dims, edge_factor);
    let inner_line = smoothstep_custom(0.68, 0.85, edge_factor)
                   * (1.0 - smoothstep_custom(0.85, 1.0, edge_factor));
    let center_dist = length(uv - vec2(0.5));
    let curvature_highlight = params.curvature_strength
                            * exp(-center_dist * center_dist * 8.0) * 0.03;

    var result = vec4(0.0, 0.0, 0.0, 0.0);

    let g0 = glow_color * fresnel * 0.7;
    result.r = result.r + g0.x;
    result.g = result.g + g0.y;
    result.b = result.b + g0.z;

    let d0 = vec3(diffuse * 0.7, diffuse * 0.78, diffuse * 1.0) * 0.45;
    result.r = result.r + d0.x;
    result.g = result.g + d0.y;
    result.b = result.b + d0.z;

    let il = vec3(inner_line * 0.08);
    result.r = result.r - il.x;
    result.g = result.g - il.y;
    result.b = result.b - il.z;

    let ch = vec3(curvature_highlight * 2.5);
    result.r = result.r + ch.x;
    result.g = result.g + ch.y;
    result.b = result.b + ch.z;

    result.r = result.r + params.tint.r * edge_factor * 0.15;
    result.g = result.g + params.tint.g * edge_factor * 0.15;
    result.b = result.b + params.tint.b * edge_factor * 0.15;

    let falloff = pow(1.0 - edge_factor, 3.5);
    let alpha = clamp(1.0 - falloff, 0.0, 1.0);
    let alpha_boost = smoothstep_custom(0.85, 1.0, edge_factor) * 0.2;
    result.a = clamp(alpha + alpha_boost + fresnel * 0.4, 0.0, 1.0);

    result = clamp(result, vec4(0.0), vec4(1.0));

    textureStore(output_tex, vec2<i32>(gid.xy), result);
}
