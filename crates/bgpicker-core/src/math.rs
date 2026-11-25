//! Wallpaper geometry, parallax blending, and file URI formatting.
//!
//! Mirrors `WallpaperMath.js` so the renderer and tests share identical math.

use percent_encoding::{percent_encode, AsciiSet, NON_ALPHANUMERIC};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Unreserved RFC 3986 path characters plus `/` (matches Python `Path.as_uri()`
/// and `WallpaperMath.fileUrl` for special characters like spaces, `#`, `%`, UTF-8).
const FILE_URI_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~')
    .remove(b'/');

/// Aspect-ratio threshold above which an image is treated as a wide panorama
/// and covers the screen without additional overscan zoom.
pub const PANORAMA_ASPECT_THRESHOLD: f64 = 1.1;

/// Base layout dimensions and horizontal travel room for one zoom level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub width: f64,
    pub height: f64,
    pub overflow_x: f64,
    pub y: f64,
}

/// Interpolated draw rectangle for a wallpaper surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeGeometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Clamps `value` to `[low, high]`. Non-finite values clamp to `low`.
#[inline]
pub fn clamp(value: f64, low: f64, high: f64) -> f64 {
    if !value.is_finite() {
        low
    } else {
        value.clamp(low, high)
    }
}

/// Computes cover-fit geometry and horizontal overscan for a surface and media size.
///
/// Wide panoramas (`aspect > (w / h) * 1.1`) already have horizontal travel room,
/// so `zoom` is forced to `1.0` instead of upscaling.
pub fn geometry(
    width: f64,
    height: f64,
    image_width: f64,
    image_height: f64,
    zoom: f64,
) -> Geometry {
    let w = width.max(1.0);
    let h = height.max(1.0);
    let aspect = if image_width > 0.0 && image_height > 0.0 {
        image_width / image_height
    } else {
        w / h
    };
    let factor = if aspect > (w / h) * PANORAMA_ASPECT_THRESHOLD {
        1.0
    } else {
        zoom
    };
    let canvas_width = w.max(h * aspect) * factor;
    let canvas_height = h.max(w / aspect) * factor;
    Geometry {
        width: canvas_width,
        height: canvas_height,
        overflow_x: (canvas_width - w).max(0.0),
        y: (h - canvas_height) / 2.0,
    }
}

/// Interpolates between centered normal mode (`blend = 0.0`) and active
/// parallax mode (`blend = 1.0`) at horizontal `progress` (`0.0..=1.0`).
pub fn mode_geometry(
    width: f64,
    height: f64,
    image_width: f64,
    image_height: f64,
    zoom: f64,
    progress: f64,
    blend: f64,
) -> ModeGeometry {
    let normal = geometry(width, height, image_width, image_height, 1.0);
    let parallax = geometry(width, height, image_width, image_height, zoom);
    let t = clamp(blend, 0.0, 1.0);
    let p = clamp(progress, 0.0, 1.0);
    let mix = |a: f64, b: f64| a + (b - a) * t;
    ModeGeometry {
        x: mix(-normal.overflow_x * 0.5, -parallax.overflow_x * p),
        y: mix(normal.y, parallax.y),
        width: mix(normal.width, parallax.width),
        height: mix(normal.height, parallax.height),
    }
}

/// Encodes an absolute or relative filesystem path as a `file://` URI.
pub fn file_url<P: AsRef<Path>>(path: P) -> String {
    let bytes = path.as_ref().as_os_str().as_bytes();
    let encoded = percent_encode(bytes, FILE_URI_ENCODE_SET);
    format!("file://{encoded}")
}
