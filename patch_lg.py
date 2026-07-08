import sys
path = r"C:\Users\Administrator\WinIsland\src\utils\liquid_glass.rs"
with open(path, "r", encoding="utf-8") as f:
    content = f.read()

# 1. LiquidParams struct
content = content.replace(
    "curvature_strength: f32,  // 48\n    _pad1: [f32; 3],          // 52",
    "curvature_strength: f32,  // 48\n    margin: [f32; 2],         // 52\n    _pad1: f32,               // 60"
)

# 2. LiquidParams construction
old_p = (
'        let ep = expansion_progress.clamp(0.0, 1.0);\n'
'        let params = LiquidParams {\n'
'            max_displacement: 8.0 + 10.0 * ep,\n'
'            blur_sigma: blur_sigma.clamp(0.5, 20.0),\n'
'            blur_center_falloff: 0.80,\n'
'            fresnel_power: 2.2,\n'
'            fresnel_intensity: 0.05 + 0.15 * ep,\n'
'            glass_opacity: 1.0,\n'
'            _pad0: [0.0; 2],\n'
'            tint: [0.06, 0.06, 0.08, 0.16],\n'
'            curvature_strength: 0.4 + 0.3 * ep,\n'
'            _pad1: [0.0; 3],\n'
'        };'
)
new_p = (
'        let ep = expansion_progress.clamp(0.0, 1.0);\n'
'        let params = LiquidParams {\n'
'            max_displacement: 30.0 + 30.0 * ep,\n'
'            blur_sigma: 0.01,\n'
'            blur_center_falloff: 0.80,\n'
'            fresnel_power: 1.5,\n'
'            fresnel_intensity: 0.2 + 0.4 * ep,\n'
'            glass_opacity: 0.0,\n'
'            _pad0: [0.0; 2],\n'
'            tint: [0.0, 0.0, 0.0, 0.0],\n'
'            curvature_strength: 1.2 + 0.8 * ep,\n'
'            margin: [margin as f32, margin as f32],\n'
'            _pad1: 0.0,\n'
'        };'
)
content = content.replace(old_p, new_p)

# 3. Displacement map
content = content.replace(
    "let disp_pixels = generate_displacement_map(w, h, 0.35, 0.3, 0.55);",
    "let disp_pixels = generate_displacement_map(w, h, 0.3, 0.2, 0.6);"
)

# 4. Render body
old_r = (
"        let raw_pixels = unsafe { capture_region(hwnd, screen_x, screen_y, w, h)? };\n"
"        self.run_shader(&raw_pixels, w, h, blur_sigma, expansion_progress, time)"
)
new_r = (
"        let margin = (w.max(h) / 2) as i32;\n"
"        let cap_w = w + margin as u32 * 2;\n"
"        let cap_h = h + margin as u32 * 2;\n"
"        let raw_pixels = unsafe { crate::utils::wgc_capture::get_wgc_background(hwnd, screen_x - margin, screen_y - margin, cap_w, cap_h) }?;\n"
"        self.run_shader(&raw_pixels, w, h, cap_w, cap_h, margin, blur_sigma, expansion_progress, time)"
)
content = content.replace(old_r, new_r)

# 5. run_shader signature
old_sig = (
"    fn run_shader(\n"
"        &self,\n"
"        pixels: &[u8],\n"
"        w: u32,\n"
"        h: u32,\n"
"        blur_sigma: f32,\n"
"        expansion_progress: f32,\n"
"        _time: f32,"
)
new_sig = (
"    fn run_shader(\n"
"        &self,\n"
"        pixels: &[u8],\n"
"        w: u32,\n"
"        h: u32,\n"
"        cap_w: u32,\n"
"        cap_h: u32,\n"
"        margin: i32,\n"
"        blur_sigma: f32,\n"
"        expansion_progress: f32,\n"
"        _time: f32,"
)
content = content.replace(old_sig, new_sig)

# 6. extent -> input_extent/output_extent
old_ext = (
"        let extent = wgpu::Extent3d {\n"
"            width: w,\n"
"            height: h,\n"
"            depth_or_array_layers: 1,\n"
"        };\n"
"        let row_bytes = w * 4;\n"
"        let aligned_row = row_bytes.div_ceil(256) * 256;"
)
new_ext = (
"        let input_extent = wgpu::Extent3d {\n"
"            width: cap_w,\n"
"            height: cap_h,\n"
"            depth_or_array_layers: 1,\n"
"        };\n"
"        let output_extent = wgpu::Extent3d {\n"
"            width: w,\n"
"            height: h,\n"
"            depth_or_array_layers: 1,\n"
"        };\n"
"        let in_row_bytes = cap_w * 4;\n"
"        let in_aligned_row = in_row_bytes.div_ceil(256) * 256;\n"
"        let out_row_bytes = w * 4;"
)
content = content.replace(old_ext, new_ext)

# 7. displacement texture upload
content = content.replace(
    '"DisplacementMap",\n            extent,\n            &disp_pixels,\n            w,\n            h,\n            aligned_row,\n            row_bytes,',
    '"DisplacementMap",\n            output_extent,\n            &disp_pixels,\n            w,\n            h,\n            in_aligned_row,\n            out_row_bytes,'
)

# 8. input texture upload
content = content.replace(
    'upload_texture("Input", extent, pixels, w, h, aligned_row, row_bytes);',
    'upload_texture("Input", input_extent, pixels, cap_w, cap_h, in_aligned_row, in_row_bytes);'
)

# 9. Blur textures
content = content.replace(
    'label: Some("BlurTex A"),\n            size: extent,',
    'label: Some("BlurTex A"),\n            size: output_extent,'
)
content = content.replace(
    'label: Some("BlurTex B"),\n            size: extent,',
    'label: Some("BlurTex B"),\n            size: output_extent,'
)

# 10. Output texture
content = content.replace(
    'label: Some("Output"),\n            size: extent,',
    'label: Some("Output"),\n            size: output_extent,'
)

# 11. Readback fixes
content = content.replace(
    "let padded_size = (aligned_row * h) as u64;",
    "let padded_size = (in_aligned_row * h) as u64;"
)
content = content.replace(
    "let start = (row * aligned_row) as usize;",
    "let start = (row * in_aligned_row) as usize;"
)
content = content.replace(
    "bytes_per_row: Some(aligned_row),",
    "bytes_per_row: Some(in_aligned_row),"
)

with open(path, "w", encoding="utf-8") as f:
    f.write(content)

print("OK")
print("margin:", "margin: [f32; 2]" in content)
print("WGC:", "wgc_capture::get_wgc_background" in content)
print("cap_w:", "cap_w: u32" in content)
