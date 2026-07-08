// =============================================================================
// Liquid Glass — Enhanced Background Pass WGSL Compute Shader
// WinIsland | Rust + wgpu
//
// Input is pre-blurred by Kawase blur passes (kawase_blur.wgsl).
// This shader applies refraction, curvature deformation, Fresnel, and compositing.
//
// Effects:
//   - SDF rounded-rect edge distance → displacement map decode
//   - Curvature deformation → convex lens UV warping
//   - Normal gradient UV offset → light refraction simulation
//   - Chromatic dispersion → edge-only R/B channel split
//   - Schlick Fresnel → edge highlight with secondary rim term
//   - Ambient diffuse → gradient-based directional light
//   - Semi-transparent tint blending → glass color
//   - Inner contour → 3D depth at edges
// =============================================================================

struct Params {
    max_displacement: f32,           // offset 0
    blur_sigma: f32,                 // offset 4
    blur_center_falloff: f32,        // offset 8
    fresnel_power: f32,              // offset 12
    fresnel_intensity: f32,          // offset 16
    glass_opacity: f32,              // offset 20
    _pad0: vec2<f32>,                // offset 24: align tint to 16-byte boundary
    tint: vec4<f32>,                 // offset 32
    curvature_strength: f32,         // offset 48
    _pad1a: f32,                     // offset 52
    _pad1b: f32,                     // offset 56
    _pad1c: f32,                     // offset 60 → total 64 bytes
}

@group(0) @binding(0) var input_tex: texture_2d<f32>;           // pre-blurred desktop
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var displacement_tex: texture_2d<f32>;   // R=dx, G=dy, B=edge

// --- Manual bilinear sampling (no sampler needed) ---

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

// --- Decode displacement map ---

fn decode_displacement(uv: vec2<f32>, dims: vec2<u32>) -> vec3<f32> {
    let dm = sample_bilinear(displacement_tex, uv, dims);
    let dx = (dm.r - 0.5) * 2.0 * params.max_displacement;
    let dy = (dm.g - 0.5) * 2.0 * params.max_displacement;
    return vec3(dx, dy, dm.b);
}

// --- Curvature deformation — convex lens UV warping ---

fn apply_curvature(uv: vec2<f32>, edge_factor: f32) -> vec2<f32> {
    let center = vec2(0.5);
    let to_center = center - uv;
    let dist = length(to_center);
    if dist < 0.001 {
        return uv;
    }
    let curvature = params.curvature_strength * edge_factor * dist * dist;
    return uv + normalize(to_center) * curvature * 0.15;
}

// --- Residual gradient blur (on top of Kawase pre-blur) ---

fn gradient_blur(
    tex: texture_2d<f32>,
    uv: vec2<f32>,
    dims: vec2<u32>,
    edge_factor: f32,
) -> vec4<f32> {
    let local_sigma = params.blur_sigma * mix(0.1, 1.0, edge_factor * params.blur_center_falloff);
    if local_sigma < 0.4 {
        return sample_bilinear(tex, uv, dims);
    }
    let dims_f = vec2<f32>(dims);
    let radius = i32(min(ceil(local_sigma * 3.0), 12.0));
    var sum = vec4<f32>(0.0);
    var wsum = 0.0f;
    for (var dy = -radius; dy <= radius; dy += 1) {
        for (var dx = -radius; dx <= radius; dx += 1) {
            let dist2 = f32(dx * dx + dy * dy);
            let w = exp(-0.5 * dist2 / (local_sigma * local_sigma));
            let suv = clamp(uv + vec2(f32(dx), f32(dy)) / dims_f, vec2(0.0), vec2(1.0));
            let coord = suv * dims_f - 0.5;
            let px = clamp(vec2<i32>(floor(coord)), vec2(0), vec2<i32>(dims) - 1);
            sum += textureLoad(tex, px, 0) * w;
            wsum += w;
        }
    }
    return sum / wsum;
}

// --- Schlick Fresnel ---

fn schlick_fresnel(edge_factor: f32) -> f32 {
    let F0 = 0.04;
    let cos_theta = 1.0 - edge_factor;
    let primary = F0 + (1.0 - F0) * pow(1.0 - cos_theta, params.fresnel_power);
    let rim = pow(edge_factor, 1.5) * 0.3;
    return (primary + rim) * params.fresnel_intensity;
}

// --- Ambient diffuse reflection ---

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

// --- Main ---

@compute @workgroup_size(8, 8)
fn liquid_glass_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let dims_f = vec2<f32>(dims);
    let uv = vec2<f32>(gid.xy) / dims_f;

    let disp = decode_displacement(uv, dims);
    let dx = disp.x;
    let dy = disp.y;
    let edge_factor = disp.z;

    let curved_uv = apply_curvature(uv, edge_factor);

    let refracted_uv = curved_uv + vec2(dx, dy) / dims_f;
    let refracted_color = sample_bilinear(input_tex, refracted_uv, dims);

    // Chromatic dispersion (edge only)
    var r_chan = refracted_color.r;
    var b_chan = refracted_color.b;
    if edge_factor > 0.3 {
        let dispersion = params.max_displacement * 0.03;
        let r_uv = curved_uv + vec2(dx + dispersion, dy) / dims_f;
        let b_uv = curved_uv + vec2(dx - dispersion, dy) / dims_f;
        r_chan = sample_bilinear(input_tex, r_uv, dims).r;
        b_chan = sample_bilinear(input_tex, b_uv, dims).b;
    }
    let dispersed_color = vec4(r_chan, refracted_color.g, b_chan, refracted_color.a);

    let blurred_color = gradient_blur(input_tex, refracted_uv, dims, edge_factor);
    let base_color = mix(dispersed_color, blurred_color, edge_factor * 0.55);

    let fresnel = schlick_fresnel(edge_factor);
    let diffuse = ambient_diffuse(uv, dims, edge_factor);

    var result = base_color;

    let tint_weight = params.tint.a * edge_factor;
    result = mix(result, params.tint, tint_weight);

    result = result + vec4(vec3(diffuse * 0.8, diffuse * 0.85, diffuse), 0.0);
    result = result + vec4(vec3(fresnel), 0.0);

    let inner_line = smoothstep(0.7, 0.85, edge_factor) * (1.0 - smoothstep(0.85, 1.0, edge_factor));
    result = result - vec4(vec3(inner_line * 0.03), 0.0);

    let center_dist = length(uv - vec2(0.5));
    let curvature_highlight = params.curvature_strength * exp(-center_dist * center_dist * 8.0) * 0.02;
    result = result + vec4(vec3(curvature_highlight), 0.0);

    result = clamp(result, vec4(0.0), vec4(1.0));
    result.a = 1.0;

    textureStore(output_tex, vec2<i32>(gid.xy), result);
}
