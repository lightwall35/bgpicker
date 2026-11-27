//! Dynamic HiDPI CJK + Latin single-channel (`R8`) glyph atlas backed by `swash`.
//!
//! Uses read-only memory-mapped font files (`MAP_PRIVATE`) and lazy on-demand glyph
//! rasterization via `swash`, loading at most **1 Latin font + 1 CJK font** so startup
//! takes only a few milliseconds and open memory overhead stays minimal even for large
//! OpenType CFF `.ttc` collections such as `NotoSansCJK-Regular.ttc`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use glow::HasContext;
use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::Format;
use swash::{CacheKey, FontRef};

const INITIAL_ATLAS_SIZE: u32 = 1024;
const MAX_ATLAS_SIZE: u32 = 2048;
const GLYPH_PAD: u32 = 1;

/// Positioned glyph quad in logical pixels relative to the text run's top-left `(0, 0)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphQuad {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
}

/// Measured and positioned glyph run in logical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLayout {
    pub width: f32,
    pub height: f32,
    pub ascent: f32,
    pub quads: Vec<GlyphQuad>,
}

enum FontBytes {
    Mmap { ptr: *mut libc::c_void, len: usize },
    Owned(Vec<u8>),
}

// Safety: `FontBytes::Mmap` maps an immutable read-only (`PROT_READ`, `MAP_PRIVATE`)
// byte slice that is never mutated and unmapped strictly on Drop.
unsafe impl Send for FontBytes {}
unsafe impl Sync for FontBytes {}

impl FontBytes {
    fn open(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len() as usize;
        if len == 0 {
            return Ok(Self::Owned(Vec::new()));
        }
        // Prefer read-only MAP_PRIVATE mmap so the kernel only faults in the tiny
        // subset of pages needed for cmap + the ~25 rendered glyphs.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr != libc::MAP_FAILED && !ptr.is_null() {
            Ok(Self::Mmap { ptr, len })
        } else {
            Ok(Self::Owned(fs::read(path)?))
        }
    }

    #[inline]
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Mmap { ptr, len } => unsafe {
                std::slice::from_raw_parts((*ptr).cast::<u8>(), *len)
            },
            Self::Owned(v) => v.as_slice(),
        }
    }
}

impl Drop for FontBytes {
    fn drop(&mut self) {
        if let Self::Mmap { ptr, len } = *self
            && len > 0
            && !ptr.is_null()
        {
            unsafe {
                let _ = libc::munmap(ptr, len);
            }
        }
    }
}

struct LoadedFont {
    data: FontBytes,
    offset: u32,
    key: CacheKey,
}

impl LoadedFont {
    fn load(path: &Path, collection_index: usize) -> Option<Self> {
        let data = FontBytes::open(path).ok()?;
        let font_ref = FontRef::from_index(data.as_slice(), collection_index)
            .or_else(|| FontRef::from_index(data.as_slice(), 0))?;
        let offset = font_ref.offset;
        let key = font_ref.key;
        Some(Self { data, offset, key })
    }

    #[inline]
    fn as_ref(&self) -> FontRef<'_> {
        FontRef {
            data: self.data.as_slice(),
            offset: self.offset,
            key: self.key,
        }
    }

    #[inline]
    fn lookup_glyph(&self, ch: char) -> u16 {
        self.as_ref().charmap().map(ch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    ch: char,
    phys_px: u16,
}

#[derive(Debug, Clone, Copy)]
struct CachedGlyph {
    atlas_x: u32,
    atlas_y: u32,
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    advance_width: f32,
    has_bitmap: bool,
}

#[derive(Debug, Clone, Copy)]
struct DirtyRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Dynamic single-channel (`R8`) glyph atlas supporting Latin + CJK fallback fonts.
pub struct TextAtlas {
    fonts: Vec<LoadedFont>,
    scale_ctx: ScaleContext,
    atlas_w: u32,
    atlas_h: u32,
    pixels: Vec<u8>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
    glyphs: HashMap<GlyphKey, CachedGlyph>,
    dirty_rects: Vec<DirtyRect>,
    needs_full_upload: bool,
    gpu_texture: Option<glow::Texture>,
}

impl Default for TextAtlas {
    fn default() -> Self {
        Self::new_system()
    }
}

impl TextAtlas {
    /// Creates a [`TextAtlas`] using at most 1 Latin font + 1 CJK font auto-discovered
    /// from the system without eager full-font parsing.
    pub fn new_system() -> Self {
        let fonts = load_system_fonts();
        Self::from_loaded_fonts(fonts)
    }

    fn from_loaded_fonts(fonts: Vec<LoadedFont>) -> Self {
        let size = INITIAL_ATLAS_SIZE;
        Self {
            fonts,
            scale_ctx: ScaleContext::new(),
            atlas_w: size,
            atlas_h: size,
            pixels: vec![0u8; (size * size) as usize],
            shelf_x: GLYPH_PAD,
            shelf_y: GLYPH_PAD,
            shelf_h: 0,
            glyphs: HashMap::new(),
            dirty_rects: Vec::new(),
            needs_full_upload: true,
            gpu_texture: None,
        }
    }

    #[inline]
    pub fn font_count(&self) -> usize {
        self.fonts.len()
    }

    #[inline]
    pub fn cached_glyph_count(&self) -> usize {
        self.glyphs.len()
    }

    #[inline]
    pub fn atlas_size(&self) -> (u32, u32) {
        (self.atlas_w, self.atlas_h)
    }

    /// Returns `true` if any loaded font can rasterize `ch` (i.e. non-zero glyph ID).
    pub fn supports_char(&self, ch: char) -> bool {
        self.fonts.iter().any(|f| f.lookup_glyph(ch) != 0)
    }

    /// Measures logical `(width, height)` of single-line `text`.
    pub fn measure_text(
        &mut self,
        text: &str,
        logical_px: f32,
        dpr: f32,
        letter_spacing: f32,
    ) -> (f32, f32) {
        let layout = self.layout_text(text, logical_px, dpr, letter_spacing);
        (layout.width, layout.height)
    }

    /// Lays out single-line `text` at physical pixel size `(logical_px * dpr)`, returning
    /// logical glyph quads relative to top-left `(0, 0)`.
    pub fn layout_text(
        &mut self,
        text: &str,
        logical_px: f32,
        dpr: f32,
        letter_spacing: f32,
    ) -> TextLayout {
        let dpr = dpr.clamp(1.0, 4.0);
        let phys_px_f = (logical_px * dpr).round().clamp(8.0, 128.0);
        let phys_px = phys_px_f as u16;
        let inv_dpr = 1.0 / dpr;

        let (ascent_phys, line_h_phys) = self.primary_line_metrics(phys_px_f);
        let ascent = ascent_phys * inv_dpr;
        let height = line_h_phys * inv_dpr;

        if text.is_empty() || self.fonts.is_empty() {
            return TextLayout {
                width: 0.0,
                height,
                ascent,
                quads: Vec::new(),
            };
        }

        // Rasterize / cache all characters up front so atlas growth cannot invalidate UVs mid-run.
        for ch in text.chars() {
            let _ = self.ensure_glyph(ch, phys_px);
        }

        let inv_w = 1.0 / (self.atlas_w as f32);
        let inv_h = 1.0 / (self.atlas_h as f32);
        let spacing_phys = letter_spacing * dpr;

        let mut pen_x_phys = 0.0_f32;
        let char_count = text.chars().count();
        let mut quads = Vec::with_capacity(char_count);

        for (idx, ch) in text.chars().enumerate() {
            let key = GlyphKey { ch, phys_px };
            let Some(&glyph) = self.glyphs.get(&key) else {
                continue;
            };
            if glyph.has_bitmap && glyph.width > 0 && glyph.height > 0 {
                // swash placement: `left` is +right from pen X, `top` is +up from baseline to glyph top.
                let gx_phys = pen_x_phys + glyph.left as f32;
                let gy_phys = ascent_phys - glyph.top as f32;
                let u0 = glyph.atlas_x as f32 * inv_w;
                let v0 = glyph.atlas_y as f32 * inv_h;
                let u1 = (glyph.atlas_x + glyph.width) as f32 * inv_w;
                let v1 = (glyph.atlas_y + glyph.height) as f32 * inv_h;
                quads.push(GlyphQuad {
                    x: gx_phys * inv_dpr,
                    y: gy_phys * inv_dpr,
                    width: glyph.width as f32 * inv_dpr,
                    height: glyph.height as f32 * inv_dpr,
                    uv_min: [u0, v0],
                    uv_max: [u1, v1],
                });
            }
            pen_x_phys += glyph.advance_width;
            if idx + 1 < char_count {
                pen_x_phys += spacing_phys;
            }
        }

        TextLayout {
            width: pen_x_phys * inv_dpr,
            height,
            ascent,
            quads,
        }
    }

    /// Uploads any newly rasterized glyphs to the GPU `R8` texture and returns its handle.
    pub fn sync_gpu(&mut self, gl: &glow::Context) -> Option<glow::Texture> {
        if self.gpu_texture.is_none() {
            match unsafe { gl.create_texture() } {
                Ok(tex) => {
                    self.gpu_texture = Some(tex);
                    self.needs_full_upload = true;
                }
                Err(err) => {
                    log::error!("bgpicker-picker: failed to create text atlas texture: {err}");
                    return None;
                }
            }
        }
        let tex = self.gpu_texture?;

        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);

            if self.needs_full_upload {
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MAG_FILTER,
                    glow::LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_S,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_T,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::R8 as i32,
                    self.atlas_w as i32,
                    self.atlas_h as i32,
                    0,
                    glow::RED,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&self.pixels)),
                );
                self.needs_full_upload = false;
                self.dirty_rects.clear();
            } else if !self.dirty_rects.is_empty() {
                let mut scratch = Vec::new();
                for rect in self.dirty_rects.drain(..) {
                    let rw = rect.width as usize;
                    let rh = rect.height as usize;
                    scratch.clear();
                    scratch.reserve(rw * rh);
                    for row in 0..rh {
                        let start = ((rect.y as usize + row) * self.atlas_w as usize)
                            + rect.x as usize;
                        scratch.extend_from_slice(&self.pixels[start..start + rw]);
                    }
                    gl.tex_sub_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        rect.x as i32,
                        rect.y as i32,
                        rect.width as i32,
                        rect.height as i32,
                        glow::RED,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(&scratch)),
                    );
                }
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
        }

        Some(tex)
    }

    /// Returns a read-only view of the CPU-side `R8` atlas bitmap (useful for verification/tests).
    #[inline]
    pub fn atlas_pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Deletes the GPU atlas texture cleanly.
    pub fn destroy_gpu(&mut self, gl: &glow::Context) {
        if let Some(tex) = self.gpu_texture.take() {
            unsafe {
                gl.delete_texture(tex);
            }
        }
        self.needs_full_upload = true;
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn primary_line_metrics(&self, phys_px: f32) -> (f32, f32) {
        if let Some(font) = self.fonts.first() {
            let m = font.as_ref().metrics(&[]).scale(phys_px);
            let line_h = (m.ascent + m.descent + m.leading).max(phys_px);
            if m.ascent > 0.0 {
                return (m.ascent, line_h);
            }
        }
        (phys_px * 0.8, phys_px * 1.2)
    }

    fn pick_font_and_glyph(&self, ch: char) -> Option<(usize, u16)> {
        for (i, font) in self.fonts.iter().enumerate() {
            let gid = font.lookup_glyph(ch);
            if gid != 0 {
                return Some((i, gid));
            }
        }
        if !self.fonts.is_empty() {
            Some((0, 0))
        } else {
            None
        }
    }

    fn ensure_glyph(&mut self, ch: char, phys_px: u16) -> Option<CachedGlyph> {
        let key = GlyphKey { ch, phys_px };
        if let Some(&cached) = self.glyphs.get(&key) {
            return Some(cached);
        }
        let (font_idx, gid) = self.pick_font_and_glyph(ch)?;
        let phys_px_f = phys_px as f32;

        let (advance_width, rendered) = {
            let font_ref = self.fonts[font_idx].as_ref();
            let adv = font_ref
                .glyph_metrics(&[])
                .scale(phys_px_f)
                .advance_width(gid);
            let mut scaler = self
                .scale_ctx
                .builder(font_ref)
                .size(phys_px_f)
                .hint(true)
                .build();
            let img = Render::new(&[Source::Outline])
                .format(Format::Alpha)
                .render(&mut scaler, gid);
            (adv, img)
        };

        let Some(image) = rendered else {
            let entry = CachedGlyph {
                atlas_x: 0,
                atlas_y: 0,
                width: 0,
                height: 0,
                left: 0,
                top: 0,
                advance_width,
                has_bitmap: false,
            };
            self.glyphs.insert(key, entry);
            return Some(entry);
        };

        let gw = image.placement.width;
        let gh = image.placement.height;
        if gw == 0 || gh == 0 || image.data.len() < (gw * gh) as usize {
            let entry = CachedGlyph {
                atlas_x: 0,
                atlas_y: 0,
                width: 0,
                height: 0,
                left: image.placement.left,
                top: image.placement.top,
                advance_width,
                has_bitmap: false,
            };
            self.glyphs.insert(key, entry);
            return Some(entry);
        }

        let (ax, ay) = self.allocate_rect(gw, gh)?;
        for row in 0..gh {
            let src_start = (row * gw) as usize;
            let dst_start = ((ay + row) * self.atlas_w + ax) as usize;
            self.pixels[dst_start..dst_start + gw as usize]
                .copy_from_slice(&image.data[src_start..src_start + gw as usize]);
        }

        if !self.needs_full_upload {
            self.dirty_rects.push(DirtyRect {
                x: ax,
                y: ay,
                width: gw,
                height: gh,
            });
        }

        let entry = CachedGlyph {
            atlas_x: ax,
            atlas_y: ay,
            width: gw,
            height: gh,
            left: image.placement.left,
            top: image.placement.top,
            advance_width,
            has_bitmap: true,
        };
        self.glyphs.insert(key, entry);
        Some(entry)
    }

    fn allocate_rect(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w + 2 * GLYPH_PAD > MAX_ATLAS_SIZE || h + 2 * GLYPH_PAD > MAX_ATLAS_SIZE {
            return None;
        }
        if self.shelf_x + w + GLYPH_PAD > self.atlas_w {
            self.shelf_x = GLYPH_PAD;
            self.shelf_y += self.shelf_h + GLYPH_PAD;
            self.shelf_h = 0;
        }
        if self.shelf_y + h + GLYPH_PAD > self.atlas_h {
            if self.atlas_w < MAX_ATLAS_SIZE {
                self.grow_atlas();
                return self.allocate_rect(w, h);
            }
            // Atlas full at max size: reset shelf and clear glyph cache cleanly.
            self.pixels.fill(0);
            self.glyphs.clear();
            self.dirty_rects.clear();
            self.shelf_x = GLYPH_PAD;
            self.shelf_y = GLYPH_PAD;
            self.shelf_h = 0;
            self.needs_full_upload = true;
        }
        let pos = (self.shelf_x, self.shelf_y);
        self.shelf_x += w + GLYPH_PAD;
        self.shelf_h = self.shelf_h.max(h);
        Some(pos)
    }

    fn grow_atlas(&mut self) {
        let new_w = (self.atlas_w * 2).min(MAX_ATLAS_SIZE);
        let new_h = (self.atlas_h * 2).min(MAX_ATLAS_SIZE);
        let mut next = vec![0u8; (new_w * new_h) as usize];
        for row in 0..self.atlas_h {
            let old_start = (row * self.atlas_w) as usize;
            let new_start = (row * new_w) as usize;
            next[new_start..new_start + self.atlas_w as usize]
                .copy_from_slice(&self.pixels[old_start..old_start + self.atlas_w as usize]);
        }
        self.pixels = next;
        self.atlas_w = new_w;
        self.atlas_h = new_h;
        self.needs_full_upload = true;
        self.dirty_rects.clear();
    }
}

/// Loads strictly at most **1 Latin font + 1 CJK font** so startup stays fast and memory minimal.
fn load_system_fonts() -> Vec<LoadedFont> {
    let mut fonts: Vec<LoadedFont> = Vec::with_capacity(2);
    let mut latin_path: Option<PathBuf> = None;

    // 1. Discover and load at most ONE Latin/UI sans-serif font.
    if let Some((path, idx)) = find_first_latin_font()
        && let Some(loaded) = LoadedFont::load(&path, idx as usize)
    {
        latin_path = Some(path);
        fonts.push(loaded);
    }

    // 2. If the primary font already covers Simplified Chinese glyphs, no second font is needed.
    let cjk_covered = fonts.iter().any(|f| f.lookup_glyph('淡') != 0 && f.lookup_glyph('视') != 0);
    if !cjk_covered
        && let Some((path, idx)) = find_first_cjk_font(latin_path.as_deref())
        && let Some(loaded) = LoadedFont::load(&path, idx as usize)
    {
        fonts.push(loaded);
    }

    fonts
}

fn find_first_latin_font() -> Option<(PathBuf, u32)> {
    if let Some((path, idx)) = run_fc_match("sans-serif")
        && path.is_file()
    {
        return Some((path, idx));
    }
    const LATIN_CANDIDATES: &[(&str, u32)] = &[
        ("/usr/share/fonts/noto/NotoSans-Regular.ttf", 0),
        ("/usr/share/fonts/TTF/DejaVuSans.ttf", 0),
        ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0),
        ("/usr/share/fonts/liberation/LiberationSans-Regular.ttf", 0),
    ];
    for &(path_str, idx) in LATIN_CANDIDATES {
        let p = PathBuf::from(path_str);
        if p.is_file() {
            return Some((p, idx));
        }
    }
    if let Ok(entries) = fs::read_dir(Path::new("/usr/share/fonts/TTF")) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("ttf") && p.is_file() {
                return Some((p, 0));
            }
        }
    }
    None
}

fn find_first_cjk_font(exclude_path: Option<&Path>) -> Option<(PathBuf, u32)> {
    for query in ["Noto Sans CJK SC", "sans-serif:lang=zh-cn"] {
        if let Some((path, idx)) = run_fc_match(query)
            && path.is_file()
            && exclude_path != Some(path.as_path())
        {
            return Some((path, idx));
        }
    }
    const CJK_CANDIDATES: &[(&str, u32)] = &[
        ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2),
        ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2),
        ("/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc", 0),
    ];
    for &(path_str, idx) in CJK_CANDIDATES {
        let p = PathBuf::from(path_str);
        if p.is_file() && exclude_path != Some(p.as_path()) {
            return Some((p, idx));
        }
    }
    None
}

fn run_fc_match(query: &str) -> Option<(PathBuf, u32)> {
    let output = Command::new("fc-match")
        .args([query, "--format=%{file}\t%{index}\n"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    let mut parts = line.split('\t');
    let file = parts.next()?.trim();
    if file.is_empty() {
        return None;
    }
    let idx = parts
        .next()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    Some((PathBuf::from(file), idx))
}
