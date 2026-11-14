//! Screen capture + JPEG encode for the vision path of `char-rig-watcher.exe`.
//!
//! GDI `BitBlt` + `GetDIBits` on purpose: unlike DXGI desktop duplication it has
//! no session/permission handshake, works over RDP and in windowed sessions, and
//! the whole thing is one synchronous blit of the rectangle we actually want
//! (no full-desktop read + crop).
//!
//! Everything here is pure except [`capture`]: the geometry helpers are
//! unit-tested without touching a single pixel, and the capture path copies the
//! BGRA bitmap into an `image::RgbaImage`, downscales it so the longest edge is
//! `max_dim`, and encodes JPEG **in memory** — nothing is ever written to disk.

use base64::Engine as _;
use image::RgbaImage;
use serde::{Deserialize, Serialize};

/// Downscale target for the longest edge (vision tokens scale with pixels).
pub const DEFAULT_MAX_IMAGE_DIM: u32 = 1024;
/// Shipped JPEG quality.
pub const DEFAULT_JPEG_QUALITY: u8 = 85;
/// A capture smaller than this on either edge is treated as "no rect".
pub const MIN_CAPTURE_EDGE: u32 = 16;

/// A screen rectangle in physical pixels (the same space as `/context`'s
/// `window_rect`, which `lib.rs` already converts from the DWM frame rect).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    /// Right/bottom edges, saturating so a huge monitor cannot overflow.
    pub fn right(&self) -> i32 {
        self.x.saturating_add(self.width as i32)
    }

    pub fn bottom(&self) -> i32 {
        self.y.saturating_add(self.height as i32)
    }

    /// Big enough to be worth sending to a model?
    pub fn is_usable(&self) -> bool {
        self.width >= MIN_CAPTURE_EDGE && self.height >= MIN_CAPTURE_EDGE
    }
}

/// One captured frame, already encoded.
#[derive(Debug, Clone)]
pub struct Captured {
    /// Size actually sent (after downscaling).
    pub width: u32,
    pub height: u32,
    /// JPEG bytes.
    pub jpeg: Vec<u8>,
    /// `data:image/jpeg;base64,…` — what the multimodal payload carries.
    pub data_url: String,
    /// Human description of what was captured (`"window"` / `"screen"`).
    pub source: &'static str,
}

/// Clamp `rect` into `bounds` (both physical px) and reject degenerate results.
///
/// A window can hang off the left/top edge or be larger than the desktop; the
/// blit source coordinates must stay inside the virtual screen or GDI silently
/// returns black.
pub fn clamp_rect(rect: Rect, bounds: Rect) -> Option<Rect> {
    if !bounds.is_usable() {
        return None;
    }
    let left = rect.x.max(bounds.x);
    let top = rect.y.max(bounds.y);
    let right = rect.right().min(bounds.right());
    let bottom = rect.bottom().min(bounds.bottom());
    if right <= left || bottom <= top {
        return None;
    }
    let clamped = Rect::new(
        left,
        top,
        (right - left) as u32,
        (bottom - top) as u32,
    );
    clamped.is_usable().then_some(clamped)
}

/// Longest edge fits in `max_dim`, aspect ratio preserved (never upscales).
pub fn fit_dimensions(width: u32, height: u32, max_dim: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    let max_dim = max_dim.max(1);
    let longest = width.max(height);
    if longest <= max_dim {
        return (width, height);
    }
    let scale = max_dim as f64 / longest as f64;
    let scaled_width = ((width as f64 * scale).round() as u32).max(1);
    let scaled_height = ((height as f64 * scale).round() as u32).max(1);
    (scaled_width.min(max_dim), scaled_height.min(max_dim))
}

/// Virtual desktop rectangle in physical px (`SM_*VIRTUALSCREEN`).
#[cfg(windows)]
pub fn virtual_screen() -> Rect {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    // SAFETY: GetSystemMetrics is a pure query.
    unsafe {
        Rect::new(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(0) as u32,
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(0) as u32,
        )
    }
}

#[cfg(not(windows))]
pub fn virtual_screen() -> Rect {
    Rect::default()
}

/// Capture `rect` (or the whole virtual desktop when `None`) and encode it.
pub fn capture(rect: Option<Rect>, max_dim: u32, quality: u8) -> Result<Captured, String> {
    let bounds = virtual_screen();
    let (target, source) = match rect.and_then(|rect| clamp_rect(rect, bounds)) {
        Some(rect) => (rect, "window"),
        None => (bounds, "screen"),
    };
    if !target.is_usable() {
        return Err("the virtual screen has no usable area".to_string());
    }
    let rgba = grab_bgra(target)?;
    encode(rgba, target.width, target.height, max_dim, quality, source)
}

/// BGRA pixels (top-down) → downscaled, JPEG-encoded [`Captured`].
fn encode(
    bgra: Vec<u8>,
    width: u32,
    height: u32,
    max_dim: u32,
    quality: u8,
    source: &'static str,
) -> Result<Captured, String> {
    let mut rgba = bgra;
    // GDI hands back BGRA on little-endian Windows; `image` wants RGBA.
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let image = RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "the captured buffer has the wrong length".to_string())?;
    let (target_width, target_height) = fit_dimensions(width, height, max_dim);
    let image = if (target_width, target_height) == (width, height) {
        image
    } else {
        image::imageops::resize(
            &image,
            target_width,
            target_height,
            image::imageops::FilterType::Triangle,
        )
    };

    let mut jpeg = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
        &mut jpeg,
        quality.clamp(1, 100),
    );
    encoder
        .encode_image(&image)
        .map_err(|err| format!("could not encode the screenshot as JPEG: {err}"))?;
    if jpeg.is_empty() {
        return Err("the JPEG encoder produced no bytes".to_string());
    }
    let data_url = format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&jpeg)
    );
    Ok(Captured { width: target_width, height: target_height, jpeg, data_url, source })
}

/// GDI blit of `rect` into a fresh 32-bit top-down BGRA buffer.
#[cfg(windows)]
fn grab_bgra(rect: Rect) -> Result<Vec<u8>, String> {
    use std::mem::size_of;

    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HDC,
        HGDIOBJ, SRCCOPY,
    };

    let width = rect.width as i32;
    let height = rect.height as i32;
    if width <= 0 || height <= 0 {
        return Err("the capture rectangle is empty".to_string());
    }

    // SAFETY: every handle created here is released on every path below.
    unsafe {
        let screen: HDC = GetDC(None);
        if screen.is_invalid() {
            return Err("GetDC failed (no desktop access?)".to_string());
        }
        let memory = CreateCompatibleDC(screen);
        if memory.is_invalid() {
            ReleaseDC(None, screen);
            return Err("CreateCompatibleDC failed".to_string());
        }
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if bitmap.is_invalid() {
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
            return Err("CreateCompatibleBitmap failed".to_string());
        }
        let previous = SelectObject(memory, HGDIOBJ(bitmap.0));
        let blit = BitBlt(
            memory, 0, 0, width, height, screen, rect.x, rect.y, SRCCOPY,
        );

        let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative height asks for a top-down DIB: rows arrive in screen order.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            ..Default::default()
        };
        let lines = if blit.is_ok() {
            GetDIBits(
                memory,
                bitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr() as *mut core::ffi::c_void),
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        // Always release, whatever happened above.
        let _ = SelectObject(memory, previous);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);

        blit.map_err(|err| format!("BitBlt failed: {err}"))?;
        if lines <= 0 {
            return Err("GetDIBits copied no scan lines".to_string());
        }
        Ok(pixels)
    }
}

#[cfg(not(windows))]
fn grab_bgra(_rect: Rect) -> Result<Vec<u8>, String> {
    Err("screen capture is only implemented on Windows".to_string())
}

/// Parse `/context`'s `window_rect` **and** `screen` into a physical [`Rect`].
///
/// `/context` reports window bounds in CSS pixels relative to the virtual
/// desktop's origin (`screen.x`/`screen.y`, physical), because that is the space
/// the overlay and `POST /perch` use. GDI `BitBlt` needs physical screen pixels,
/// so this converts back: `physical = css * scale + origin`. A left-hand monitor
/// therefore yields negative physical coordinates, which `clamp_rect` then
/// keeps inside the virtual screen.
///
/// An older avatar build has no `screen` object; its rect is used as-is, which
/// is exactly what the previous release did.
pub fn rect_from_context(value: &serde_json::Value) -> Option<Rect> {
    let rect = rect_from_json(value.get("window_rect")?)?;
    let Some(screen) = value.get("screen").filter(|screen| screen.is_object()) else {
        return Some(rect);
    };
    let number = |key: &str| screen.get(key).and_then(serde_json::Value::as_f64);
    let scale = number("scale")
        .filter(|scale| scale.is_finite() && *scale > 0.0)
        .unwrap_or(1.0);
    let origin_x = number("x").unwrap_or(0.0);
    let origin_y = number("y").unwrap_or(0.0);
    if !origin_x.is_finite() || !origin_y.is_finite() {
        return Some(rect);
    }
    let left = (rect.x as f64 * scale + origin_x).round();
    let top = (rect.y as f64 * scale + origin_y).round();
    let width = (rect.width as f64 * scale).round();
    let height = (rect.height as f64 * scale).round();
    if !left.is_finite() || !top.is_finite() || width < 1.0 || height < 1.0 {
        return Some(rect);
    }
    Some(Rect::new(
        left as i32,
        top as i32,
        width as u32,
        height as u32,
    ))
}

/// Parse `/context`'s `window_rect` object into a [`Rect`].
pub fn rect_from_json(value: &serde_json::Value) -> Option<Rect> {
    let number = |key: &str| value.get(key).and_then(serde_json::Value::as_f64);
    let width = number("width")?;
    let height = number("height")?;
    if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some(Rect::new(
        number("x").unwrap_or(0.0).round() as i32,
        number("y").unwrap_or(0.0).round() as i32,
        width.round().max(0.0) as u32,
        height.round().max(0.0) as u32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Rect {
        // A 1920×1080 desktop whose virtual origin is (0,0).
        Rect::new(0, 0, 1920, 1080)
    }

    #[test]
    fn clamp_keeps_an_inside_rect_untouched() {
        let rect = Rect::new(100, 120, 800, 600);
        assert_eq!(clamp_rect(rect, bounds()), Some(rect));
    }

    #[test]
    fn clamp_cuts_a_window_that_hangs_off_the_edges() {
        // Starts off-screen top-left and runs past the bottom-right corner.
        let rect = Rect::new(-200, -50, 1200, 1400);
        let clamped = clamp_rect(rect, bounds()).expect("clamped");
        assert_eq!(clamped, Rect::new(0, 0, 1000, 1080));
    }

    #[test]
    fn clamp_rejects_degenerate_and_offscreen_rects() {
        assert_eq!(clamp_rect(Rect::new(0, 0, 0, 0), bounds()), None);
        assert_eq!(clamp_rect(Rect::new(0, 0, 8, 8), bounds()), None, "below the minimum edge");
        assert_eq!(
            clamp_rect(Rect::new(5000, 5000, 400, 400), bounds()),
            None,
            "entirely off-screen"
        );
        assert_eq!(clamp_rect(Rect::new(0, 0, 800, 600), Rect::default()), None);
    }

    #[test]
    fn clamp_handles_a_negative_virtual_origin() {
        // A monitor to the left of the primary: virtual origin (-1920, 0).
        let bounds = Rect::new(-1920, 0, 3840, 1080);
        let rect = Rect::new(-2000, -20, 500, 400);
        assert_eq!(
            clamp_rect(rect, bounds),
            Some(Rect::new(-1920, 0, 420, 380))
        );
    }

    #[test]
    fn fit_dimensions_preserves_aspect_ratio_and_never_upscales() {
        // 2560×1440 → 1024×576 (same 16:9, longest edge exactly the cap).
        assert_eq!(fit_dimensions(2560, 1440, 1024), (1024, 576));
        // Portrait: the *height* is the longest edge.
        assert_eq!(fit_dimensions(1080, 1920, 1024), (576, 1024));
        // Already small enough → untouched.
        assert_eq!(fit_dimensions(800, 600, 1024), (800, 600));
        // Square stays square.
        assert_eq!(fit_dimensions(2000, 2000, 1024), (1024, 1024));
        // Degenerate input.
        assert_eq!(fit_dimensions(0, 0, 1024), (0, 0));
        assert_eq!(fit_dimensions(1920, 1080, 0), (1, 1), "a zero cap still yields 1×1");
    }

    #[test]
    fn fit_dimensions_result_is_always_inside_the_cap() {
        for (width, height) in [(3840u32, 2160u32), (1000, 4000), (1025, 1), (1024, 1024)] {
            let (w, h) = fit_dimensions(width, height, DEFAULT_MAX_IMAGE_DIM);
            assert!(w <= DEFAULT_MAX_IMAGE_DIM, "{width}x{height} → {w}x{h}");
            assert!(h <= DEFAULT_MAX_IMAGE_DIM, "{width}x{height} → {w}x{h}");
            assert!(w >= 1 && h >= 1);
        }
    }


    #[test]
    fn rect_from_context_converts_css_to_physical_on_a_left_monitor() {
        // The overlay reports CSS px from the virtual origin; this window sits
        // on a monitor to the LEFT of the primary, so its physical x is
        // negative and its CSS x is positive.
        let context = serde_json::json!({
            "window_rect": { "x": 120.0, "y": 100.0, "width": 800.0, "height": 600.0 },
            "screen": { "x": -1920.0, "y": 0.0, "width": 3840.0, "height": 1080.0, "scale": 1.0 }
        });
        assert_eq!(
            rect_from_context(&context),
            Some(Rect::new(-1800, 100, 800, 600))
        );

        // A 150 % display scales back up and offsets the origin.
        let scaled = serde_json::json!({
            "window_rect": { "x": 0.0, "y": 0.0, "width": 800.0, "height": 800.0 },
            "screen": { "x": -1200.0, "y": -300.0, "width": 3000.0, "height": 1800.0, "scale": 1.5 }
        });
        assert_eq!(
            rect_from_context(&scaled),
            Some(Rect::new(-1200, -300, 1200, 1200))
        );
    }

    #[test]
    fn rect_from_context_tolerates_an_older_avatar_without_screen() {
        // Pre-Stage-6 `/context`: no `screen` object, the rect is used as-is.
        let legacy = serde_json::json!({
            "window_rect": { "x": 10.0, "y": 20.0, "width": 300.0, "height": 200.0 }
        });
        assert_eq!(rect_from_context(&legacy), Some(Rect::new(10, 20, 300, 200)));

        // A nonsense scale/origin must not produce garbage: fall back to the
        // rect unchanged rather than panicking or blitting NaN.
        let broken = serde_json::json!({
            "window_rect": { "x": 10.0, "y": 20.0, "width": 300.0, "height": 200.0 },
            "screen": { "x": "left", "scale": 0.0 }
        });
        assert_eq!(rect_from_context(&broken), Some(Rect::new(10, 20, 300, 200)));

        // No rect at all is still `None` (vision falls back to full screen).
        assert_eq!(
            rect_from_context(&serde_json::json!({ "screen": { "scale": 1.0 } })),
            None
        );
    }

    #[test]
    fn rect_from_json_parses_context_shapes() {
        let value = serde_json::json!({ "x": 10.4, "y": 20.6, "width": 800.0, "height": 600.5 });
        assert_eq!(
            rect_from_json(&value),
            Some(Rect::new(10, 21, 800, 601))
        );
        assert_eq!(rect_from_json(&serde_json::json!({ "x": 0, "y": 0 })), None);
        assert_eq!(
            rect_from_json(&serde_json::json!({ "x": 0, "y": 0, "width": 0, "height": 600 })),
            None
        );
        assert_eq!(
            rect_from_json(&serde_json::json!({ "x": 0, "y": 0, "width": 1e9, "height": 1e9 }))
                .is_some(),
            true
        );
    }
}
