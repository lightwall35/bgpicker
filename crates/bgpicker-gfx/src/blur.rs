//! Real-time 3-pass Dual-Kawase blur FBO chain matching `DualKawaseBlur.qml`.

use glow::HasContext;

use crate::error::{GfxError, Result};
use crate::math::KawaseSettings;
use crate::shaders::ShaderPrograms;

#[derive(Debug)]
pub(crate) struct RenderTarget {
    pub(crate) fbo: glow::Framebuffer,
    pub(crate) tex: glow::Texture,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl RenderTarget {
    pub(crate) fn new(gl: &glow::Context, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let tex = gl.create_texture().map_err(|e| GfxError::Gl {
                context: "kawase create_texture",
                detail: e,
            })?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                width as i32,
                height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.bind_texture(glow::TEXTURE_2D, None);

            let fbo = gl.create_framebuffer().map_err(|e| GfxError::Gl {
                context: "kawase create_framebuffer",
                detail: e,
            })?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(tex),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            if status != glow::FRAMEBUFFER_COMPLETE {
                gl.delete_framebuffer(fbo);
                gl.delete_texture(tex);
                return Err(GfxError::Gl {
                    context: "kawase framebuffer",
                    detail: format!("incomplete framebuffer status 0x{status:x}"),
                });
            }

            Ok(Self {
                fbo,
                tex,
                width,
                height,
            })
        }
    }

    pub(crate) fn destroy(self, gl: &glow::Context) {
        unsafe {
            gl.delete_framebuffer(self.fbo);
            gl.delete_texture(self.tex);
        }
    }
}

/// Manages the multi-resolution FBO pyramid for `bgpicker:backdrop` real-time Dual-Kawase blur.
#[derive(Debug, Default)]
pub struct DualKawaseBlur {
    out_width: u32,
    out_height: u32,
    s0: Option<RenderTarget>,
    down: [Option<RenderTarget>; 3],
    up: [Option<RenderTarget>; 3],
    compute_count: u64,
}

impl DualKawaseBlur {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn compute_count(&self) -> u64 {
        self.compute_count
    }

    /// Computes capped image-space blur target dimensions `(w, h)` preserving aspect ratio,
    /// fitting within `1920x1080` (so `s0` half-res base fits within `960x540` and `>= 64x64`).
    pub fn capped_image_size(src_w: u32, src_h: u32) -> (u32, u32) {
        let w = src_w.max(1) as f32;
        let h = src_h.max(1) as f32;
        let scale = (1920.0 / w).min(1080.0 / h).min(1.0);
        let out_w = (w * scale).round().max(64.0) as u32;
        let out_h = (h * scale).round().max(64.0) as u32;
        (out_w, out_h)
    }

    /// Computes pyramid dimensions `(base, level1, level2, level3)` matching `DualKawaseBlur.qml`.
    pub fn pyramid_sizes(width: u32, height: u32) -> [(u32, u32); 4] {
        let w = width.max(1) as f32;
        let h = height.max(1) as f32;
        let base_w = (w * 0.5).ceil().max(64.0) as u32;
        let base_h = (h * 0.5).ceil().max(64.0) as u32;
        let w1 = ((base_w as f32) * 0.5).ceil().max(32.0) as u32;
        let h1 = ((base_h as f32) * 0.5).ceil().max(32.0) as u32;
        let w2 = ((w1 as f32) * 0.5).ceil().max(16.0) as u32;
        let h2 = ((h1 as f32) * 0.5).ceil().max(16.0) as u32;
        let w3 = ((w2 as f32) * 0.5).ceil().max(8.0) as u32;
        let h3 = ((h2 as f32) * 0.5).ceil().max(8.0) as u32;
        [(base_w, base_h), (w1, h1), (w2, h2), (w3, h3)]
    }

    pub fn ensure_size(&mut self, gl: &glow::Context, width: u32, height: u32) -> Result<()> {
        let w = width.max(1);
        let h = height.max(1);
        if self.out_width == w && self.out_height == h && self.s0.is_some() {
            return Ok(());
        }
        self.destroy(gl);

        let [(base_w, base_h), (w1, h1), (w2, h2), (w3, h3)] = Self::pyramid_sizes(w, h);
        self.s0 = Some(RenderTarget::new(gl, base_w, base_h)?);
        self.down = [
            Some(RenderTarget::new(gl, w1, h1)?),
            Some(RenderTarget::new(gl, w2, h2)?),
            Some(RenderTarget::new(gl, w3, h3)?),
        ];
        // up[0] = su1 (w2, h2), up[1] = su2 (w1, h1), up[2] = su3 (base_w, base_h)
        self.up = [
            Some(RenderTarget::new(gl, w2, h2)?),
            Some(RenderTarget::new(gl, w1, h1)?),
            Some(RenderTarget::new(gl, base_w, base_h)?),
        ];
        self.out_width = w;
        self.out_height = h;
        Ok(())
    }

    /// Executes the source pass into `s0` (half-res), runs `passes` downsample + upsample
    /// Kawase passes, and writes the final saturated/brightness-adjusted output into `target_fbo`.
    #[allow(clippy::too_many_arguments)]
    pub fn render<F>(
        &mut self,
        gl: &glow::Context,
        programs: &ShaderPrograms,
        target_fbo: Option<glow::Framebuffer>,
        width: u32,
        height: u32,
        opacity: f32,
        settings: KawaseSettings,
        render_source: F,
    ) -> Result<()>
    where
        F: FnOnce(Option<glow::Framebuffer>, u32, u32),
    {
        self.ensure_size(gl, width, height)?;
        let passes = settings.passes.min(3) as usize;

        let s0 = self.s0.as_ref().unwrap();
        // Step 1: Render wallpaper/transition source into half-resolution s0.
        render_source(Some(s0.fbo), s0.width, s0.height);

        // Step 2: Downsample chain s0 -> d0 -> d1 -> d2
        for i in 0..passes {
            let src = if i == 0 {
                s0
            } else {
                self.down[i - 1].as_ref().unwrap()
            };
            let dst = self.down[i].as_ref().unwrap();
            draw_kawase_down(gl, programs, src, dst, settings.offset, 1.0);
        }

        // Step 3: Upsample chain matching DualKawaseBlur.qml:
        // passes == 3: d2 -> up[0](w2,h2) -> up[1](w1,h1) -> up[2](base) -> final(out)
        // passes == 2: d1 -> up[1](w1,h1) -> up[2](base) -> final(out)
        // passes == 1: d0 -> up[2](base) -> final(out)
        let final_src: &RenderTarget = if passes == 0 {
            s0
        } else {
            if passes >= 3 {
                let src = self.down[2].as_ref().unwrap();
                let dst = self.up[0].as_ref().unwrap();
                draw_kawase_up(gl, programs, src, Some(dst.fbo), dst.width, dst.height, settings.offset, 1.0, 0.0, 1.0);
            }
            if passes >= 2 {
                let src = if passes >= 3 {
                    self.up[0].as_ref().unwrap()
                } else {
                    self.down[1].as_ref().unwrap()
                };
                let dst = self.up[1].as_ref().unwrap();
                draw_kawase_up(gl, programs, src, Some(dst.fbo), dst.width, dst.height, settings.offset, 1.0, 0.0, 1.0);
            }
            let src = if passes >= 2 {
                self.up[1].as_ref().unwrap()
            } else {
                self.down[0].as_ref().unwrap()
            };
            let dst = self.up[2].as_ref().unwrap();
            draw_kawase_up(gl, programs, src, Some(dst.fbo), dst.width, dst.height, settings.offset, 1.0, 0.0, 1.0);
            self.up[2].as_ref().unwrap()
        };

        // Step 4: Final upsample pass into target framebuffer with saturation & brightness.
        draw_kawase_up(
            gl,
            programs,
            final_src,
            target_fbo,
            width.max(1),
            height.max(1),
            settings.offset,
            settings.saturation,
            settings.brightness,
            opacity,
        );

        self.compute_count += 1;
        Ok(())
    }

    pub fn destroy(&mut self, gl: &glow::Context) {
        if let Some(rt) = self.s0.take() {
            rt.destroy(gl);
        }
        for slot in &mut self.down {
            if let Some(rt) = slot.take() {
                rt.destroy(gl);
            }
        }
        for slot in &mut self.up {
            if let Some(rt) = slot.take() {
                rt.destroy(gl);
            }
        }
        self.out_width = 0;
        self.out_height = 0;
    }
}

fn draw_kawase_down(
    gl: &glow::Context,
    programs: &ShaderPrograms,
    src: &RenderTarget,
    dst: &RenderTarget,
    offset: f32,
    opacity: f32,
) {
    let prog = &programs.kawase_down;
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(dst.fbo));
        gl.viewport(0, 0, dst.width as i32, dst.height as i32);
        gl.use_program(Some(prog.program));
        gl.bind_vertex_array(Some(programs.empty_vao));

        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(src.tex));
        gl.uniform_1_i32(prog.u_source.as_ref(), 0);
        gl.uniform_2_f32(
            prog.u_halfpixel.as_ref(),
            0.5 / (src.width as f32),
            0.5 / (src.height as f32),
        );
        gl.uniform_1_f32(prog.u_offset.as_ref(), offset);
        gl.uniform_1_f32(prog.u_opacity.as_ref(), opacity);

        gl.draw_arrays(glow::TRIANGLES, 0, 3);
        gl.bind_texture(glow::TEXTURE_2D, None);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_kawase_up(
    gl: &glow::Context,
    programs: &ShaderPrograms,
    src: &RenderTarget,
    dst_fbo: Option<glow::Framebuffer>,
    dst_w: u32,
    dst_h: u32,
    offset: f32,
    saturation: f32,
    brightness: f32,
    opacity: f32,
) {
    let prog = &programs.kawase_up;
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, dst_fbo);
        gl.viewport(0, 0, dst_w as i32, dst_h as i32);
        gl.use_program(Some(prog.program));
        gl.bind_vertex_array(Some(programs.empty_vao));

        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(src.tex));
        gl.uniform_1_i32(prog.u_source.as_ref(), 0);
        gl.uniform_2_f32(
            prog.u_halfpixel.as_ref(),
            0.5 / (src.width as f32),
            0.5 / (src.height as f32),
        );
        gl.uniform_1_f32(prog.u_offset.as_ref(), offset);
        gl.uniform_1_f32(prog.u_saturation.as_ref(), saturation);
        gl.uniform_1_f32(prog.u_brightness.as_ref(), brightness);
        gl.uniform_1_f32(prog.u_opacity.as_ref(), opacity);

        gl.draw_arrays(glow::TRIANGLES, 0, 3);
        gl.bind_texture(glow::TEXTURE_2D, None);
    }
}
