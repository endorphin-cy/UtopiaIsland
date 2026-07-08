use std::cell::RefCell;

use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::RoGetActivationFactory;

use windows::core::HSTRING;

pub struct WgcCapturer {
    d3d_device: ID3D11Device,
    d3d_context: ID3D11DeviceContext,
    _winrt_device: IDirect3DDevice,
    frame_pool: Direct3D11CaptureFramePool,
    _session: GraphicsCaptureSession,
    last_region: Option<(i32, i32, u32, u32, Vec<u8>)>,
    monitor_left: i32,
    monitor_top: i32,
    frame_width: u32,
    frame_height: u32,
    staging_width: u32,
    staging_height: u32,
    staging_texture: ID3D11Texture2D,
}

thread_local! {
    static WGC_STATE: RefCell<Option<WgcCapturer>> = const { RefCell::new(None) };
}

impl WgcCapturer {
    unsafe fn new(hwnd: HWND) -> Option<Self> {
        unsafe {
            // WDA_EXCLUDEFROMCAPTURE can black out capture on Win10; skip it

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

            // Get DXGI device from D3D11 device
            let dxgi_device: IDXGIDevice = windows::core::Interface::cast(&d3d_device).ok()?;

            // Create WinRT IDirect3DDevice
            let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device).ok()?;
            let winrt_device: IDirect3DDevice =
                windows::core::Interface::cast(&inspectable).ok()?;

            // Get monitor size
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mon_info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..std::mem::zeroed()
            };
            let _ = GetMonitorInfoW(monitor, &mut mon_info);
            let rc = mon_info.rcMonitor;
            let monitor_left = rc.left;
            let monitor_top = rc.top;
            let mon_w = (rc.right - rc.left) as u32;
            let mon_h = (rc.bottom - rc.top) as u32;

            // Create frame pool
            let size = windows::Graphics::SizeInt32 {
                Width: mon_w as i32,
                Height: mon_h as i32,
            };
            let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &winrt_device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                1,
                size,
            )
            .ok()?;

            // Create capture item from monitor via interop factory
            let class_id = HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureItem");
            let interop: IGraphicsCaptureItemInterop = RoGetActivationFactory(&class_id).ok()?;
            let item: GraphicsCaptureItem = interop.CreateForMonitor(monitor).ok()?;

            let session = frame_pool.CreateCaptureSession(&item).ok()?;
            let _: Result<_, _> = session.SetIsBorderRequired(false);
            let _: Result<_, _> = session.SetIsCursorCaptureEnabled(false);
            let _: Result<_, _> = session.StartCapture();

            // Create staging texture for CPU readback
            let desc = D3D11_TEXTURE2D_DESC {
                Width: 1,
                Height: 1,
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
            let mut staging: Option<ID3D11Texture2D> = None;
            d3d_device
                .CreateTexture2D(&desc, None, Some(&mut staging))
                .ok()?;
            let staging_texture = staging?;

            Some(Self {
                d3d_device,
                d3d_context,
                _winrt_device: winrt_device,
                frame_pool,
                _session: session,
                last_region: None,
                monitor_left,
                monitor_top,
                frame_width: mon_w,
                frame_height: mon_h,
                staging_width: 1,
                staging_height: 1,
                staging_texture,
            })
        }
    }

    unsafe fn ensure_staging_texture(&mut self, w: u32, h: u32) -> Option<()> {
        unsafe {
            if self.staging_width == w && self.staging_height == h {
                return Some(());
            }

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
            let mut staging: Option<ID3D11Texture2D> = None;
            self.d3d_device
                .CreateTexture2D(&desc, None, Some(&mut staging))
                .ok()?;
            self.staging_texture = staging?;
            self.staging_width = w;
            self.staging_height = h;
            Some(())
        }
    }

    unsafe fn try_grab_region(
        &mut self,
        local_x: i32,
        local_y: i32,
        w: u32,
        h: u32,
    ) -> Option<Vec<u8>> {
        unsafe {
            let frame = self.frame_pool.TryGetNextFrame().ok()?;
            let surface = frame.Surface().ok()?;

            // Get the underlying D3D11 texture via IDirect3DDxgiInterfaceAccess
            let dxgi_access: IDirect3DDxgiInterfaceAccess =
                windows::core::Interface::cast(&surface).ok()?;
            let texture: ID3D11Texture2D = dxgi_access.GetInterface().ok()?;

            self.ensure_staging_texture(w, h)?;

            let frame_w = self.frame_width as i32;
            let frame_h = self.frame_height as i32;
            let src_left = local_x.clamp(0, frame_w);
            let src_top = local_y.clamp(0, frame_h);
            let src_right = (local_x + w as i32).clamp(0, frame_w);
            let src_bottom = (local_y + h as i32).clamp(0, frame_h);

            if src_left >= src_right || src_top >= src_bottom {
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
                &self.staging_texture,
                0,
                dst_x,
                dst_y,
                0,
                &texture,
                0,
                Some(&src_box),
            );

            // Map staging texture for CPU read
            let mut mapped = std::mem::zeroed();
            self.d3d_context
                .Map(
                    &self.staging_texture,
                    0,
                    D3D11_MAP_READ,
                    0,
                    Some(&mut mapped),
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
                    pixels[dst] = *src_px.add(2);
                    pixels[dst + 1] = *src_px.add(1);
                    pixels[dst + 2] = *src_px;
                    pixels[dst + 3] = 255;
                }
            }

            self.d3d_context.Unmap(&self.staging_texture, 0);
            Some(pixels)
        }
    }
}

pub unsafe fn get_wgc_background(hwnd: HWND, sx: i32, sy: i32, w: u32, h: u32) -> Option<Vec<u8>> {
    unsafe {
        WGC_STATE.with(|cell| {
            let mut opt = cell.borrow_mut();
            if opt.is_none() {
                *opt = WgcCapturer::new(hwnd);
            }
            let capturer = opt.as_mut()?;

            let local_x = sx - capturer.monitor_left;
            let local_y = sy - capturer.monitor_top;

            if let Some(result) = capturer.try_grab_region(local_x, local_y, w, h) {
                capturer.last_region = Some((local_x, local_y, w, h, result.clone()));
                return Some(result);
            }

            if let Some((last_x, last_y, last_w, last_h, pixels)) = &capturer.last_region
                && *last_x == local_x
                && *last_y == local_y
                && *last_w == w
                && *last_h == h
            {
                return Some(pixels.clone());
            }

            None
        })
    }
}
