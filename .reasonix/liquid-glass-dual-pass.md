# Plan: Dual-Pass GPU Rendering Pipeline (Liquid Glass Core)

## Goal
Upgrade the existing single-pass liquid glass compute shader into a proper dual-pass GPU pipeline:
- **Pass 1 (Kawase Blur)**: Multi-layer Kawase blur preprocessing — 3 compute passes with ping-pong textures
- **Pass 2 (Enhanced Glass)**: Enhanced liquid glass shader with curvature deformation, improved Fresnel, ambient diffuse reflection

## Current Architecture
- `src/shaders/liquid_glass.wgsl` — single compute shader: displacement decode + refraction + chromatic dispersion + gradient blur + Fresnel + tint + inner contour
- `src/utils/liquid_glass.rs` — `LiquidGlassRenderer` with 1 compute pipeline, 1 bind group layout, CPU-generated displacement map
- Desktop captured via GDI `BitBlt`, uploaded as `input_tex`; displacement map generated on CPU and uploaded as `displacement_tex`
- Result read back to CPU → `skia_safe::Image`, composited by Skia for foreground UI

## File Changes

### 1. NEW: `src/shaders/kawase_blur.wgsl`
Kawase blur compute shader for desktop background preprocessing:
- `@compute @workgroup_size(8, 8)` entry point `kawase_blur`
- Bindings: input_tex (texture_2d), output_tex (storage, write), params (uniform), blur_sampler (sampler)
- Uniform `BlurParams`: texel_size (vec2<f32>) + radius (f32) = 16 bytes with padding
- Kawase kernel: samples 4 texels at (±0.5 ± radius) * texel_size offsets, averages them
- Single dispatch = one blur pass; Rust chains 3 passes with decreasing radius

### 2. MODIFY: `src/shaders/liquid_glass.wgsl`
Enhanced glass shader — keep all existing effects, add:
- **Curvature deformation** (new): distance-based UV remapping simulating convex lens curvature — stronger distortion near edges, controlled by `curvature_strength` uniform
- **Ambient diffuse reflection** (new): gradient-based normal estimation → directional light simulation for realistic glass light interaction
- **Enhanced Fresnel** (improved): Schlick-like approximation `F = F0 + (1-F0) * pow(1-NdotV, power)` with secondary rim term for richer edge glow
- Rename entry point from `main` to `liquid_glass_main`
- Add `curvature_strength` field to Params struct (new uniform field, extends from 64 → 80 bytes with padding)

### 3. MODIFY: `src/utils/liquid_glass.rs`
Refactored multi-pass pipeline:
- **New structs**: `BlurParams` (16 bytes), updated `LiquidParams` with curvature_strength
- **New fields on `LiquidGlassRenderer`**: blur_pipeline, blur_bind_group_layout, blur_sampler, glass_bind_group_layout
- **Two separate bind group layouts** (blur: input+output+uniform+sampler; glass: input+output+uniform+displacement)
- **Two pipeline layouts** (one per bind group layout)
- **New `run_blur_pass()`**: helper to dispatch a single Kawase blur pass (creates bind group, dispatches)
- **Refactored `run_shader()`**:
  1. Generate + upload displacement map (unchanged)
  2. Upload desktop capture as input_tex (unchanged)
  3. Create 2 ping-pong intermediate textures (RGBA8Unorm, storage+sample usage)
  4. Kawase blur pass 0: input_tex → tex_a (radius=3.0)
  5. Kawase blur pass 1: tex_a → tex_b (radius=2.0)
  6. Kawase blur pass 2: tex_b → tex_a (radius=1.0)
  7. Enhanced glass shader: tex_a (blurred) → output_tex
  8. Read back output → skia_safe::Image (unchanged)
- **Public API unchanged**: `get_liquid_glass_background()` signature identical, no changes to `render.rs` or `app.rs`

### 4. NO CHANGES NEEDED
- `src/core/render.rs` — API unchanged
- `src/window/app.rs` — no changes
- `Cargo.toml` — no new dependencies (wgpu 29 supports all needed features)

## Technical Details

### Kawase Blur Radius Schedule
3 passes with radii [3.0, 2.0, 1.0] × blur_factor. Each pass only 4 texture samples — very efficient. Progressive refinement eliminates box-blur artifacts. Total effective blur ≈ 6× blur_factor.

### Curvature Deimation
`curved_uv = center + (uv - center) * (1.0 - curvature * dist²)` — quadratic falloff, strongest at edges. Simulates convex glass lens. Controlled by new `curvature_strength` uniform (0 = flat, 1 = maximum curvature).

### Shared Bind Group Layout Strategy
- Blur BGL (4 entries): binding 0 texture, binding 1 storage_texture, binding 2 uniform, binding 3 sampler
- Glass BGL (4 entries): binding 0 texture, binding 1 storage_texture, binding 2 uniform, binding 3 texture (displacement)
- Two separate layouts because binding 3 types differ (sampler vs texture)
- Each pipeline gets its own `PipelineLayout` referencing its BGL

### Texture Lifecycle
- All GPU textures (input, displacement, blur intermediates, output) created fresh each frame
- Existing 100ms thread-local image cache in `get_liquid_glass_background()` handles frame dedup
- This matches the current pattern — no regression in memory behavior
