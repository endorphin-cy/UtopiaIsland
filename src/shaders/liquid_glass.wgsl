// =============================================================================
// Liquid Glass — Apple-style background refraction shader
// Ported from liquid-glass-react (https://github.com/nicepkg/liquid-glass-react)
//
// CSS/SVG-inspired macOS liquid glass trial.
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

@group(0) @binding(0) var raw_tex: texture_2d<f32>;
@group(0) @binding(1) var blur_tex: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(3) var<uniform> params: Params;
@group(0) @binding(4) var displacement_tex: texture_2d<f32>;

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

fn hash21(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(hash21(i), hash21(i + vec2(1.0, 0.0)), u.x),
        mix(hash21(i + vec2(0.0, 1.0)), hash21(i + vec2(1.0, 1.0)), u.x),
        u.y,
    );
}

fn svg_turbulence(uv: vec2<f32>) -> vec2<f32> {
    let p = uv * vec2(4.0, 3.0);
    let n1 = value_noise(p);
    let n2 = value_noise(p + vec2(19.17, 7.31));
    return vec2(n1 - 0.5, n2 - 0.5);
}

fn saturate_rgb(color: vec3<f32>, amount: f32) -> vec3<f32> {
    let luma = dot(color, vec3(0.299, 0.587, 0.114));
    return mix(vec3(luma), color, amount);
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
    return uv + normalize(to_center) * curvature * 0.30;
}

fn rect_edge_info(uv: vec2<f32>, dims_f: vec2<f32>) -> vec3<f32> {
    let px = uv * dims_f;
    let left = px.x;
    let right = dims_f.x - px.x;
    let top = px.y;
    let bottom = dims_f.y - px.y;
    let edge_px = min(min(left, right), min(top, bottom));
    let ring = 1.0 - smoothstep(4.0, 32.0, edge_px);

    var normal = vec2<f32>(0.0, 0.0);
    if left <= right && left <= top && left <= bottom {
        normal = vec2<f32>(-1.0, 0.0);
    } else if right <= top && right <= bottom {
        normal = vec2<f32>(1.0, 0.0);
    } else if top <= bottom {
        normal = vec2<f32>(0.0, -1.0);
    } else {
        normal = vec2<f32>(0.0, 1.0);
    }
    return vec3<f32>(normal, ring);
}

@compute @workgroup_size(8, 8)
fn liquid_glass_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let disp_dims = textureDimensions(displacement_tex);
    let input_dims = textureDimensions(blur_tex);
    if gid.x >= disp_dims.x || gid.y >= disp_dims.y { return; }
    let disp_dims_f = vec2<f32>(disp_dims);
    let input_dims_f = vec2<f32>(input_dims);
    let uv = vec2<f32>(gid.xy) / disp_dims_f;
    let input_px = vec2<f32>(gid.xy) + vec2(params.margin_x, params.margin_y);

    let disp = decode_displacement(uv, disp_dims);
    let raw_edge_factor = disp.z;
    let rect_edge = rect_edge_info(uv, disp_dims_f);
    let shape_edge = smoothstep(0.28, 0.58, raw_edge_factor);
    let edge_factor = max(rect_edge.z, shape_edge);
    let edge_adhesion = pow(edge_factor, 1.55);
    let refraction_level = 0.45;
    let rect_push = rect_edge.xy * params.max_displacement * 2.40 * refraction_level * edge_adhesion;
    let svg_warp = svg_turbulence(uv) * params.max_displacement * 1.60 * refraction_level * edge_factor;
    let dx = disp.x * shape_edge * 2.10 * refraction_level + rect_push.x + svg_warp.x;
    let dy = disp.y * shape_edge * 2.10 * refraction_level + rect_push.y + svg_warp.y;

    let base_uv = input_px / input_dims_f;
    let base_rgb = sample_bilinear(blur_tex, base_uv, input_dims).rgb;

    let curved_uv = apply_curvature(uv, edge_factor);
    let curvature_px = (curved_uv - uv) * disp_dims_f;
    let refract_uv = (input_px + curvature_px + vec2(dx, dy)) / input_dims_f;
    let refracted_rgb = sample_bilinear(blur_tex, refract_uv, input_dims).rgb;
    let glass_rgb = mix(base_rgb, refracted_rgb, min(refraction_level, refraction_level * edge_factor));
    var final_rgb = glass_rgb;

    let px = uv * disp_dims_f;
    let top_px = px.y;
    let left_px = px.x;
    let right_px = disp_dims_f.x - px.x;
    let bottom_px = disp_dims_f.y - px.y;
    let min_edge_px = min(min(left_px, right_px), min(top_px, bottom_px));
    let hairline = 1.0 - smoothstep(0.0, 1.4, min_edge_px);
    let top_line = (1.0 - smoothstep(0.0, 2.2, top_px)) * 0.22;
    let left_line = (1.0 - smoothstep(0.0, 2.0, left_px)) * 0.18;
    let right_line = (1.0 - smoothstep(0.0, 1.8, right_px)) * 0.13;
    let bottom_line = (1.0 - smoothstep(0.0, 1.6, bottom_px)) * 0.10;
    let edge_shine = hairline * 0.16 + top_line + left_line + right_line + bottom_line;
    let mirror_opacity = 0.50;
    let mirror_saturation = 10.0;
    let saturated_mirror = clamp(saturate_rgb(glass_rgb, mirror_saturation), vec3(0.0), vec3(1.0));
    let mirror_rgb = mix(saturated_mirror, vec3(1.0), 0.72);
    final_rgb = final_rgb + mirror_rgb * edge_shine * mirror_opacity;

    var result = vec4(final_rgb, 1.0);

    result = clamp(result, vec4(0.0), vec4(1.0));
    result.a = 1.0;

    textureStore(output_tex, vec2<i32>(gid.xy), result);
}
