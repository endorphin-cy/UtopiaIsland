use skia_safe::{
    AlphaType, ColorType, Data, ISize, Image, ImageInfo, Paint, image_filters, images, surfaces,
};
use std::cell::RefCell;
use std::time::Instant;
use windows::Win32::Foundation::HWND;

type GlassCacheEntry = (Image, Instant, i32, i32, u32, u32);

thread_local! {
    static GLASS_CACHE: RefCell<Option<GlassCacheEntry>> = const { RefCell::new(None) };
}

/// Frosted dark glass backdrop: captures the island region + margin from the
/// desktop via WGC (Windows Graphics Capture), then applies a heavy blur
/// (sigma ~40). A strong darkening blend (Multiply + dark base) guarantees
/// the signature dark glass look.
///
/// WGC is hardware-accelerated and provides real-time frame capture without
/// cursor artifacts or dithering stripes.
#[allow(dead_code)]
pub fn get_glass_background(
    hwnd: HWND,
    screen_x: i32,
    screen_y: i32,
    w: u32,
    h: u32,
    blur_sigma: f32,
) -> Option<Image> {
    if w == 0 || h == 0 {
        return None;
    }

    let cached = GLASS_CACHE.with(|cell| {
        let cache = cell.borrow();
        if let Some((img, time, ..)) = cache.as_ref()
            && time.elapsed().as_millis() < 200
        {
            return Some(img.clone());
        }
        None
    });
    if let Some(img) = cached {
        return Some(img);
    }

    // SAFETY: capture_and_blur validates inputs internally.
    let result = unsafe { capture_and_blur(hwnd, screen_x, screen_y, w, h, blur_sigma) };

    if let Some(ref img) = result {
        GLASS_CACHE.with(|cell| {
            *cell.borrow_mut() = Some((img.clone(), Instant::now(), screen_x, screen_y, w, h));
        });
    }

    result
}

pub fn clear_glass_cache() {
    GLASS_CACHE.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

/// Captures the island region + margin from the desktop via WGC, heavily blurs,
/// crops to the island area.
///
/// WGC provides real-time hardware-accelerated capture without cursor artifacts.
#[allow(dead_code)]
unsafe fn capture_and_blur(
    hwnd: HWND,
    sx: i32,
    sy: i32,
    w: u32,
    h: u32,
    blur_sigma: f32,
) -> Option<Image> {
    let downscale = 4u32;
    let margin = (w.max(h) / downscale) as i32;
    let cap_w = (w as i32 + 2 * margin).max(1) as u32;
    let cap_h = (h as i32 + 2 * margin).max(1) as u32;

    // WGC capture — hardware accelerated, cursor-free, from exact position.
    // On cold start (first call), WGC may need a frame or two before
    // the frame pool delivers data. Retry once to avoid a blank frame.
    let pixels = unsafe {
        crate::utils::wgc_capture::get_wgc_background(hwnd, sx - margin, sy - margin, cap_w, cap_h)
            .or_else(|| {
                crate::utils::wgc_capture::get_wgc_background(
                    hwnd,
                    sx - margin,
                    sy - margin,
                    cap_w,
                    cap_h,
                )
            })?
    };

    // WGC returns RGBA pixels; Skia BGRA8888 needs R<->B swap
    let mut bgra = pixels;
    for chunk in bgra.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    let info = ImageInfo::new(
        ISize::new(cap_w as i32, cap_h as i32),
        ColorType::BGRA8888,
        AlphaType::Opaque,
        None,
    );
    let data = Data::new_copy(&bgra);
    let src_img = images::raster_from_data(&info, data, (cap_w * 4) as usize)?;

    // Frosted glass: heavy blur (sigma ~40).
    let scaled_sigma = (blur_sigma * 1.5) / downscale as f32;
    let mut blur_surface = surfaces::raster_n32_premul(ISize::new(cap_w as i32, cap_h as i32))?;
    let blur_canvas = blur_surface.canvas();
    let mut paint = Paint::default();
    if let Some(filter) = image_filters::blur((scaled_sigma, scaled_sigma), None, None, None) {
        paint.set_image_filter(filter);
    }
    blur_canvas.draw_image(&src_img, (0, 0), Some(&paint));
    let blurred = blur_surface.image_snapshot();

    let crop_x = (margin / downscale as i32) as f32;
    let crop_y = (margin / downscale as i32) as f32;
    let crop_w = (w / downscale).max(1) as i32;
    let crop_h = (h / downscale).max(1) as i32;

    let mut final_surface = surfaces::raster_n32_premul(ISize::new(crop_w, crop_h))?;
    let final_canvas = final_surface.canvas();
    final_canvas.draw_image(&blurred, (-crop_x, -crop_y), None);

    Some(final_surface.image_snapshot())
}
