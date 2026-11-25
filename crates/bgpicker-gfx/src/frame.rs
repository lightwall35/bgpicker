//! Unified `FrameInput` and GPU `TextureSlot` abstractions so static images and
//! async video frames drive the exact same layer pipeline.

use std::path::Path;
use std::sync::Arc;

use glow::HasContext;
use image::imageops::FilterType;

use crate::blur::{DualKawaseBlur, RenderTarget};
use crate::error::{GfxError, Result};
use crate::math::KawaseSettings;

/// Pixel layout of a [`FrameInput`] payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// Tightly packed or strided 8-bit RGBA (`[R, G, B, A]`).
    Rgba8,
    /// Tightly packed or strided 8-bit BGRA (`[B, G, R, A]`), e.g. from VAAPI/FFmpeg or Cairo/shm.
    Bgra8,
    /// Semi-planar 4:2:0 YUV (`Y` plane followed by interleaved `UV` plane).
    Nv12 { y_stride: u32, uv_stride: u32 },
}

/// CPU-side frame payload shared cheaply via `Arc<[u8]>` between foreground
/// (`bgpicker:wallpaper`) and backdrop (`bgpicker:backdrop`) layers without copying.
#[derive(Debug, Clone)]
pub struct FrameInput {
    /// Logical media identifier (typically canonical file path).
    pub media_key: String,
    /// Monotonic frame counter (`0` for static images, incrementing per decoded video frame).
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    /// Row stride in bytes for packed formats (`Rgba8`/`Bgra8`). Ignored for `Nv12`.
    pub stride_bytes: u32,
    pub format: PixelFormat,
    pub data: Arc<[u8]>,
}

impl FrameInput {
    /// Constructs a tightly packed or strided RGBA8 frame.
    pub fn from_rgba(
        media_key: impl Into<String>,
        generation: u64,
        width: u32,
        height: u32,
        stride_bytes: Option<u32>,
        data: impl Into<Arc<[u8]>>,
    ) -> Result<Self> {
        Self::new_packed(media_key, generation, width, height, stride_bytes, PixelFormat::Rgba8, data)
    }

    /// Constructs a tightly packed or strided BGRA8 frame.
    pub fn from_bgra(
        media_key: impl Into<String>,
        generation: u64,
        width: u32,
        height: u32,
        stride_bytes: Option<u32>,
        data: impl Into<Arc<[u8]>>,
    ) -> Result<Self> {
        Self::new_packed(media_key, generation, width, height, stride_bytes, PixelFormat::Bgra8, data)
    }

    /// Constructs a semi-planar NV12 frame (`Y` plane of `y_stride * height`, followed by
    /// `UV` plane of `uv_stride * ceil(height / 2)`).
    pub fn from_nv12(
        media_key: impl Into<String>,
        generation: u64,
        width: u32,
        height: u32,
        y_stride: u32,
        uv_stride: u32,
        data: impl Into<Arc<[u8]>>,
    ) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(GfxError::InvalidFrame("frame dimensions must be non-zero".into()));
        }
        let uv_h = height.div_ceil(2);
        let uv_w = width.div_ceil(2) * 2;
        if y_stride < width || uv_stride < uv_w {
            return Err(GfxError::InvalidFrame(format!(
                "NV12 strides ({y_stride}, {uv_stride}) smaller than plane widths ({width}, {uv_w})"
            )));
        }
        let needed = (y_stride as usize)
            .saturating_mul(height as usize)
            .saturating_add((uv_stride as usize).saturating_mul(uv_h as usize));
        let data = data.into();
        if data.len() < needed {
            return Err(GfxError::InvalidFrame(format!(
                "NV12 buffer too small: got {} bytes, need at least {needed}",
                data.len()
            )));
        }
        Ok(Self {
            media_key: media_key.into(),
            generation,
            width,
            height,
            stride_bytes: y_stride,
            format: PixelFormat::Nv12 { y_stride, uv_stride },
            data,
        })
    }

    fn new_packed(
        media_key: impl Into<String>,
        generation: u64,
        width: u32,
        height: u32,
        stride_bytes: Option<u32>,
        format: PixelFormat,
        data: impl Into<Arc<[u8]>>,
    ) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(GfxError::InvalidFrame("frame dimensions must be non-zero".into()));
        }
        let min_stride = width
            .checked_mul(4)
            .ok_or_else(|| GfxError::InvalidFrame("width overflow".into()))?;
        let stride = stride_bytes.unwrap_or(min_stride);
        if stride < min_stride || !stride.is_multiple_of(4) {
            return Err(GfxError::InvalidFrame(format!(
                "invalid packed stride {stride} for width {width}"
            )));
        }
        let needed = (stride as usize).saturating_mul(height as usize);
        let data = data.into();
        if data.len() < needed {
            return Err(GfxError::InvalidFrame(format!(
                "pixel buffer too small: got {} bytes, need at least {needed}",
                data.len()
            )));
        }
        Ok(Self {
            media_key: media_key.into(),
            generation,
            width,
            height,
            stride_bytes: stride,
            format,
            data,
        })
    }

    /// Decodes a static image file from disk into an RGBA8 [`FrameInput`], bounding
    /// either axis by `max_texture_size` while preserving aspect ratio.
    pub fn from_image_path(path: impl AsRef<Path>, max_texture_size: u32) -> Result<Self> {
        let path_ref = path.as_ref();
        let path_str = path_ref.to_string_lossy().into_owned();
        let img = image::open(path_ref).map_err(|e| GfxError::ImageDecode {
            path: path_str.clone(),
            detail: e.to_string(),
        })?;

        let max_dim = max_texture_size.max(64);
        let img = if img.width() > max_dim || img.height() > max_dim {
            img.resize(max_dim, max_dim, FilterType::Lanczos3)
        } else {
            img
        };

        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        let raw: Arc<[u8]> = Arc::from(rgba.into_raw().into_boxed_slice());
        Self::from_rgba(path_str, 0, width, height, None, raw)
    }
}

/// Kind of GPU texture binding held by a [`TextureSlot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlotKind {
    #[default]
    Empty,
    Rgba,
    BgraSwizzled,
    Nv12,
}

/// Cached image-space Dual-Kawase blur target for a [`TextureSlot`].
#[derive(Debug)]
pub(crate) struct CachedBlur {
    pub(crate) target: RenderTarget,
    pub(crate) media_key: String,
    pub(crate) generation: u64,
    pub(crate) kawase: KawaseSettings,
}

/// GPU texture slot (Slot A or Slot B) backing one side of a layer transition.
///
/// Reuses underlying GL texture storage across video frames of matching dimensions
/// and skips redundant GPU uploads when `(media_key, generation)` has not changed.
#[derive(Debug, Default)]
pub struct TextureSlot {
    pub media_key: String,
    pub uploaded_generation: Option<u64>,
    pub width: u32,
    pub height: u32,
    pub kind: SlotKind,
    pub primary_tex: Option<glow::Texture>,
    pub uv_tex: Option<glow::Texture>,
    pub(crate) cached_blur: Option<CachedBlur>,
    pub(crate) blur_dirty: bool,
}

impl TextureSlot {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn is_ready(&self) -> bool {
        self.kind != SlotKind::Empty && self.primary_tex.is_some()
    }

    #[inline]
    pub fn is_nv12(&self) -> bool {
        self.kind == SlotKind::Nv12
    }

    #[inline]
    pub fn needs_bgra_swizzle(&self) -> bool {
        self.kind == SlotKind::BgraSwizzled
    }

    #[inline]
    pub(crate) fn blurred_tex(&self) -> Option<glow::Texture> {
        self.cached_blur.as_ref().map(|c| c.target.tex)
    }

    #[inline]
    pub(crate) fn needs_blur_recompute(&self, kawase: KawaseSettings) -> bool {
        if !self.is_ready() {
            return false;
        }
        if self.blur_dirty {
            return true;
        }
        let Some(ref cache) = self.cached_blur else {
            return true;
        };
        let expected_size = DualKawaseBlur::capped_image_size(self.width, self.height);
        cache.media_key != self.media_key
            || Some(cache.generation) != self.uploaded_generation
            || cache.kawase != kawase
            || (cache.target.width, cache.target.height) != expected_size
    }

    /// Uploads `frame` into this GPU slot, reallocating only if dimensions/format change
    /// and skipping upload entirely if `(media_key, generation, width, height)` already match.
    pub fn upload(
        &mut self,
        gl: &glow::Context,
        frame: &FrameInput,
        bgra_ext_supported: bool,
    ) -> Result<bool> {
        let target_kind = match frame.format {
            PixelFormat::Rgba8 => SlotKind::Rgba,
            PixelFormat::Bgra8 => {
                if bgra_ext_supported {
                    SlotKind::Rgba
                } else {
                    SlotKind::BgraSwizzled
                }
            }
            PixelFormat::Nv12 { .. } => SlotKind::Nv12,
        };

        if self.media_key == frame.media_key
            && self.uploaded_generation == Some(frame.generation)
            && self.width == frame.width
            && self.height == frame.height
            && self.kind == target_kind
            && self.primary_tex.is_some()
        {
            return Ok(false);
        }

        let realloc = self.width != frame.width
            || self.height != frame.height
            || self.kind != target_kind
            || self.primary_tex.is_none();

        if realloc {
            self.release(gl);
        }

        unsafe {
            match frame.format {
                PixelFormat::Rgba8 | PixelFormat::Bgra8 => {
                    let tex = match self.primary_tex {
                        Some(t) => t,
                        None => {
                            let t = create_2d_texture(gl)?;
                            self.primary_tex = Some(t);
                            t
                        }
                    };
                    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                    let row_pixels = (frame.stride_bytes / 4) as i32;
                    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
                    gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, row_pixels);

                    let (internal_fmt, upload_fmt) = match frame.format {
                        PixelFormat::Bgra8 if bgra_ext_supported => {
                            (glow::RGBA8 as i32, glow::BGRA)
                        }
                        _ => (glow::RGBA8 as i32, glow::RGBA),
                    };

                    let byte_len = (frame.stride_bytes as usize) * (frame.height as usize);
                    let slice = &frame.data[..byte_len];
                    if realloc {
                        gl.tex_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            internal_fmt,
                            frame.width as i32,
                            frame.height as i32,
                            0,
                            upload_fmt,
                            glow::UNSIGNED_BYTE,
                            glow::PixelUnpackData::Slice(Some(slice)),
                        );
                    } else {
                        gl.tex_sub_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            0,
                            0,
                            frame.width as i32,
                            frame.height as i32,
                            upload_fmt,
                            glow::UNSIGNED_BYTE,
                            glow::PixelUnpackData::Slice(Some(slice)),
                        );
                    }
                    gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
                    gl.bind_texture(glow::TEXTURE_2D, None);
                }
                PixelFormat::Nv12 { y_stride, uv_stride } => {
                    let y_tex = match self.primary_tex {
                        Some(t) => t,
                        None => {
                            let t = create_2d_texture(gl)?;
                            self.primary_tex = Some(t);
                            t
                        }
                    };
                    let uv_tex = match self.uv_tex {
                        Some(t) => t,
                        None => {
                            let t = create_2d_texture(gl)?;
                            self.uv_tex = Some(t);
                            t
                        }
                    };

                    let y_bytes = (y_stride as usize) * (frame.height as usize);
                    let uv_w = frame.width.div_ceil(2);
                    let uv_h = frame.height.div_ceil(2);
                    let uv_bytes = (uv_stride as usize) * (uv_h as usize);

                    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);

                    // Y plane (R8)
                    gl.bind_texture(glow::TEXTURE_2D, Some(y_tex));
                    gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, y_stride as i32);
                    let y_slice = &frame.data[..y_bytes];
                    if realloc {
                        gl.tex_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            glow::R8 as i32,
                            frame.width as i32,
                            frame.height as i32,
                            0,
                            glow::RED,
                            glow::UNSIGNED_BYTE,
                            glow::PixelUnpackData::Slice(Some(y_slice)),
                        );
                    } else {
                        gl.tex_sub_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            0,
                            0,
                            frame.width as i32,
                            frame.height as i32,
                            glow::RED,
                            glow::UNSIGNED_BYTE,
                            glow::PixelUnpackData::Slice(Some(y_slice)),
                        );
                    }

                    // UV plane (RG8)
                    gl.bind_texture(glow::TEXTURE_2D, Some(uv_tex));
                    gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, (uv_stride / 2) as i32);
                    let uv_slice = &frame.data[y_bytes..y_bytes + uv_bytes];
                    if realloc {
                        gl.tex_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            glow::RG8 as i32,
                            uv_w as i32,
                            uv_h as i32,
                            0,
                            glow::RG,
                            glow::UNSIGNED_BYTE,
                            glow::PixelUnpackData::Slice(Some(uv_slice)),
                        );
                    } else {
                        gl.tex_sub_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            0,
                            0,
                            uv_w as i32,
                            uv_h as i32,
                            glow::RG,
                            glow::UNSIGNED_BYTE,
                            glow::PixelUnpackData::Slice(Some(uv_slice)),
                        );
                    }

                    gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
                    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
                    gl.bind_texture(glow::TEXTURE_2D, None);
                }
            }
        }

        self.media_key.clear();
        self.media_key.push_str(&frame.media_key);
        self.uploaded_generation = Some(frame.generation);
        self.width = frame.width;
        self.height = frame.height;
        self.kind = target_kind;
        self.blur_dirty = true;
        Ok(true)
    }

    /// Releases GPU textures owned by this slot.
    pub fn release(&mut self, gl: &glow::Context) {
        unsafe {
            if let Some(t) = self.primary_tex.take() {
                gl.delete_texture(t);
            }
            if let Some(t) = self.uv_tex.take() {
                gl.delete_texture(t);
            }
        }
        if let Some(blur) = self.cached_blur.take() {
            blur.target.destroy(gl);
        }
        self.blur_dirty = false;
        self.media_key.clear();
        self.uploaded_generation = None;
        self.width = 0;
        self.height = 0;
        self.kind = SlotKind::Empty;
    }
}

unsafe fn create_2d_texture(gl: &glow::Context) -> Result<glow::Texture> {
    let tex = unsafe {
        gl.create_texture().map_err(|e| GfxError::Gl {
            context: "create_texture",
            detail: e,
        })?
    };
    unsafe {
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        gl.bind_texture(glow::TEXTURE_2D, None);
    }
    Ok(tex)
}
