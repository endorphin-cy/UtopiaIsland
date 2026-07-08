#![allow(dead_code)]
use std::cell::RefCell;
use std::time::Instant;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::core::Interface;

pub struct DdCapturer {
    d3d_device: ID3D11Device,
    d3d_context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging_texture: Option<ID3D11Texture2D>,
    staging_width: u32,
    staging_height: u32,
    last_frame: Option<(Vec<u8>, Instant)>,
}

thread_local! {
    static DD_STATE: RefCell<Option<DdCapturer>> = const { RefCell::new(None) };
}

impl DdCapturer {
    unsafe fn new(hwnd: HWND) -> Option<Self> {
        unsafe {
            // Get monitor
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mon_info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..std::mem::zeroed()
            };
            let _ = GetMonitorInfoW(monitor, &mut mon_info);

            // Create D3D11 device
            let mut d3d_device: Option<ID3D11Device> = None;
            let mut d3d_context: Option<ID3D11DeviceContext> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut d3d_device),
                None,
                Some(&mut d3d_context),
            )
            .ok()?;
            let d3d_device = d3d_device?;
            let d3d_context = d3d_context?;

            // Get DXGI factory from device
            let dxgi_device: IDXGIDevice = d3d_device.cast().ok()?;
            let adapter: IDXGIAdapter = dxgi_device.GetAdapter().ok()?;
            let adapter1: IDXGIAdapter1 = adapter.cast().ok()?;

            // Find the output matching our monitor
            let mut output_idx = 0u32;
            let output: IDXGIOutput = loop {
                let out = adapter1.EnumOutputs(output_idx);
                match out {
                    Ok(o) => {
                        if let Ok(desc) = o.GetDesc() {
                            let monitor_handle = desc.Monitor;
                            if monitor_handle == monitor {
                                break o;
                            }
                        }
                        output_idx += 1;
                    }
                    Err(_) => return None,
                }
            };

            let output1: IDXGIOutput1 = output.cast().ok()?;
            let duplication = output1.DuplicateOutput(&d3d_device).ok()?;

            Some(Self {
                d3d_device,
                d3d_context,
                duplication,
                staging_texture: None,
                staging_width: 0,
                staging_height: 0,
                last_frame: None,
            })
        }
    }

    unsafe fn ensure_staging(&mut self, w: u32, h: u32) -> Option<()> {
        if self.staging_width == w && self.staging_height == h && self.staging_texture.is_some() {
            return Some(());
        }
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            let staging_ptr: *mut Option<ID3D11Texture2D> = &mut staging as *mut _;
            self.d3d_device
                .CreateTexture2D(&desc, None, Some(staging_ptr))
                .ok()?;
            self.staging_texture = staging;
            self.staging_width = w;
            self.staging_height = h;
        }
        Some(())
    }

    unsafe fn try_grab(&mut self, local_x: i32, local_y: i32, w: u32, h: u32) -> Option<Vec<u8>> {
        unsafe {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;

            let result = self
                .duplication
                .AcquireNextFrame(100, &mut frame_info, &mut resource);

            if result.is_err() {
                // Return last frame if available and not too old
                if let Some((pixels, t)) = &self.last_frame
                    && t.elapsed().as_millis() < 50
                {
                    return Some(pixels.clone());
                }
                // Release any stale frame
                let _ = self.duplication.ReleaseFrame();
                return None;
            }

            let resource = resource?;
            let texture: ID3D11Texture2D = resource.cast().ok()?;

            // Get frame description

            self.ensure_staging(w, h)?;

            let frame_desc = self.duplication.GetDesc();
            let frame_w = frame_desc.ModeDesc.Width as i32;
            let frame_h = frame_desc.ModeDesc.Height as i32;
            let src_left = local_x.clamp(0, frame_w);
            let src_top = local_y.clamp(0, frame_h);
            let src_right = (local_x + w as i32).clamp(0, frame_w);
            let src_bottom = (local_y + h as i32).clamp(0, frame_h);

            if src_left >= src_right || src_top >= src_bottom {
                let _ = self.duplication.ReleaseFrame();
                return None;
            }

            let dst_x = (src_left - local_x) as u32;
            let dst_y = (src_top - local_y) as u32;
            let visible_w = (src_right - src_left) as usize;
            let visible_h = (src_bottom - src_top) as usize;
            let visible_x0 = dst_x as usize;
            let visible_y0 = dst_y as usize;
            let visible_x1 = visible_x0 + visible_w - 1;
            let visible_y1 = visible_y0 + visible_h - 1;

            let src_box = D3D11_BOX {
                left: src_left as u32,
                top: src_top as u32,
                front: 0,
                right: src_right as u32,
                bottom: src_bottom as u32,
                back: 1,
            };

            self.d3d_context.CopySubresourceRegion(
                self.staging_texture.as_ref()?,
                0,
                dst_x,
                dst_y,
                0,
                &texture,
                0,
                Some(&src_box),
            );

            let _ = self.duplication.ReleaseFrame();

            // Map and read
            let mut mapped: D3D11_MAPPED_SUBRESOURCE = std::mem::zeroed();
            self.d3d_context
                .Map(
                    self.staging_texture.as_ref()?,
                    0,
                    D3D11_MAP_READ,
                    0,
                    Some(&mut mapped as *mut _),
                )
                .ok()?;

            let src = mapped.pData as *const u8;
            let row_pitch = mapped.RowPitch as usize;
            let dst_w = w as usize;
            let dst_h = h as usize;

            let mut pixels = vec![0u8; dst_w * dst_h * 4];
            for y in 0..dst_h {
                let sample_y = y.clamp(visible_y0, visible_y1);
                for x in 0..dst_w {
                    let sample_x = x.clamp(visible_x0, visible_x1);
                    let src_px = src.add(sample_y * row_pitch + sample_x * 4);
                    let dst = (y * dst_w + x) * 4;
                    pixels[dst] = *src_px.add(2); // B -> R
                    pixels[dst + 1] = *src_px.add(1); // G -> G
                    pixels[dst + 2] = *src_px; // R -> B
                    pixels[dst + 3] = 255; // A
                }
            }

            self.d3d_context.Unmap(self.staging_texture.as_ref()?, 0);

            let result = pixels.clone();
            self.last_frame = Some((pixels, Instant::now()));
            Some(result)
        }
    }
}

pub unsafe fn get_dd_background(hwnd: HWND, sx: i32, sy: i32, w: u32, h: u32) -> Option<Vec<u8>> {
    unsafe {
        DD_STATE.with(|cell| {
            let mut opt = cell.borrow_mut();
            if opt.is_none() {
                *opt = DdCapturer::new(hwnd);
            }
            let capturer = opt.as_mut()?;
            let local_x = sx;
            let local_y = sy;
            capturer.try_grab(local_x, local_y, w, h)
        })
    }
}
