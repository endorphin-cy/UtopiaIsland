#![allow(dead_code)]

use skia_safe::{AlphaType, ColorType, Data, ISize, Image, ImageInfo, images};
use std::cell::RefCell;
use std::sync::Mutex;
use std::time::Instant;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowDisplayAffinity, SW_HIDE, SW_SHOW, SetWindowDisplayAffinity, ShowWindow,
    WDA_EXCLUDEFROMCAPTURE, WINDOW_DISPLAY_AFFINITY,
};

type GlassCacheEntry = (Image, Instant, i32, i32, u32, u32);

thread_local! {
    static LIQUID_CACHE: RefCell<Option<GlassCacheEntry>> = const { RefCell::new(None) };
}

// =============================================================================
// Uniform buffers �?match WGSL structs exactly
// =============================================================================

/// Kawase blur uniform �?16 bytes
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurParams {
    texel_size: [f32; 2],
    radius: f32,
    _pad: f32,
}

/// Liquid glass uniform �?64 bytes
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct LiquidParams {
    max_displacement: f32,    // 0
    blur_sigma: f32,          // 4
    blur_center_falloff: f32, // 8
    fresnel_power: f32,       // 12
    fresnel_intensity: f32,   // 16
    glass_opacity: f32,       // 20
    _pad0: [f32; 2],          // 24
    tint: [f32; 4],           // 32
    curvature_strength: f32,  // 48
    margin_x: f32,            // 52
    margin_y: f32,            // 56
    _pad1: f32,               // 60 �?total 64
}

const _: () = assert!(std::mem::size_of::<BlurParams>() == 16);
const _: () = assert!(std::mem::size_of::<LiquidParams>() == 64);

// =============================================================================
// Displacement map generator
// =============================================================================

fn rounded_rect_sdf(px: f32, py: f32, half_w: f32, half_h: f32, r: f32) -> f32 {
    let qx = px.abs() - half_w + r;
    let qy = py.abs() - half_h + r;
    qx.max(qy).min(0.0) + (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() - r
}

fn smoothstep(a: f32, b: f32, t: f32) -> f32 {
    let t = ((t - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn generate_displacement_map(
    w: u32,
    h: u32,
    half_w: f32,
    half_h: f32,
    corner_radius: f32,
) -> Vec<u8> {
    let w_i = w as i32;
    let h_i = h as i32;
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let mut raw: Vec<(f32, f32)> = Vec::with_capacity((w * h) as usize);
    let mut max_scale = 0.0f32;

    for y in 0..h_i {
        for x in 0..w_i {
            let ix = (x as f32 / w as f32) - 0.5;
            let iy = (y as f32 / h as f32) - 0.5;
            let d = rounded_rect_sdf(ix, iy, half_w, half_h, corner_radius);
            let displacement = smoothstep(0.8, 0.0, d - 0.15);
            let scaled = smoothstep(0.0, 1.0, displacement);
            let target_x = ix * scaled + 0.5;
            let target_y = iy * scaled + 0.5;
            let dx = target_x * w as f32 - x as f32;
            let dy = target_y * h as f32 - y as f32;
            max_scale = max_scale.max(dx.abs()).max(dy.abs());
            raw.push((dx, dy));
        }
    }

    if max_scale < 1.0 {
        max_scale = 1.0;
    }

    let mut idx = 0usize;
    for _y in 0..h_i {
        for _x in 0..w_i {
            let (dx, dy) = raw[idx];
            let r = (dx / max_scale + 0.5).clamp(0.0, 1.0);
            let g = (dy / max_scale + 0.5).clamp(0.0, 1.0);
            let edge_factor = ((dx.abs() + dy.abs()) / (max_scale * 2.0)).clamp(0.0, 1.0);
            let pi = idx * 4;
            pixels[pi] = (r * 255.0) as u8;
            pixels[pi + 1] = (g * 255.0) as u8;
            pixels[pi + 2] = (edge_factor * 255.0) as u8;
            pixels[pi + 3] = 255u8;
            idx += 1;
        }
    }

    pixels
}

// =============================================================================
// LiquidGlassRenderer �?Dual-Pass Pipeline with Offset Capture
//
// Anti-feedback: captures desktop from a horizontally offset position
// (same technique as glass.rs) to avoid self-capture stacking.
// The Kawase blur further dilutes any residual artifacts.
// =============================================================================

pub struct LiquidGlassRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    blur_pipeline: wgpu::ComputePipeline,
    blur_bind_group_layout: wgpu::BindGroupLayout,
    blur_sampler: wgpu::Sampler,
    glass_pipeline: wgpu::ComputePipeline,
    glass_bind_group_layout: wgpu::BindGroupLayout,
    overlay_pipeline: wgpu::ComputePipeline,
    overlay_bind_group_layout: wgpu::BindGroupLayout,
}

impl LiquidGlassRenderer {
    pub fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("LiquidGlass Device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .ok()?;

        let blur_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Kawase Blur Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/kawase_blur.wgsl").into()),
        });
        let glass_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("LiquidGlass Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/liquid_glass.wgsl").into()),
        });
        let overlay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("LiquidGlass Overlay Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../shaders/liquid_glass_overlay.wgsl").into(),
            ),
        });

        // Blur BGL: input tex + output storage + uniform + sampler
        let blur_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Kawase Blur BGL"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        // Glass BGL: input tex + output storage + uniform + displacement tex
        let glass_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("LiquidGlass BGL"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            });

        // Overlay BGL: output storage + uniform + displacement tex (no input capture)
        let overlay_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("LiquidGlass Overlay BGL"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            });

        let blur_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Kawase Blur Pipeline Layout"),
            bind_group_layouts: &[Some(&blur_bind_group_layout)],
            immediate_size: 0,
        });
        let glass_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("LiquidGlass Pipeline Layout"),
                bind_group_layouts: &[Some(&glass_bind_group_layout)],
                immediate_size: 0,
            });

        let blur_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Kawase Blur Pipeline"),
            layout: Some(&blur_pipeline_layout),
            module: &blur_shader,
            entry_point: Some("kawase_blur"),
            compilation_options: Default::default(),
            cache: None,
        });
        let glass_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("LiquidGlass Pipeline"),
            layout: Some(&glass_pipeline_layout),
            module: &glass_shader,
            entry_point: Some("liquid_glass_main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let overlay_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("LiquidGlass Overlay Pipeline Layout"),
                bind_group_layouts: &[Some(&overlay_bind_group_layout)],
                immediate_size: 0,
            });
        let overlay_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("LiquidGlass Overlay Pipeline"),
            layout: Some(&overlay_pipeline_layout),
            module: &overlay_shader,
            entry_point: Some("liquid_glass_overlay_main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let blur_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Kawase Blur Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        log::info!("LiquidGlass renderer initialized (dual-pass: Kawase blur + enhanced glass)");

        Some(Self {
            device,
            queue,
            blur_pipeline,
            blur_bind_group_layout,
            blur_sampler,
            glass_pipeline,
            glass_bind_group_layout,
            overlay_pipeline,
            overlay_bind_group_layout,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        hwnd: windows::Win32::Foundation::HWND,
        screen_x: i32,
        screen_y: i32,
        w: u32,
        h: u32,
        blur_sigma: f32,
        expansion_progress: f32,
        time: f32,
    ) -> Option<Image> {
        if w == 0 || h == 0 || w > 2048 || h > 2048 {
            return None;
        }

        let margin = (w.max(h) / 8).clamp(16, 48) as i32;
        let cap_w = w + margin as u32 * 2;
        let cap_h = h + margin as u32 * 2;
        let raw_pixels = unsafe {
            crate::utils::wgc_capture::get_wgc_background(
                hwnd,
                screen_x - margin,
                screen_y - margin,
                cap_w,
                cap_h,
            )
        }?;
        self.run_shader(
            &raw_pixels,
            w,
            h,
            cap_w,
            cap_h,
            margin,
            blur_sigma,
            expansion_progress,
            time,
        )
    }

    /// Capture-free overlay render ? desktop shows through transparent window.
    /// Only edge effects are rendered: Fresnel glow, curvature, ambient diffuse.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn render_overlay(
        &self,
        w: u32,
        h: u32,
        expansion_progress: f32,
        time: f32,
    ) -> Option<Image> {
        if w == 0 || h == 0 || w > 2048 || h > 2048 {
            return None;
        }

        let output_extent = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };

        // Displacement map (same as always, no capture needed)
        let disp_pixels = generate_displacement_map(w, h, 0.3, 0.2, 0.6);
        let disp_texture =
            self.upload_texture_overlay("DisplacementMap", output_extent, &disp_pixels, w, h);
        let disp_view = disp_texture.create_view(&Default::default());

        // Output texture
        let output_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Overlay Output"),
            size: output_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output_texture.create_view(&Default::default());

        #[repr(C)]
        #[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
        struct OverlayParams {
            fresnel_power: f32,
            fresnel_intensity: f32,
            curvature_strength: f32,
            _std140_pad0: f32, // align vec4<f32> to offset 16
            tint: [f32; 4],
            time: f32,
            _std140_pad1: f32, // align vec2<f32> to offset 40
            _pad: [f32; 2],
        }

        let ep = expansion_progress.clamp(0.0, 1.0);
        let params = OverlayParams {
            fresnel_power: 1.5,
            fresnel_intensity: 0.2 + 0.4 * ep,
            curvature_strength: 1.2 + 0.8 * ep,
            _std140_pad0: 0.0,
            tint: [0.08, 0.09, 0.16, 0.0],
            time,
            _std140_pad1: 0.0,
            _pad: [0.0; 2],
        };
        let ub = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("OverlayParams"),
            size: std::mem::size_of::<OverlayParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&ub, 0, bytemuck::bytes_of(&params));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("OverlayBG"),
            layout: &self.overlay_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: ub.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&disp_view),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("LiquidGlass Overlay Encoder"),
            });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("LiquidGlass Overlay"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.overlay_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }

        let out_row_bytes = w * 4;
        let out_aligned_row = out_row_bytes.div_ceil(256) * 256;
        let buffer_size = out_aligned_row as u64 * h as u64;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Overlay Readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut copy_enc = self.device.create_command_encoder(&Default::default());
        copy_enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(out_aligned_row),
                    rows_per_image: Some(h),
                },
            },
            output_extent,
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.submit(std::iter::once(copy_enc.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .ok()?;

        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(Ok(())) => {}
            _ => {
                log::error!("LiquidGlass Overlay: GPU readback failed");
                return None;
            }
        }

        let data = slice.get_mapped_range();
        let padded_data = data.to_vec();
        drop(data);
        readback.unmap();

        let mut result = Vec::with_capacity((out_row_bytes * h) as usize);
        for row in 0..h {
            let start = (row * out_aligned_row) as usize;
            result.extend_from_slice(&padded_data[start..start + out_row_bytes as usize]);
        }

        let info = ImageInfo::new(
            ISize::new(w as i32, h as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            None,
        );
        images::raster_from_data(&info, Data::new_copy(&result), out_row_bytes as usize)
    }

    #[allow(dead_code)]
    fn upload_texture_overlay(
        &self,
        label: &str,
        extent: wgpu::Extent3d,
        pixels: &[u8],
        tex_w: u32,
        tex_h: u32,
    ) -> wgpu::Texture {
        let row_bytes = tex_w * 4;
        let aligned_row = row_bytes.div_ceil(256) * 256;
        let mut padded = vec![0u8; aligned_row as usize * tex_h as usize];
        for y in 0..tex_h {
            let src_start = (y * row_bytes) as usize;
            let dst_start = (y * aligned_row) as usize;
            if src_start + row_bytes as usize <= pixels.len() {
                padded[dst_start..dst_start + row_bytes as usize]
                    .copy_from_slice(&pixels[src_start..src_start + row_bytes as usize]);
            }
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(aligned_row),
                rows_per_image: Some(tex_h),
            },
            extent,
        );
        texture
    }

    #[allow(clippy::too_many_arguments)]
    fn run_shader(
        &self,
        pixels: &[u8],
        w: u32,
        h: u32,
        cap_w: u32,
        cap_h: u32,
        margin: i32,
        blur_sigma: f32,
        expansion_progress: f32,
        _time: f32,
    ) -> Option<Image> {
        let input_extent = wgpu::Extent3d {
            width: cap_w,
            height: cap_h,
            depth_or_array_layers: 1,
        };
        let output_extent = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let in_row_bytes = cap_w * 4;
        let in_aligned_row = in_row_bytes.div_ceil(256) * 256;
        let out_row_bytes = w * 4;
        let out_aligned_row = out_row_bytes.div_ceil(256) * 256;

        // Displacement map
        let disp_pixels = generate_displacement_map(w, h, 0.3, 0.2, 0.6);
        let disp_texture = self.upload_texture(
            "DisplacementMap",
            output_extent,
            &disp_pixels,
            w,
            h,
            in_aligned_row,
            out_row_bytes,
        );

        // Desktop capture
        let input_texture = self.upload_texture(
            "Input",
            input_extent,
            pixels,
            cap_w,
            cap_h,
            in_aligned_row,
            in_row_bytes,
        );

        // Ping-pong blur textures
        let tex_a = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("BlurTex A"),
            size: output_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let tex_b = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("BlurTex B"),
            size: output_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });

        let input_view = input_texture.create_view(&Default::default());
        let tex_a_view = tex_a.create_view(&Default::default());
        let tex_b_view = tex_b.create_view(&Default::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("LiquidGlass Encoder"),
            });

        // Only run Kawase blur passes when sigma is meaningful
        let glass_input_view = if blur_sigma > 0.5 {
            let texel_size = [1.0 / w as f32, 1.0 / h as f32];
            let blur_factor = (blur_sigma / 2.5).clamp(0.3, 2.0);
            let radii = [3.0 * blur_factor, 2.0 * blur_factor, 1.0 * blur_factor];

            for (i, &radius) in radii.iter().enumerate() {
                let (src, dst) = match i {
                    0 => (&input_view, &tex_a_view),
                    1 => (&tex_a_view, &tex_b_view),
                    _ => (&tex_b_view, &tex_a_view),
                };
                let params = BlurParams {
                    texel_size,
                    radius,
                    _pad: 0.0,
                };
                let ub = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("BlurParams"),
                    size: 16,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.queue.write_buffer(&ub, 0, bytemuck::bytes_of(&params));

                let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("BlurBG"),
                    layout: &self.blur_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(src),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(dst),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: ub.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(&self.blur_sampler),
                        },
                    ],
                });
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("Kawase Blur"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.blur_pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
            }
            tex_a.create_view(&Default::default())
        } else {
            // Skip blur entirely �?use raw desktop capture directly
            input_texture.create_view(&Default::default())
        };

        // Glass pass
        let output_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Output"),
            size: output_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let output_view = output_texture.create_view(&Default::default());
        let disp_view = disp_texture.create_view(&Default::default());

        let ep = expansion_progress.clamp(0.0, 1.0);
        let params = LiquidParams {
            max_displacement: (margin as f32 * (0.55 + 0.25 * ep)).max(8.0),
            blur_sigma: 0.01,
            blur_center_falloff: 0.80,
            fresnel_power: 1.5,
            fresnel_intensity: 0.2 + 0.4 * ep,
            glass_opacity: 0.0,
            _pad0: [0.0; 2],
            tint: [0.0, 0.0, 0.0, 0.0],
            curvature_strength: 1.2 + 0.8 * ep,
            margin_x: margin as f32,
            margin_y: margin as f32,
            _pad1: 0.0,
        };
        let ub = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GlassParams"),
            size: std::mem::size_of::<LiquidParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&ub, 0, bytemuck::bytes_of(&params));

        let glass_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("GlassBG"),
            layout: &self.glass_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&glass_input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ub.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&disp_view),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("LiquidGlass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.glass_pipeline);
            pass.set_bind_group(0, &glass_bg, &[]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));

        // Read back
        let padded_size = (out_aligned_row * h) as u64;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Readback"),
            size: padded_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut copy_enc = self.device.create_command_encoder(&Default::default());
        copy_enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(out_aligned_row),
                    rows_per_image: Some(h),
                },
            },
            output_extent,
        );
        self.queue.submit(std::iter::once(copy_enc.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .ok()?;

        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(Ok(())) => {}
            _ => {
                log::error!("LiquidGlass: GPU readback failed");
                return None;
            }
        }

        let data = slice.get_mapped_range();
        let padded_data = data.to_vec();
        drop(data);
        readback.unmap();

        let mut result = Vec::with_capacity((out_row_bytes * h) as usize);
        for row in 0..h {
            let start = (row * out_aligned_row) as usize;
            result.extend_from_slice(&padded_data[start..start + out_row_bytes as usize]);
        }

        let info = ImageInfo::new(
            ISize::new(w as i32, h as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            None,
        );
        images::raster_from_data(&info, Data::new_copy(&result), (w * 4) as usize)
    }

    #[allow(clippy::too_many_arguments)]
    fn upload_texture(
        &self,
        label: &str,
        extent: wgpu::Extent3d,
        pixels: &[u8],
        _w: u32,
        h: u32,
        aligned_row: u32,
        row_bytes: u32,
    ) -> wgpu::Texture {
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut padded = Vec::with_capacity((aligned_row * h) as usize);
        for row in 0..h {
            let off = (row * row_bytes) as usize;
            padded.extend_from_slice(&pixels[off..off + row_bytes as usize]);
            padded.resize(padded.len() + (aligned_row - row_bytes) as usize, 0);
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(aligned_row),
                rows_per_image: Some(h),
            },
            extent,
        );
        tex
    }
}

// =============================================================================
// GDI screen capture �?OFFSET to avoid self-capture feedback
// Same technique as glass.rs: captures from a horizontally shifted position
// so the island's previous frame is not in the capture region.
// =============================================================================

#[allow(dead_code)]
unsafe fn capture_region(
    hwnd: windows::Win32::Foundation::HWND,
    sx: i32,
    sy: i32,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    unsafe {
        let margin = (w.max(h) / 2) as i32;
        let cap_w = w + margin as u32 * 2;
        let cap_h = h + margin as u32 * 2;

        // Strategy 1: Try WDA_EXCLUDEFROMCAPTURE (invisible to capture, visible on screen)
        let mut prev_affinity_raw: u32 = 0;
        let _ = GetWindowDisplayAffinity(hwnd, &mut prev_affinity_raw);
        let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);

        let pixels_wda = do_capture(sx, sy, margin, cap_w, cap_h);

        // Restore affinity immediately
        let _ = SetWindowDisplayAffinity(hwnd, WINDOW_DISPLAY_AFFINITY(prev_affinity_raw));

        // Check if WDA worked: sample a few pixels in the center of the capture.
        // If the island is dark (typical), the center should not be pure black
        // when capturing real desktop content behind it.
        if let Some(ref px) = pixels_wda {
            let cx = cap_w as usize / 2;
            let cy = cap_h as usize / 2;
            let idx = (cy * cap_w as usize + cx) * 4;
            let is_black = idx + 2 < px.len() && px[idx] < 5 && px[idx + 1] < 5 && px[idx + 2] < 5;
            if !is_black {
                // WDA worked �?crop and return
                return crop_center(px, w, h, cap_w, margin);
            }
        }

        // Strategy 2: WDA failed (older GPU, Remote Desktop, etc.)
        // Hide window, capture, then show. Brief sleep lets compositor update.
        let _ = ShowWindow(hwnd, SW_HIDE);
        std::thread::sleep(std::time::Duration::from_millis(2));

        let pixels_hide = do_capture(sx, sy, margin, cap_w, cap_h);

        let _ = ShowWindow(hwnd, SW_SHOW);

        if let Some(ref px) = pixels_hide {
            return crop_center(px, w, h, cap_w, margin);
        }

        None
    }
}

#[allow(dead_code)]
fn do_capture(sx: i32, sy: i32, margin: i32, cap_w: u32, cap_h: u32) -> Option<Vec<u8>> {
    unsafe {
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            return None;
        }
        let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
        if hdc_mem.is_invalid() {
            ReleaseDC(None, hdc_screen);
            return None;
        }
        let hbm = CreateCompatibleBitmap(hdc_screen, cap_w as i32, cap_h as i32);
        if hbm.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(None, hdc_screen);
            return None;
        }
        let old = SelectObject(hdc_mem, hbm.into());

        let _ = BitBlt(
            hdc_mem,
            0,
            0,
            cap_w as i32,
            cap_h as i32,
            Some(hdc_screen),
            sx - margin,
            sy - margin,
            SRCCOPY,
        );

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = cap_w as i32;
        bmi.bmiHeader.biHeight = -(cap_h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;

        let pixel_count = (cap_w * cap_h * 4) as usize;
        let mut pixels = vec![0u8; pixel_count];
        GetDIBits(
            hdc_mem,
            hbm,
            0,
            cap_h,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        SelectObject(hdc_mem, old);
        let _ = DeleteObject(hbm.into());
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(None, hdc_screen);

        // BGRA -> RGBA
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.swap(0, 2);
            chunk[3] = 255;
        }

        Some(pixels)
    }
}

#[allow(dead_code)]
fn crop_center(all_pixels: &[u8], w: u32, h: u32, cap_w: u32, margin: i32) -> Option<Vec<u8>> {
    let mut result = Vec::with_capacity((w * h * 4) as usize);
    let src_row = (cap_w * 4) as usize;
    let margin_px = margin as usize;
    for y in margin_px..margin_px + h as usize {
        let row_start = y * src_row + margin_px * 4;
        result.extend_from_slice(&all_pixels[row_start..row_start + (w * 4) as usize]);
    }
    Some(result)
}

// =============================================================================
// Public API
// =============================================================================

#[allow(clippy::too_many_arguments)]
pub fn get_liquid_glass_background(
    renderer: &Mutex<Option<LiquidGlassRenderer>>,
    hwnd: windows::Win32::Foundation::HWND,
    screen_x: i32,
    screen_y: i32,
    w: u32,
    h: u32,
    blur_sigma: f32,
    expansion_progress: f32,
    time: f32,
) -> Option<Image> {
    if w == 0 || h == 0 {
        return None;
    }

    let cached = LIQUID_CACHE.with(|cell| {
        let cache = cell.borrow();
        if let Some((img, t, cx, cy, cw, ch)) = cache.as_ref()
            && t.elapsed().as_millis() < 16
            && *cx == screen_x
            && *cy == screen_y
            && *cw == w
            && *ch == h
        {
            return Some(img.clone());
        }
        None
    });
    if let Some(img) = cached {
        return Some(img);
    }

    let result = renderer.lock().ok()?.as_ref()?.render(
        hwnd,
        screen_x,
        screen_y,
        w,
        h,
        blur_sigma,
        expansion_progress,
        time,
    );

    if let Some(ref img) = result {
        LIQUID_CACHE.with(|cell| {
            *cell.borrow_mut() = Some((img.clone(), Instant::now(), screen_x, screen_y, w, h));
        });
    }
    result
}

pub fn clear_liquid_cache() {
    LIQUID_CACHE.with(|cell| {
        *cell.borrow_mut() = None;
    });
}
