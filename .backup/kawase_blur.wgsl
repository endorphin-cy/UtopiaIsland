// =============================================================================
// Kawase Blur — Multi-Pass Gaussian Approximation Compute Shader
// WinIsland | Rust + wgpu
//
// Each dispatch performs one Kawase blur pass — samples 4 texels at
// (±0.5 ± radius) * texel_size from the pixel center and averages them.
// Multiple passes with decreasing radius approximate a Gaussian blur
// with far fewer texture samples than a single large kernel.
// =============================================================================

struct BlurParams {
    texel_size: vec2<f32>,   // 1.0 / texture_dimensions
    radius: f32,             // sample offset radius
    _pad: f32,               // alignment padding to 16 bytes
}

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> params: BlurParams;
@group(0) @binding(3) var blur_sampler: sampler;

@compute @workgroup_size(8, 8)
fn kawase_blur(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let offset = params.texel_size * params.radius;

    // Kawase kernel: 4 samples at half-pixel ± radius offsets
    var sum = textureSampleLevel(input_tex, blur_sampler, uv + vec2(-offset.x, -offset.y), 0.0);
    sum += textureSampleLevel(input_tex, blur_sampler, uv + vec2( offset.x, -offset.y), 0.0);
    sum += textureSampleLevel(input_tex, blur_sampler, uv + vec2(-offset.x,  offset.y), 0.0);
    sum += textureSampleLevel(input_tex, blur_sampler, uv + vec2( offset.x,  offset.y), 0.0);

    textureStore(output_tex, vec2<i32>(gid.xy), sum * 0.25);
}
