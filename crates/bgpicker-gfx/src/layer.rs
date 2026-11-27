//! Unified GPU layer pipeline shared by `bgpicker:wallpaper` and `bgpicker:backdrop`.
//!
//! Handles A/B `TextureSlot` transitions, smooth `modeBlend` zoom/pan interpolation
//! (eliminating external `awww` surface handoff/z-order races), `OutCubic` parallax
//! scroll smoothing, and optional real-time 3-pass Dual-Kawase blur.

use glow::HasContext;

use crate::blur::{DualKawaseBlur, RenderTarget};
use crate::error::Result;
use crate::frame::{CachedBlur, FrameInput, TextureSlot};
use crate::math::{
    KawaseSettings, TransitionEffect, TransitionSpec, compute_mode_geometry,
    ease_in_out_cubic, ease_out_cubic, ease_transition_bezier,
};
use crate::shaders::ShaderPrograms;

/// Role of a rendered layer-shell surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerRole {
    /// Foreground crisp wallpaper/video surface (`bgpicker:wallpaper`).
    Wallpaper,
    /// Blurred overview backdrop surface (`bgpicker:backdrop`).
    Backdrop,
}

impl LayerRole {
    pub const fn namespace(self) -> &'static str {
        match self {
            Self::Wallpaper => "bgpicker:wallpaper",
            Self::Backdrop => "bgpicker:backdrop",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ScalarAnim {
    from: f32,
    to: f32,
    elapsed_s: f32,
    duration_s: f32,
}

impl ScalarAnim {
    fn start(from: f32, to: f32, duration_ms: u32) -> Option<Self> {
        if duration_ms == 0 || (from - to).abs() < 1e-5 {
            None
        } else {
            Some(Self {
                from,
                to,
                elapsed_s: 0.0,
                duration_s: (duration_ms as f32) / 1000.0,
            })
        }
    }

    fn advance(&mut self, dt_s: f32, curve: fn(f32) -> f32) -> (f32, bool) {
        self.elapsed_s += dt_s.max(0.0);
        if self.elapsed_s >= self.duration_s {
            (self.to, true)
        } else {
            let t = curve(self.elapsed_s / self.duration_s);
            (self.from + (self.to - self.from) * t, false)
        }
    }
}

/// Per-output, per-role GPU rendering state machine.
pub struct LayerPipeline {
    pub role: LayerRole,
    slots: [TextureSlot; 2],
    front_index: Option<usize>,
    staged_slot: Option<usize>,

    transition_active: bool,
    transition_spec: TransitionSpec,
    transition_elapsed_s: f32,
    transition_progress: f32,

    target_progress: f32,
    animated_progress: f32,
    progress_anim: Option<ScalarAnim>,

    mode_blend: f32,
    old_mode_blend: f32,
    mode_anim: Option<ScalarAnim>,

    blur: Option<DualKawaseBlur>,
    needs_redraw: bool,
}

impl LayerPipeline {
    pub fn new(role: LayerRole, initial_parallax: bool) -> Self {
        let initial_blend = if initial_parallax { 1.0 } else { 0.0 };
        Self {
            role,
            slots: [TextureSlot::new(), TextureSlot::new()],
            front_index: None,
            staged_slot: None,
            transition_active: false,
            transition_spec: TransitionSpec::instant(),
            transition_elapsed_s: 0.0,
            transition_progress: 1.0,
            target_progress: 0.5,
            animated_progress: 0.5,
            progress_anim: None,
            mode_blend: initial_blend,
            old_mode_blend: initial_blend,
            mode_anim: None,
            blur: (role == LayerRole::Backdrop).then(DualKawaseBlur::new),
            needs_redraw: false,
        }
    }

    #[inline]
    pub fn is_presented(&self) -> bool {
        self.front_index.is_some()
    }

    #[inline]
    pub fn is_transition_active(&self) -> bool {
        self.transition_active
    }

    #[inline]
    pub fn mode_blend(&self) -> f32 {
        self.mode_blend
    }

    #[inline]
    pub fn old_mode_blend(&self) -> f32 {
        self.old_mode_blend
    }

    #[inline]
    pub fn animated_progress(&self) -> f32 {
        self.animated_progress
    }

    #[inline]
    pub fn transition_progress(&self) -> f32 {
        self.transition_progress
    }

    #[inline]
    pub fn needs_redraw(&self) -> bool {
        self.needs_redraw || self.is_animating()
    }

    #[inline]
    pub fn is_animating(&self) -> bool {
        self.transition_active || self.progress_anim.is_some() || self.mode_anim.is_some()
    }

    #[inline]
    pub fn mark_dirty(&mut self) {
        self.needs_redraw = true;
    }

    /// Total number of Dual-Kawase blur recomputations performed by this pipeline.
    #[inline]
    pub fn blur_compute_count(&self) -> u64 {
        self.blur.as_ref().map_or(0, DualKawaseBlur::compute_count)
    }

    /// Stages a new static image or initial video frame into the inactive back slot
    /// without disturbing the currently visible front slot.
    pub fn stage_frame(
        &mut self,
        gl: &glow::Context,
        frame: &FrameInput,
        bgra_ext_supported: bool,
    ) -> Result<()> {
        if self.transition_active {
            self.finish_transition(gl);
        }
        let back_idx = match self.front_index {
            Some(f) => 1 - f,
            None => 0,
        };
        self.slots[back_idx].upload(gl, frame, bgra_ext_supported)?;
        self.staged_slot = Some(back_idx);
        Ok(())
    }

    /// Commits the previously staged slot as the new front slot, atomically applying an
    /// optional target parallax mode for the incoming frame while preserving the outgoing
    /// frame's current `mode_blend` for the duration of the transition.
    pub fn commit_staged_with_mode(
        &mut self,
        gl: &glow::Context,
        spec: TransitionSpec,
        next_parallax: Option<bool>,
    ) -> bool {
        let Some(next_idx) = self.staged_slot.take() else {
            return false;
        };
        if !self.slots[next_idx].is_ready() {
            return false;
        }
        let had_previous = self.front_index.is_some();
        let prev_blend = self.mode_blend;
        if let Some(parallax) = next_parallax {
            self.mode_anim = None;
            self.mode_blend = if parallax { 1.0 } else { 0.0 };
        }
        self.front_index = Some(next_idx);
        let spec = spec.clamped();
        self.transition_spec = spec;
        if had_previous && spec.effect != TransitionEffect::None && spec.duration_ms > 0 {
            self.old_mode_blend = prev_blend;
            self.transition_active = true;
            self.transition_elapsed_s = 0.0;
            self.transition_progress = 0.0;
        } else {
            self.old_mode_blend = self.mode_blend;
            self.finish_transition(gl);
        }
        self.needs_redraw = true;
        true
    }

    /// Commits the previously staged slot as the new front slot and begins `spec`
    /// if a previous texture was already presented.
    pub fn commit_staged(&mut self, gl: &glow::Context, spec: TransitionSpec) -> bool {
        self.commit_staged_with_mode(gl, spec, None)
    }

    /// Convenience helper that stages and immediately commits `frame` with `spec`
    /// and an optional atomic target parallax mode.
    pub fn apply_frame_with_mode(
        &mut self,
        gl: &glow::Context,
        frame: &FrameInput,
        spec: TransitionSpec,
        next_parallax: Option<bool>,
        bgra_ext_supported: bool,
    ) -> Result<()> {
        self.stage_frame(gl, frame, bgra_ext_supported)?;
        self.commit_staged_with_mode(gl, spec, next_parallax);
        Ok(())
    }

    /// Convenience helper that stages and immediately commits `frame` with `spec`.
    pub fn apply_frame(
        &mut self,
        gl: &glow::Context,
        frame: &FrameInput,
        spec: TransitionSpec,
        bgra_ext_supported: bool,
    ) -> Result<()> {
        self.apply_frame_with_mode(gl, frame, spec, None, bgra_ext_supported)
    }

    /// Streams a live video frame into whichever active slot matches `frame.media_key`
    /// (front slot, staged slot, or old slot during a transition). If no slot is active yet,
    /// presents it immediately.
    pub fn push_video_frame(
        &mut self,
        gl: &glow::Context,
        frame: &FrameInput,
        bgra_ext_supported: bool,
    ) -> Result<bool> {
        let target_idx = if let Some(staged) = self.staged_slot
            && self.slots[staged].media_key == frame.media_key
        {
            Some(staged)
        } else if let Some(front) = self.front_index {
            let back = 1 - front;
            if self.slots[front].media_key == frame.media_key {
                Some(front)
            } else if self.transition_active && self.slots[back].media_key == frame.media_key {
                Some(back)
            } else {
                None
            }
        } else {
            None
        };

        let Some(idx) = target_idx else {
            self.apply_frame(gl, frame, TransitionSpec::instant(), bgra_ext_supported)?;
            return Ok(true);
        };

        let uploaded = self.slots[idx].upload(gl, frame, bgra_ext_supported)?;
        if uploaded {
            self.needs_redraw = true;
        }
        Ok(uploaded)
    }

    /// Cancels a pending staged frame (if any) and releases its slot texture.
    pub fn cancel_staged(&mut self, gl: &glow::Context) {
        if let Some(idx) = self.staged_slot.take()
            && Some(idx) != self.front_index
        {
            self.slots[idx].release(gl);
        }
    }

    /// Smoothly animates `modeBlend` between `0.0` (normal centered cover) and `1.0`
    /// (zoomed overscan + parallax pan) inside the same surface with zero z-order handoff.
    pub fn set_mode(&mut self, parallax: bool, duration_ms: u32) {
        let target = if parallax { 1.0 } else { 0.0 };
        if let Some(anim) = ScalarAnim::start(self.mode_blend, target, duration_ms) {
            self.mode_anim = Some(anim);
        } else {
            self.mode_anim = None;
            self.mode_blend = target;
            if !self.transition_active {
                self.old_mode_blend = target;
            }
        }
        self.needs_redraw = true;
    }

    /// Sets the target horizontal parallax progress `[0.0, 1.0]`, smoothly animating
    /// with `OutCubic` over `duration_ms`.
    pub fn set_progress(&mut self, progress: f32, duration_ms: u32) {
        let target = if progress.is_finite() {
            progress.clamp(0.0, 1.0)
        } else {
            0.5
        };
        if (target - self.target_progress).abs() < 1e-5 && self.progress_anim.is_none() {
            return;
        }
        self.target_progress = target;
        if let Some(anim) = ScalarAnim::start(self.animated_progress, target, duration_ms) {
            self.progress_anim = Some(anim);
        } else {
            self.progress_anim = None;
            self.animated_progress = target;
        }
        self.needs_redraw = true;
    }

    /// Advances active animations by `dt_s` seconds and returns `true` if the surface
    /// should render a frame now.
    pub fn tick(&mut self, gl: &glow::Context, dt_s: f32) -> bool {
        let dt = dt_s.clamp(0.0, 0.25);

        if self.transition_active {
            let dur_s = (self.transition_spec.duration_ms as f32) / 1000.0;
            self.transition_elapsed_s += dt;
            if dur_s <= 0.0 || self.transition_elapsed_s >= dur_s {
                self.finish_transition(gl);
            } else {
                let raw = (self.transition_elapsed_s / dur_s).clamp(0.0, 1.0);
                self.transition_progress = ease_transition_bezier(raw);
            }
            self.needs_redraw = true;
        }

        if let Some(mut anim) = self.progress_anim {
            let (val, done) = anim.advance(dt, ease_out_cubic);
            self.animated_progress = val;
            self.progress_anim = if done { None } else { Some(anim) };
            self.needs_redraw = true;
        }

        if let Some(mut anim) = self.mode_anim {
            let (val, done) = anim.advance(dt, ease_in_out_cubic);
            self.mode_blend = val;
            if !self.transition_active {
                self.old_mode_blend = val;
            }
            self.mode_anim = if done { None } else { Some(anim) };
            self.needs_redraw = true;
        }

        self.needs_redraw
    }

    /// Renders this layer into `target_fbo` (`None` = default window backbuffer).
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        gl: &glow::Context,
        programs: &ShaderPrograms,
        target_fbo: Option<glow::Framebuffer>,
        viewport_w: u32,
        viewport_h: u32,
        zoom: f32,
        kawase: KawaseSettings,
    ) -> Result<()> {
        let w = viewport_w.max(1);
        let h = viewport_h.max(1);

        let Some(front_idx) = self.front_index else {
            unsafe {
                gl.bind_framebuffer(glow::FRAMEBUFFER, target_fbo);
                gl.viewport(0, 0, w as i32, h as i32);
                gl.clear_color(0.0, 0.0, 0.0, 0.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
            }
            self.needs_redraw = false;
            return Ok(());
        };

        let back_idx = 1 - front_idx;
        let has_old = self.transition_active && self.slots[back_idx].is_ready();

        if let Some(ref mut blur) = self.blur {
            ensure_slot_blur(gl, programs, blur, &mut self.slots[front_idx], kawase)?;
            if has_old {
                ensure_slot_blur(gl, programs, blur, &mut self.slots[back_idx], kawase)?;
            }
        }

        let old_slot = if has_old {
            &self.slots[back_idx]
        } else {
            &self.slots[front_idx]
        };
        let new_slot = &self.slots[front_idx];

        let effect_kind = if self.transition_active && has_old {
            self.transition_spec.effect.shader_kind()
        } else {
            TransitionEffect::None.shader_kind()
        };
        let t_progress = if self.transition_active && has_old {
            self.transition_progress
        } else {
            1.0
        };
        let spec = self.transition_spec;
        let animated_progress = self.animated_progress;
        let old_mode_blend = if has_old {
            self.old_mode_blend
        } else {
            self.mode_blend
        };
        let new_mode_blend = self.mode_blend;
        let use_cached_blur = self.blur.is_some();

        let fw = w as f32;
        let fh = h as f32;
        let old_geom = compute_mode_geometry(
            fw,
            fh,
            old_slot.width as f32,
            old_slot.height as f32,
            zoom,
            animated_progress,
            old_mode_blend,
        );
        let new_geom = compute_mode_geometry(
            fw,
            fh,
            new_slot.width as f32,
            new_slot.height as f32,
            zoom,
            animated_progress,
            new_mode_blend,
        );
        let dir = spec.direction_vec();
        let p = &programs.transition;

        let (old_mode, old_src, old_uv_src) = slot_render_binding(old_slot, use_cached_blur);
        let (new_mode, new_src, new_uv_src) = slot_render_binding(new_slot, use_cached_blur);

        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, target_fbo);
            gl.viewport(0, 0, w as i32, h as i32);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::BLEND);
            gl.use_program(Some(p.program));
            gl.bind_vertex_array(Some(programs.empty_vao));

            gl.uniform_1_f32(p.u_transition_progress.as_ref(), t_progress);
            gl.uniform_1_i32(p.u_effect_kind.as_ref(), effect_kind);
            gl.uniform_2_f32(p.u_resolution.as_ref(), fw, fh);
            let or = old_geom.as_rect_uniform();
            gl.uniform_4_f32(p.u_old_rect.as_ref(), or[0], or[1], or[2], or[3]);
            let nr = new_geom.as_rect_uniform();
            gl.uniform_4_f32(p.u_new_rect.as_ref(), nr[0], nr[1], nr[2], nr[3]);
            gl.uniform_2_f32(p.u_origin.as_ref(), spec.position[0], spec.position[1]);
            gl.uniform_2_f32(p.u_direction.as_ref(), dir[0], dir[1]);
            gl.uniform_2_f32(p.u_wave.as_ref(), spec.wave[0], spec.wave[1]);
            gl.uniform_1_f32(p.u_opacity.as_ref(), 1.0);

            gl.uniform_1_i32(p.u_old_mode.as_ref(), old_mode);
            gl.uniform_1_i32(p.u_new_mode.as_ref(), new_mode);

            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, old_src);
            gl.uniform_1_i32(p.u_old_source.as_ref(), 0);

            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, old_uv_src);
            gl.uniform_1_i32(p.u_old_uv_source.as_ref(), 1);

            gl.active_texture(glow::TEXTURE2);
            gl.bind_texture(glow::TEXTURE_2D, new_src);
            gl.uniform_1_i32(p.u_new_source.as_ref(), 2);

            gl.active_texture(glow::TEXTURE3);
            gl.bind_texture(glow::TEXTURE_2D, new_uv_src);
            gl.uniform_1_i32(p.u_new_uv_source.as_ref(), 3);

            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }

        self.needs_redraw = false;
        Ok(())
    }

    pub fn finish_transition(&mut self, gl: &glow::Context) {
        self.transition_active = false;
        self.transition_progress = 1.0;
        self.transition_elapsed_s = 0.0;
        self.old_mode_blend = self.mode_blend;
        if let Some(front) = self.front_index {
            let old = 1 - front;
            if self.staged_slot != Some(old) {
                self.slots[old].release(gl);
            }
        }
    }

    pub fn release_all(&mut self, gl: &glow::Context) {
        self.transition_active = false;
        self.old_mode_blend = self.mode_blend;
        self.staged_slot = None;
        self.front_index = None;
        self.slots[0].release(gl);
        self.slots[1].release(gl);
        if let Some(ref mut blur) = self.blur {
            blur.destroy(gl);
        }
        self.needs_redraw = true;
    }

    /// Releases idle scratch resources (inactive slot and intermediate Dual-Kawase
    /// FBO chain once cached blur is ready) without disturbing the active image.
    pub fn trim_idle(&mut self, gl: &glow::Context) {
        if self.transition_active || self.staged_slot.is_some() {
            return;
        }
        let Some(front_idx) = self.front_index else {
            return;
        };
        let idle_idx = 1 - front_idx;
        self.slots[idle_idx].release(gl);

        if self.slots[front_idx].cached_blur.is_some()
            && let Some(ref mut blur) = self.blur
        {
            blur.destroy(gl);
        }
    }
}

#[inline]
fn slot_shader_mode(slot: &TextureSlot) -> i32 {
    if slot.is_nv12() {
        2
    } else if slot.needs_bgra_swizzle() {
        1
    } else {
        0
    }
}

#[inline]
fn slot_render_binding(
    slot: &TextureSlot,
    use_cached_blur: bool,
) -> (i32, Option<glow::Texture>, Option<glow::Texture>) {
    if use_cached_blur && let Some(blur_tex) = slot.blurred_tex() {
        return (0, Some(blur_tex), Some(blur_tex));
    }
    (
        slot_shader_mode(slot),
        slot.primary_tex,
        slot.uv_tex.or(slot.primary_tex),
    )
}

fn ensure_slot_blur(
    gl: &glow::Context,
    programs: &ShaderPrograms,
    blur: &mut DualKawaseBlur,
    slot: &mut TextureSlot,
    kawase: KawaseSettings,
) -> Result<()> {
    if !slot.needs_blur_recompute(kawase) {
        return Ok(());
    }
    let (blur_w, blur_h) = DualKawaseBlur::capped_image_size(slot.width, slot.height);
    let target = match slot.cached_blur.take() {
        Some(existing) if existing.target.width == blur_w && existing.target.height == blur_h => {
            existing.target
        }
        Some(existing) => {
            existing.target.destroy(gl);
            RenderTarget::new(gl, blur_w, blur_h)?
        }
        None => RenderTarget::new(gl, blur_w, blur_h)?,
    };

    let res = blur.render(
        gl,
        programs,
        Some(target.fbo),
        blur_w,
        blur_h,
        1.0,
        kawase,
        |s0_fbo, s0_w, s0_h| {
            draw_slot_blit(gl, programs, slot, s0_fbo, s0_w, s0_h);
        },
    );

    if let Err(err) = res {
        target.destroy(gl);
        return Err(err);
    }

    slot.cached_blur = Some(CachedBlur {
        target,
        media_key: slot.media_key.clone(),
        generation: slot.uploaded_generation.unwrap_or(0),
        kawase,
    });
    slot.blur_dirty = false;
    Ok(())
}

fn draw_slot_blit(
    gl: &glow::Context,
    programs: &ShaderPrograms,
    slot: &TextureSlot,
    dst_fbo: Option<glow::Framebuffer>,
    dst_w: u32,
    dst_h: u32,
) {
    let p = &programs.slot_blit;
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, dst_fbo);
        gl.viewport(0, 0, dst_w as i32, dst_h as i32);
        gl.disable(glow::DEPTH_TEST);
        gl.disable(glow::BLEND);
        gl.use_program(Some(p.program));
        gl.bind_vertex_array(Some(programs.empty_vao));

        gl.uniform_1_i32(p.u_mode.as_ref(), slot_shader_mode(slot));

        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, slot.primary_tex);
        gl.uniform_1_i32(p.u_source.as_ref(), 0);

        gl.active_texture(glow::TEXTURE1);
        gl.bind_texture(glow::TEXTURE_2D, slot.uv_tex.or(slot.primary_tex));
        gl.uniform_1_i32(p.u_uv_source.as_ref(), 1);

        gl.draw_arrays(glow::TRIANGLES, 0, 3);

        gl.bind_texture(glow::TEXTURE_2D, None);
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, None);
    }
}
