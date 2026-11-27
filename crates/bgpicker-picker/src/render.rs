//! Single-pass OpenGL ES 3.0 / Desktop GL 3.3 Core picker renderer.
//!
//! Renders skewed rounded-rectangle cards with upright counter-skewed `PreserveAspectCrop`
//! thumbnails + hi-res cross-fade in ONE draw call per card (zero `MultiEffect` offscreen FBOs),
//! plus skewed `SkewSelector` controls, vector icons, search bar, empty/error states, and
//! batched HiDPI CJK/Latin text.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glow::HasContext;
use thiserror::Error;

use crate::state::{CardLayout, ModeIcon, Page, PickerUi};
use crate::text::TextAtlas;
use crate::thumbs::ThumbnailCache;

/// Errors returned while compiling shaders or initializing GL state for [`PickerRenderer`].
#[derive(Debug, Error)]
pub enum PickerError {
    #[error("OpenGL error during {context}: {detail}")]
    Gl {
        context: &'static str,
        detail: String,
    },
    #[error("shader error ({stage}): {log}")]
    Shader { stage: &'static str, log: String },
}

// ── GLSL Shaders (compatible with both `#version 300 es` and `#version 330 core`) ──

const QUAD_VERT_BODY: &str = r#"
out vec2 v_local;
out vec2 v_skewed;

uniform vec2 u_viewport;
uniform vec2 u_center;
uniform vec2 u_size;
uniform float u_skew;
uniform float u_pad;

void main() {
    // 2 triangles covering [-1, +1]^2
    vec2 corner;
    if (gl_VertexID == 0) corner = vec2(-1.0, -1.0);
    else if (gl_VertexID == 1) corner = vec2( 1.0, -1.0);
    else if (gl_VertexID == 2) corner = vec2( 1.0,  1.0);
    else if (gl_VertexID == 3) corner = vec2(-1.0, -1.0);
    else if (gl_VertexID == 4) corner = vec2( 1.0,  1.0);
    else corner = vec2(-1.0,  1.0);

    vec2 half_ext = u_size * 0.5 + vec2(u_pad);
    v_local = corner * half_ext;
    v_skewed = vec2(v_local.x + u_skew * v_local.y, v_local.y);

    vec2 screen_pos = u_center + v_skewed;
    vec2 ndc = vec2(
        (screen_pos.x / max(u_viewport.x, 1.0)) * 2.0 - 1.0,
        1.0 - (screen_pos.y / max(u_viewport.y, 1.0)) * 2.0
    );
    gl_Position = vec4(ndc, 0.0, 1.0);
}
"#;

const CARD_FRAG_BODY: &str = r#"
in vec2 v_local;
in vec2 v_skewed;
out vec4 fragColor;

uniform vec2 u_size;
uniform float u_radius;
uniform float u_border_width;
uniform float u_opacity;
uniform float u_skew;
uniform float u_dpr;
uniform int u_has_thumb;
uniform int u_has_hires;
uniform float u_hires_mix;
uniform vec2 u_thumb_size;
uniform vec2 u_hires_size;

uniform sampler2D u_thumb_tex;
uniform sampler2D u_hires_tex;

float sdRoundedBox(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + vec2(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

vec2 coverUV(vec2 upright_pos, vec2 box_size, vec2 tex_size) {
    float box_aspect = box_size.x / max(box_size.y, 1.0);
    float tex_aspect = tex_size.x / max(tex_size.y, 1.0);
    vec2 fitted = box_size;
    if (tex_aspect > box_aspect) {
        fitted.x = box_size.y * tex_aspect;
    } else {
        fitted.y = box_size.x / max(tex_aspect, 0.001);
    }
    return upright_pos / max(fitted, vec2(1.0)) + vec2(0.5);
}

void main() {
    vec2 half_size = u_size * 0.5;
    float r = clamp(u_radius, 0.0, min(half_size.x, half_size.y));
    float d = sdRoundedBox(v_local, half_size, r);
    float aa = max(fwidth(d), 0.5 / max(u_dpr, 1.0));
    float outer_mask = 1.0 - smoothstep(-aa, aa, d);
    if (outer_mask <= 0.001) {
        discard;
    }

    // Upright bounding box of the skewed parallelogram for counter-skewed PreserveAspectCrop.
    vec2 upright_box = vec2(u_size.x + u_size.y * abs(u_skew), u_size.y);

    vec4 col;
    if (u_has_thumb != 0) {
        vec2 uv_thumb = coverUV(v_skewed, upright_box, u_thumb_size);
        vec4 thumb_col = texture(u_thumb_tex, clamp(uv_thumb, 0.0, 1.0));
        col = vec4(thumb_col.rgb, 1.0);
        if (u_has_hires != 0 && u_hires_mix > 0.001) {
            vec2 uv_hires = coverUV(v_skewed, upright_box, u_hires_size);
            vec4 hires_col = texture(u_hires_tex, clamp(uv_hires, 0.0, 1.0));
            col.rgb = mix(col.rgb, hires_col.rgb, clamp(u_hires_mix, 0.0, 1.0));
        }
        if (u_border_width > 0.0) {
            float inner_d = d + u_border_width;
            float border_mask = smoothstep(-aa, aa, inner_d);
            // #33FFFFFF backing border matching Carousel.qml
            col.rgb = mix(col.rgb, vec3(1.0), border_mask * 0.20);
        }
    } else {
        // Sleek translucent placeholder while async thumbnail decodes.
        float rim = smoothstep(-1.5 - aa, -1.5 + aa, d);
        vec3 base_rgb = vec3(0.08, 0.09, 0.11);
        col = vec4(mix(base_rgb, vec3(1.0), rim * 0.18), 0.72);
    }

    float a = clamp(outer_mask * u_opacity * col.a, 0.0, 1.0);
    // Premultiplied alpha output for clean Wayland transparency compositing.
    fragColor = vec4(col.rgb * a, a);
}
"#;

const RECT_FRAG_BODY: &str = r#"
in vec2 v_local;
in vec2 v_skewed;
out vec4 fragColor;

uniform vec2 u_size;
uniform float u_radius;
uniform float u_border_width;
uniform float u_dpr;
uniform vec4 u_fill_color;
uniform vec4 u_border_color;

float sdRoundedBox(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + vec2(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

void main() {
    vec2 half_size = u_size * 0.5;
    float r = clamp(u_radius, 0.0, min(half_size.x, half_size.y));
    float d = sdRoundedBox(v_local, half_size, r);
    float aa = max(fwidth(d), 0.5 / max(u_dpr, 1.0));
    float outer_mask = 1.0 - smoothstep(-aa, aa, d);
    if (outer_mask <= 0.001) {
        discard;
    }

    // Premultiplied fill
    vec4 fill_pm = vec4(u_fill_color.rgb * u_fill_color.a, u_fill_color.a);
    vec4 out_pm = fill_pm;

    if (u_border_width > 0.0 && u_border_color.a > 0.001) {
        float inner_mask = 1.0 - smoothstep(-aa, aa, d + u_border_width);
        float border_cov = clamp(outer_mask - inner_mask, 0.0, 1.0);
        vec4 border_pm = vec4(u_border_color.rgb * u_border_color.a, u_border_color.a);
        // Composite border over fill inside the outer rounded silhouette.
        out_pm = fill_pm * inner_mask + border_pm * border_cov;
        fragColor = out_pm;
        return;
    }

    fragColor = out_pm * outer_mask;
}
"#;

const TEXT_VERT_BODY: &str = r#"
layout(location = 0) in vec2 a_pos;
layout(location = 1) in vec2 a_uv;
layout(location = 2) in vec4 a_color;

out vec2 v_uv;
out vec4 v_color;

uniform vec2 u_viewport;

void main() {
    vec2 ndc = vec2(
        (a_pos.x / max(u_viewport.x, 1.0)) * 2.0 - 1.0,
        1.0 - (a_pos.y / max(u_viewport.y, 1.0)) * 2.0
    );
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_uv = a_uv;
    v_color = a_color;
}
"#;

const TEXT_FRAG_BODY: &str = r#"
in vec2 v_uv;
in vec4 v_color;
out vec4 fragColor;

uniform sampler2D u_atlas;

void main() {
    float cov = texture(u_atlas, v_uv).r;
    float a = v_color.a * cov;
    if (a <= 0.001) {
        discard;
    }
    fragColor = vec4(v_color.rgb * a, a);
}
"#;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct TextVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

struct CardProgram {
    program: glow::Program,
    u_viewport: Option<glow::UniformLocation>,
    u_center: Option<glow::UniformLocation>,
    u_size: Option<glow::UniformLocation>,
    u_skew: Option<glow::UniformLocation>,
    u_pad: Option<glow::UniformLocation>,
    u_radius: Option<glow::UniformLocation>,
    u_border_width: Option<glow::UniformLocation>,
    u_opacity: Option<glow::UniformLocation>,
    u_dpr: Option<glow::UniformLocation>,
    u_has_thumb: Option<glow::UniformLocation>,
    u_has_hires: Option<glow::UniformLocation>,
    u_hires_mix: Option<glow::UniformLocation>,
    u_thumb_size: Option<glow::UniformLocation>,
    u_hires_size: Option<glow::UniformLocation>,
    u_thumb_tex: Option<glow::UniformLocation>,
    u_hires_tex: Option<glow::UniformLocation>,
}

struct RectProgram {
    program: glow::Program,
    u_viewport: Option<glow::UniformLocation>,
    u_center: Option<glow::UniformLocation>,
    u_size: Option<glow::UniformLocation>,
    u_skew: Option<glow::UniformLocation>,
    u_pad: Option<glow::UniformLocation>,
    u_radius: Option<glow::UniformLocation>,
    u_border_width: Option<glow::UniformLocation>,
    u_dpr: Option<glow::UniformLocation>,
    u_fill_color: Option<glow::UniformLocation>,
    u_border_color: Option<glow::UniformLocation>,
}

struct TextProgram {
    program: glow::Program,
    u_viewport: Option<glow::UniformLocation>,
    u_atlas: Option<glow::UniformLocation>,
}

/// Single-pass OpenGL ES 3.0 / GL 3.3 Core renderer for [`PickerUi`].
pub struct PickerRenderer {
    gl: Arc<glow::Context>,
    empty_vao: glow::VertexArray,
    card: CardProgram,
    rect: RectProgram,
    text: TextProgram,
    text_vao: glow::VertexArray,
    text_vbo: glow::Buffer,
    text_vertices: Vec<TextVertex>,
    destroyed: bool,
}

impl PickerRenderer {
    /// Compiles all picker shaders and initializes GL vertex arrays / dynamic text VBO.
    pub fn new(gl: Arc<glow::Context>, is_gles: bool) -> Result<Self, PickerError> {
        let header = if is_gles {
            "#version 300 es\nprecision highp float;\nprecision highp int;\n"
        } else {
            "#version 330 core\n"
        };

        let quad_vs = format!("{header}{QUAD_VERT_BODY}");
        let card_fs = format!("{header}{CARD_FRAG_BODY}");
        let rect_fs = format!("{header}{RECT_FRAG_BODY}");
        let text_vs = format!("{header}{TEXT_VERT_BODY}");
        let text_fs = format!("{header}{TEXT_FRAG_BODY}");

        let card_prog = link_program(&gl, &quad_vs, &card_fs, "picker-card")?;
        let rect_prog = match link_program(&gl, &quad_vs, &rect_fs, "picker-rect") {
            Ok(p) => p,
            Err(e) => {
                unsafe { gl.delete_program(card_prog) };
                return Err(e);
            }
        };
        let text_prog = match link_program(&gl, &text_vs, &text_fs, "picker-text") {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    gl.delete_program(card_prog);
                    gl.delete_program(rect_prog);
                }
                return Err(e);
            }
        };

        unsafe {
            let empty_vao = gl.create_vertex_array().map_err(|detail| PickerError::Gl {
                context: "create_vertex_array(empty)",
                detail,
            })?;
            let text_vao = gl.create_vertex_array().map_err(|detail| PickerError::Gl {
                context: "create_vertex_array(text)",
                detail,
            })?;
            let text_vbo = gl.create_buffer().map_err(|detail| PickerError::Gl {
                context: "create_buffer(text)",
                detail,
            })?;

            gl.bind_vertex_array(Some(text_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(text_vbo));
            let stride = std::mem::size_of::<TextVertex>() as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, stride, 8);
            gl.enable_vertex_attrib_array(2);
            gl.vertex_attrib_pointer_f32(2, 4, glow::FLOAT, false, stride, 16);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_vertex_array(None);

            let card = CardProgram {
                u_viewport: gl.get_uniform_location(card_prog, "u_viewport"),
                u_center: gl.get_uniform_location(card_prog, "u_center"),
                u_size: gl.get_uniform_location(card_prog, "u_size"),
                u_skew: gl.get_uniform_location(card_prog, "u_skew"),
                u_pad: gl.get_uniform_location(card_prog, "u_pad"),
                u_radius: gl.get_uniform_location(card_prog, "u_radius"),
                u_border_width: gl.get_uniform_location(card_prog, "u_border_width"),
                u_opacity: gl.get_uniform_location(card_prog, "u_opacity"),
                u_dpr: gl.get_uniform_location(card_prog, "u_dpr"),
                u_has_thumb: gl.get_uniform_location(card_prog, "u_has_thumb"),
                u_has_hires: gl.get_uniform_location(card_prog, "u_has_hires"),
                u_hires_mix: gl.get_uniform_location(card_prog, "u_hires_mix"),
                u_thumb_size: gl.get_uniform_location(card_prog, "u_thumb_size"),
                u_hires_size: gl.get_uniform_location(card_prog, "u_hires_size"),
                u_thumb_tex: gl.get_uniform_location(card_prog, "u_thumb_tex"),
                u_hires_tex: gl.get_uniform_location(card_prog, "u_hires_tex"),
                program: card_prog,
            };

            let rect = RectProgram {
                u_viewport: gl.get_uniform_location(rect_prog, "u_viewport"),
                u_center: gl.get_uniform_location(rect_prog, "u_center"),
                u_size: gl.get_uniform_location(rect_prog, "u_size"),
                u_skew: gl.get_uniform_location(rect_prog, "u_skew"),
                u_pad: gl.get_uniform_location(rect_prog, "u_pad"),
                u_radius: gl.get_uniform_location(rect_prog, "u_radius"),
                u_border_width: gl.get_uniform_location(rect_prog, "u_border_width"),
                u_dpr: gl.get_uniform_location(rect_prog, "u_dpr"),
                u_fill_color: gl.get_uniform_location(rect_prog, "u_fill_color"),
                u_border_color: gl.get_uniform_location(rect_prog, "u_border_color"),
                program: rect_prog,
            };

            let text = TextProgram {
                u_viewport: gl.get_uniform_location(text_prog, "u_viewport"),
                u_atlas: gl.get_uniform_location(text_prog, "u_atlas"),
                program: text_prog,
            };

            Ok(Self {
                gl,
                empty_vao,
                card,
                rect,
                text,
                text_vao,
                text_vbo,
                text_vertices: Vec::with_capacity(2048),
                destroyed: false,
            })
        }
    }

    #[inline]
    pub fn gl(&self) -> &glow::Context {
        &self.gl
    }

    /// Renders one complete frame of the picker overlay into the currently bound framebuffer.
    ///
    /// - `viewport_px`: physical framebuffer size in pixels `(width, height)`.
    /// - `dpr`: device pixel ratio (e.g. `1.0`, `1.5`, `2.0`).
    ///
    /// Also synchronizes `thumbs` visible window + hi-res texture uploads and notifies `ui`
    /// when a hi-res texture becomes ready.
    pub fn render(
        &mut self,
        ui: &mut PickerUi,
        thumbs: &mut ThumbnailCache,
        text: &mut TextAtlas,
        viewport_px: (u32, u32),
        dpr: f32,
    ) {
        let dpr = dpr.clamp(1.0, 4.0);
        let vw_px = viewport_px.0.max(1);
        let vh_px = viewport_px.1.max(1);
        let vw = vw_px as f32 / dpr;
        let vh = vh_px as f32 / dpr;

        // Sync thumbnail window & hi-res slot before drawing.
        if ui.is_visible() && !ui.is_closing() {
            thumbs.sync_visible_window(ui.active_entries(), ui.current_index());
            let max_hires_dim = ((vw.max(vh) * ui.config().hold_expand_ratio * 2.0 * dpr).ceil()
                as u32)
                .max(1280);
            thumbs.sync_hires(&self.gl, ui.active_hires_path(), max_hires_dim);
            if thumbs.upload_ready(&self.gl, 4) {
                // Keep dirty so freshly decoded thumbnails or hi-res cross-fade render immediately.
            }
            ui.set_hires_ready(thumbs.hires_loaded_path());
        }

        self.text_vertices.clear();

        unsafe {
            self.gl.viewport(0, 0, vw_px as i32, vh_px as i32);
            self.gl.disable(glow::DEPTH_TEST);
            self.gl.disable(glow::CULL_FACE);
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.enable(glow::BLEND);
            self.gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            self.gl.clear_color(0.0, 0.0, 0.0, 0.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }

        let p = ui.panel_progress();
        let panel_active = ui.is_visible() || ui.is_closing() || p > 0.002;
        if !panel_active && ui.scrim_opacity() <= 0.001 {
            ui.clear_dirty();
            return;
        }

        let confirm_fade = ui.confirm_fade();
        let overlay_alpha = ui.overlay_alpha();
        let panel_y = (1.0 - p) * 24.0;
        let panel_scale = 0.92 + 0.08 * p;
        let panel_x_spread = 0.94 + 0.06 * p;
        let panel_card_alpha = p;
        let ctrl_y = (1.0 - p) * 20.0 + (1.0 - confirm_fade) * 16.0;

        // 1. Fullscreen backdrop scrim (#CC000000 * overlayOpacity * overlay_alpha)
        let scrim_alpha = (0xCC as f32 / 255.0) * ui.scrim_opacity();
        if scrim_alpha > 0.001 {
            self.draw_rect(
                (vw, vh),
                (vw * 0.5, vh * 0.5),
                (vw, vh),
                0.0,
                0.0,
                0.0,
                dpr,
                [0.0, 0.0, 0.0, scrim_alpha],
                [0.0, 0.0, 0.0, 0.0],
            );
        }

        // 2. Carousel cards (outgoing page if page-switching, then incoming/active page)
        let page_geom = ui.current_page_geometry();
        let skew = ui.config().skew_factor;
        let corner_radius = ui.config().corner_radius;
        let border_width = ui.config().border_width;

        if panel_card_alpha > 0.002 {
            if ui.is_page_animating() && page_geom.outgoing_opacity > 0.001 {
                let outgoing = ui.outgoing_cards().to_vec();
                for card in &outgoing {
                    self.draw_card(
                        thumbs,
                        (vw, vh),
                        card,
                        page_geom.outgoing_y + panel_y,
                        page_geom.outgoing_opacity * panel_card_alpha,
                        panel_scale,
                        panel_x_spread,
                        skew,
                        corner_radius,
                        border_width,
                        dpr,
                    );
                }
            }

            if page_geom.incoming_opacity > 0.001 {
                let cards = ui.card_layouts();
                for card in &cards {
                    self.draw_card(
                        thumbs,
                        (vw, vh),
                        card,
                        page_geom.incoming_y + panel_y,
                        page_geom.incoming_opacity * panel_card_alpha,
                        panel_scale,
                        panel_x_spread,
                        skew,
                        corner_radius,
                        border_width,
                        dpr,
                    );
                }
            }
        }

        // 3. Empty-folder state message
        if panel_active && overlay_alpha > 0.002 && ui.active_unfiltered_count() == 0 {
            self.queue_empty_folder_text(ui, text, (vw, vh), dpr, ctrl_y, overlay_alpha);
        }

        // 4. Bottom skewed segmented controls (`SkewSelector.qml` parity)
        if panel_active && overlay_alpha > 0.002 {
            self.draw_selectors(ui, text, (vw, vh), dpr, ctrl_y, overlay_alpha);
        }

        // 5. Error banner
        if panel_active && overlay_alpha > 0.002 && !ui.error_message().is_empty() {
            let msg = ui.error_message().to_string();
            let layout = text.layout_text(&msg, 14.0, dpr, 0.0);
            let tx = (vw - layout.width) * 0.5;
            let ty = vh - 36.0 - layout.height + ctrl_y;
            self.push_text_quads(&layout.quads, tx, ty, [1.0, 0.706, 0.671, overlay_alpha]); // #FFB4AB
        }

        // 6. Top search bar (`Carousel.qml::searchBar`)
        if ui.search_progress() > 0.002 && overlay_alpha > 0.002 {
            self.draw_search_bar(ui, text, (vw, vh), dpr, overlay_alpha);
        }

        // 7. Flush all queued text in a single batched draw call
        self.flush_text(text, (vw, vh));

        if !thumbs.has_pending_work() {
            ui.clear_dirty();
        }
    }

    /// Cleanly destroys all OpenGL programs, VAOs, and buffers owned by this renderer.
    pub fn destroy(&mut self) {
        if self.destroyed {
            return;
        }
        self.destroyed = true;
        unsafe {
            self.gl.delete_program(self.card.program);
            self.gl.delete_program(self.rect.program);
            self.gl.delete_program(self.text.program);
            self.gl.delete_vertex_array(self.empty_vao);
            self.gl.delete_vertex_array(self.text_vao);
            self.gl.delete_buffer(self.text_vbo);
        }
    }

    // ── Internal drawing helpers ──────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn draw_card(
        &self,
        thumbs: &mut ThumbnailCache,
        viewport: (f32, f32),
        card: &CardLayout,
        y_offset: f32,
        opacity_mul: f32,
        panel_scale: f32,
        panel_x_spread: f32,
        skew: f32,
        corner_radius: f32,
        border_width: f32,
        dpr: f32,
    ) {
        let opacity = (card.opacity * opacity_mul).clamp(0.0, 1.0);
        if opacity <= 0.002 {
            return;
        }
        let effective_scale = card.scale * panel_scale;
        let scaled_w = card.width * effective_scale;
        let scaled_h = card.height * effective_scale;
        let half_skew_w = (scaled_w + scaled_h * skew.abs()) * 0.5 + 4.0;
        let half_h = scaled_h * 0.5 + 4.0;
        let cx = viewport.0 * 0.5 + (card.center_x - viewport.0 * 0.5) * panel_x_spread;
        let cy = card.center_y + y_offset;

        // Viewport frustum cull: skip completely offscreen cards.
        if cx + half_skew_w < 0.0
            || cx - half_skew_w > viewport.0
            || cy + half_h < 0.0
            || cy - half_h > viewport.1
        {
            return;
        }

        let thumb_key = if !card.thumbnail_path.is_empty() {
            card.thumbnail_path.as_str()
        } else {
            card.original_path.as_str()
        };
        let thumb_tex = thumbs.get_thumbnail(thumb_key);
        let hires_tex = if card.hi_res_opacity > 0.001 && !card.original_path.is_empty() {
            thumbs.get_hires(&card.original_path)
        } else {
            None
        };

        unsafe {
            self.gl.use_program(Some(self.card.program));
            self.gl.bind_vertex_array(Some(self.empty_vao));

            self.gl
                .uniform_2_f32(self.card.u_viewport.as_ref(), viewport.0, viewport.1);
            self.gl.uniform_2_f32(self.card.u_center.as_ref(), cx, cy);
            self.gl
                .uniform_2_f32(self.card.u_size.as_ref(), scaled_w, scaled_h);
            self.gl.uniform_1_f32(self.card.u_skew.as_ref(), skew);
            self.gl.uniform_1_f32(self.card.u_pad.as_ref(), 2.0);
            self.gl.uniform_1_f32(
                self.card.u_radius.as_ref(),
                corner_radius * effective_scale,
            );
            self.gl.uniform_1_f32(
                self.card.u_border_width.as_ref(),
                border_width * effective_scale,
            );
            self.gl.uniform_1_f32(self.card.u_opacity.as_ref(), opacity);
            self.gl.uniform_1_f32(self.card.u_dpr.as_ref(), dpr);

            if let Some(t) = thumb_tex {
                self.gl.active_texture(glow::TEXTURE0);
                self.gl.bind_texture(glow::TEXTURE_2D, Some(t.texture));
                self.gl.uniform_1_i32(self.card.u_thumb_tex.as_ref(), 0);
                self.gl.uniform_1_i32(self.card.u_has_thumb.as_ref(), 1);
                self.gl.uniform_2_f32(
                    self.card.u_thumb_size.as_ref(),
                    t.width as f32,
                    t.height as f32,
                );
            } else {
                self.gl.uniform_1_i32(self.card.u_has_thumb.as_ref(), 0);
                self.gl
                    .uniform_2_f32(self.card.u_thumb_size.as_ref(), 1.0, 1.0);
            }

            if let Some(hr) = hires_tex {
                self.gl.active_texture(glow::TEXTURE1);
                self.gl.bind_texture(glow::TEXTURE_2D, Some(hr.texture));
                self.gl.uniform_1_i32(self.card.u_hires_tex.as_ref(), 1);
                self.gl.uniform_1_i32(self.card.u_has_hires.as_ref(), 1);
                self.gl
                    .uniform_1_f32(self.card.u_hires_mix.as_ref(), card.hi_res_opacity);
                self.gl.uniform_2_f32(
                    self.card.u_hires_size.as_ref(),
                    hr.width as f32,
                    hr.height as f32,
                );
            } else {
                self.gl.uniform_1_i32(self.card.u_has_hires.as_ref(), 0);
                self.gl.uniform_1_f32(self.card.u_hires_mix.as_ref(), 0.0);
                self.gl
                    .uniform_2_f32(self.card.u_hires_size.as_ref(), 1.0, 1.0);
            }

            self.gl.draw_arrays(glow::TRIANGLES, 0, 6);
            self.gl.active_texture(glow::TEXTURE1);
            self.gl.bind_texture(glow::TEXTURE_2D, None);
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, None);
            self.gl.bind_vertex_array(None);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_rect(
        &self,
        viewport: (f32, f32),
        center: (f32, f32),
        size: (f32, f32),
        skew: f32,
        radius: f32,
        border_width: f32,
        dpr: f32,
        fill_color: [f32; 4],
        border_color: [f32; 4],
    ) {
        unsafe {
            self.gl.use_program(Some(self.rect.program));
            self.gl.bind_vertex_array(Some(self.empty_vao));
            self.gl
                .uniform_2_f32(self.rect.u_viewport.as_ref(), viewport.0, viewport.1);
            self.gl
                .uniform_2_f32(self.rect.u_center.as_ref(), center.0, center.1);
            self.gl
                .uniform_2_f32(self.rect.u_size.as_ref(), size.0, size.1);
            self.gl.uniform_1_f32(self.rect.u_skew.as_ref(), skew);
            self.gl.uniform_1_f32(self.rect.u_pad.as_ref(), 2.0);
            self.gl.uniform_1_f32(self.rect.u_radius.as_ref(), radius);
            self.gl
                .uniform_1_f32(self.rect.u_border_width.as_ref(), border_width);
            self.gl.uniform_1_f32(self.rect.u_dpr.as_ref(), dpr);
            self.gl.uniform_4_f32(
                self.rect.u_fill_color.as_ref(),
                fill_color[0],
                fill_color[1],
                fill_color[2],
                fill_color[3],
            );
            self.gl.uniform_4_f32(
                self.rect.u_border_color.as_ref(),
                border_color[0],
                border_color[1],
                border_color[2],
                border_color[3],
            );
            self.gl.draw_arrays(glow::TRIANGLES, 0, 6);
            self.gl.bind_vertex_array(None);
        }
    }

    fn draw_selectors(
        &mut self,
        ui: &PickerUi,
        text: &mut TextAtlas,
        viewport: (f32, f32),
        dpr: f32,
        y_offset: f32,
        overlay_alpha: f32,
    ) {
        let sel = ui.selector_layout();
        let skew = ui.config().skew_factor;

        // ── Mode selector (普通 / 视差) ──
        let mode_op = sel.mode_opacity * overlay_alpha;
        // Sliding highlight pill (#24FFFFFF fill, 1px #32FFFFFF border, radius 5)
        self.draw_rect(
            viewport,
            (sel.mode_pill_center_x, sel.mode_pill_center_y + y_offset),
            (sel.mode_pill_width, sel.mode_pill_height),
            skew,
            5.0,
            1.0,
            dpr,
            [1.0, 1.0, 1.0, (0x24 as f32 / 255.0) * mode_op],
            [1.0, 1.0, 1.0, (0x32 as f32 / 255.0) * mode_op],
        );

        for btn in &sel.mode_buttons {
            let btn_cy = btn.center_y + y_offset;
            // Button translucent backing
            self.draw_rect(
                viewport,
                (btn.center_x, btn_cy),
                (btn.width * btn.scale, btn.height * btn.scale),
                skew,
                5.0 * btn.scale,
                0.0,
                dpr,
                [1.0, 1.0, 1.0, btn.bg_alpha * mode_op],
                [0.0, 0.0, 0.0, 0.0],
            );

            // Icon + label centered horizontally (`Row { spacing: 9 }`, icon `16x14`)
            let content_alpha = btn.content_opacity * mode_op;
            let label_layout = text.layout_text(btn.label, 13.0 * btn.scale, dpr, 1.0);
            let icon_w = 16.0 * btn.scale;
            let spacing = 9.0 * btn.scale;
            let total_w = icon_w + spacing + label_layout.width;
            let start_x = btn.center_x - total_w * 0.5;
            let icon_cx = start_x + icon_w * 0.5;
            let icon_cy = btn_cy;
            let icon_alpha = (0xE8 as f32 / 255.0) * content_alpha;

            match btn.icon {
                ModeIcon::Frame => {
                    // 14x11 hollow rounded rect, radius 1.5, border 1px #E8FFFFFF
                    self.draw_rect(
                        viewport,
                        (icon_cx, icon_cy),
                        (14.0 * btn.scale, 11.0 * btn.scale),
                        0.0,
                        1.5 * btn.scale,
                        1.0 * btn.scale,
                        dpr,
                        [0.0, 0.0, 0.0, 0.0],
                        [1.0, 1.0, 1.0, icon_alpha],
                    );
                }
                ModeIcon::Layers => {
                    // 3 skewed vertical bars (1.5 x 12, spacing 4)
                    let bar_w = 1.5 * btn.scale;
                    let bar_h = 12.0 * btn.scale;
                    let step = (1.5 + 4.0) * btn.scale;
                    for offset_idx in [-1.0_f32, 0.0, 1.0] {
                        self.draw_rect(
                            viewport,
                            (icon_cx + offset_idx * step, icon_cy),
                            (bar_w, bar_h),
                            skew,
                            0.0,
                            0.0,
                            dpr,
                            [1.0, 1.0, 1.0, icon_alpha],
                            [0.0, 0.0, 0.0, 0.0],
                        );
                    }
                }
            }

            let text_x = start_x + icon_w + spacing;
            let text_y = btn_cy - label_layout.height * 0.5;
            // #F5F1EB
            self.push_text_quads(
                &label_layout.quads,
                text_x,
                text_y,
                [0.961, 0.945, 0.922, content_alpha],
            );
        }

        // ── Transition selector (7 choices) ──
        self.draw_rect(
            viewport,
            (
                sel.transition_pill_center_x,
                sel.transition_pill_center_y + y_offset,
            ),
            (sel.transition_pill_width, sel.transition_pill_height),
            skew,
            5.0,
            1.0,
            dpr,
            [1.0, 1.0, 1.0, (0x24 as f32 / 255.0) * overlay_alpha],
            [1.0, 1.0, 1.0, (0x32 as f32 / 255.0) * overlay_alpha],
        );

        for btn in &sel.transition_buttons {
            let btn_cy = btn.center_y + y_offset;
            self.draw_rect(
                viewport,
                (btn.center_x, btn_cy),
                (btn.width * btn.scale, btn.height * btn.scale),
                skew,
                5.0 * btn.scale,
                0.0,
                dpr,
                [1.0, 1.0, 1.0, btn.bg_alpha * overlay_alpha],
                [0.0, 0.0, 0.0, 0.0],
            );

            let label_layout = text.layout_text(btn.label, 13.0 * btn.scale, dpr, 1.0);
            let text_x = btn.center_x - label_layout.width * 0.5;
            let text_y = btn_cy - label_layout.height * 0.5;
            self.push_text_quads(
                &label_layout.quads,
                text_x,
                text_y,
                [0.961, 0.945, 0.922, btn.content_opacity * overlay_alpha],
            );
        }
    }

    fn draw_search_bar(
        &mut self,
        ui: &PickerUi,
        text: &mut TextAtlas,
        viewport: (f32, f32),
        dpr: f32,
        overlay_alpha: f32,
    ) {
        let s = ui.search_progress();
        let alpha = (s * overlay_alpha).clamp(0.0, 1.0);
        if s <= 0.002 || alpha <= 0.002 {
            return;
        }

        let rect = ui.search_box_rect();
        let slide_y = (1.0 - s) * -20.0;
        let scale_x = 0.92 + 0.08 * s;
        let scale_y = 0.88 + 0.12 * s;
        let base_cx = rect.x + rect.width * 0.5;
        let base_cy = rect.y + rect.height * 0.5;
        let cx = base_cx;
        let cy = base_cy + slide_y;

        // #BB000000 fill, 1px #AAFFFFFF border, radius 8 * scale_y
        self.draw_rect(
            viewport,
            (cx, cy),
            (rect.width * scale_x, rect.height * scale_y),
            0.0,
            8.0 * scale_y,
            1.0,
            dpr,
            [0.0, 0.0, 0.0, (0xBB as f32 / 255.0) * alpha],
            [1.0, 1.0, 1.0, (0xAA as f32 / 255.0) * alpha],
        );

        let pad_x = 12.0;
        let query = ui.search_render_text();
        let cursor_x = if query.is_empty() {
            let ph = text.layout_text("Search wallpapers...", 15.0, dpr, 0.0);
            let ty = rect.y + (rect.height - ph.height) * 0.5;
            for q in &ph.quads {
                self.push_single_quad_transformed(
                    *q,
                    rect.x + pad_x,
                    ty,
                    (base_cx, base_cy),
                    (cx, cy),
                    (scale_x, scale_y),
                    [1.0, 1.0, 1.0, 0.33 * alpha],
                );
            }
            rect.x + pad_x
        } else {
            let layout = text.layout_text(query, 15.0, dpr, 0.0);
            let max_text_w = (rect.width - 2.0 * pad_x).max(16.0);
            let scroll_shift = (layout.width - max_text_w).max(0.0);
            let tx = rect.x + pad_x - scroll_shift;
            let ty = rect.y + (rect.height - layout.height) * 0.5;
            // Clip glyphs within search box interior
            let min_x = rect.x + pad_x - 2.0;
            let max_x = rect.x + rect.width - pad_x + 2.0;
            for q in &layout.quads {
                let gx = tx + q.x;
                if gx + q.width >= min_x && gx <= max_x {
                    self.push_single_quad_transformed(
                        *q,
                        tx,
                        ty,
                        (base_cx, base_cy),
                        (cx, cy),
                        (scale_x, scale_y),
                        [1.0, 1.0, 1.0, alpha],
                    );
                }
            }
            (tx + layout.width + 1.0).min(rect.x + rect.width - pad_x)
        };

        if ui.cursor_visible() && ui.search_visible() {
            let scaled_cursor_x = cx + (cursor_x - base_cx) * scale_x;
            self.draw_rect(
                viewport,
                (scaled_cursor_x, cy),
                (1.5 * scale_x, 20.0 * scale_y),
                0.0,
                0.0,
                0.0,
                dpr,
                [1.0, 1.0, 1.0, 0.92 * alpha],
                [0.0, 0.0, 0.0, 0.0],
            );
        }
    }

    fn queue_empty_folder_text(
        &mut self,
        ui: &PickerUi,
        text: &mut TextAtlas,
        viewport: (f32, f32),
        dpr: f32,
        y_offset: f32,
        overlay_alpha: f32,
    ) {
        let title = match ui.page() {
            Page::Video => "动态壁纸文件夹为空".to_string(),
            Page::Static if ui.config().has_wallpaper_configured => {
                "No images found in wallpaper folder".to_string()
            }
            Page::Static => "No wallpaper configured".to_string(),
        };
        let subtitle = match ui.page() {
            Page::Video => format!("将视频放入 {}", ui.config().video_folder),
            Page::Static if ui.config().has_wallpaper_configured => {
                format!("The folder '{}' is empty.", ui.config().wallpaper_folder)
            }
            Page::Static => ui.config().shell_settings_hint.clone(),
        };
        let hint = "Press Escape to close";

        let l_title = text.layout_text(&title, 24.0, dpr, 0.0);
        let l_sub = text.layout_text(&subtitle, 14.0, dpr, 0.0);
        let l_hint = text.layout_text(hint, 12.0, dpr, 0.0);

        let total_h = l_title.height + 12.0 + l_sub.height + 12.0 + l_hint.height;
        let mut y = (viewport.1 - total_h) * 0.5 + y_offset;

        self.push_text_quads(
            &l_title.quads,
            (viewport.0 - l_title.width) * 0.5,
            y,
            [1.0, 1.0, 1.0, overlay_alpha],
        );
        y += l_title.height + 12.0;

        self.push_text_quads(
            &l_sub.quads,
            (viewport.0 - l_sub.width) * 0.5,
            y,
            [0.733, 0.733, 0.733, overlay_alpha],
        );
        y += l_sub.height + 12.0;

        self.push_text_quads(
            &l_hint.quads,
            (viewport.0 - l_hint.width) * 0.5,
            y,
            [0.533, 0.533, 0.533, overlay_alpha],
        );
    }

    fn push_text_quads(
        &mut self,
        quads: &[crate::text::GlyphQuad],
        origin_x: f32,
        origin_y: f32,
        color: [f32; 4],
    ) {
        for &q in quads {
            self.push_single_quad(q, origin_x, origin_y, color);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_single_quad_transformed(
        &mut self,
        q: crate::text::GlyphQuad,
        origin_x: f32,
        origin_y: f32,
        pivot_src: (f32, f32),
        pivot_dst: (f32, f32),
        scale: (f32, f32),
        color: [f32; 4],
    ) {
        let raw_x0 = origin_x + q.x;
        let raw_y0 = origin_y + q.y;
        let raw_x1 = raw_x0 + q.width;
        let raw_y1 = raw_y0 + q.height;
        let x0 = pivot_dst.0 + (raw_x0 - pivot_src.0) * scale.0;
        let y0 = pivot_dst.1 + (raw_y0 - pivot_src.1) * scale.1;
        let x1 = pivot_dst.0 + (raw_x1 - pivot_src.0) * scale.0;
        let y1 = pivot_dst.1 + (raw_y1 - pivot_src.1) * scale.1;
        let [u0, v0] = q.uv_min;
        let [u1, v1] = q.uv_max;

        self.text_vertices.extend_from_slice(&[
            TextVertex {
                pos: [x0, y0],
                uv: [u0, v0],
                color,
            },
            TextVertex {
                pos: [x1, y0],
                uv: [u1, v0],
                color,
            },
            TextVertex {
                pos: [x1, y1],
                uv: [u1, v1],
                color,
            },
            TextVertex {
                pos: [x0, y0],
                uv: [u0, v0],
                color,
            },
            TextVertex {
                pos: [x1, y1],
                uv: [u1, v1],
                color,
            },
            TextVertex {
                pos: [x0, y1],
                uv: [u0, v1],
                color,
            },
        ]);
    }

    fn push_single_quad(
        &mut self,
        q: crate::text::GlyphQuad,
        origin_x: f32,
        origin_y: f32,
        color: [f32; 4],
    ) {
        let x0 = origin_x + q.x;
        let y0 = origin_y + q.y;
        let x1 = x0 + q.width;
        let y1 = y0 + q.height;
        let [u0, v0] = q.uv_min;
        let [u1, v1] = q.uv_max;

        self.text_vertices.extend_from_slice(&[
            TextVertex {
                pos: [x0, y0],
                uv: [u0, v0],
                color,
            },
            TextVertex {
                pos: [x1, y0],
                uv: [u1, v0],
                color,
            },
            TextVertex {
                pos: [x1, y1],
                uv: [u1, v1],
                color,
            },
            TextVertex {
                pos: [x0, y0],
                uv: [u0, v0],
                color,
            },
            TextVertex {
                pos: [x1, y1],
                uv: [u1, v1],
                color,
            },
            TextVertex {
                pos: [x0, y1],
                uv: [u0, v1],
                color,
            },
        ]);
    }

    fn flush_text(&mut self, text: &mut TextAtlas, viewport: (f32, f32)) {
        if self.text_vertices.is_empty() {
            return;
        }
        let Some(atlas_tex) = text.sync_gpu(&self.gl) else {
            return;
        };
        unsafe {
            self.gl.use_program(Some(self.text.program));
            self.gl.bind_vertex_array(Some(self.text_vao));
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.text_vbo));
            self.gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck::cast_slice(&self.text_vertices),
                glow::STREAM_DRAW,
            );
            self.gl
                .uniform_2_f32(self.text.u_viewport.as_ref(), viewport.0, viewport.1);
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(atlas_tex));
            self.gl.uniform_1_i32(self.text.u_atlas.as_ref(), 0);

            self.gl
                .draw_arrays(glow::TRIANGLES, 0, self.text_vertices.len() as i32);

            self.gl.bind_texture(glow::TEXTURE_2D, None);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, None);
            self.gl.bind_vertex_array(None);
        }
    }
}

impl Drop for PickerRenderer {
    fn drop(&mut self) {
        self.destroy();
    }
}

fn compile_shader(
    gl: &glow::Context,
    shader_type: u32,
    src: &str,
    stage: &'static str,
) -> Result<glow::Shader, PickerError> {
    unsafe {
        let shader = gl.create_shader(shader_type).map_err(|detail| PickerError::Gl {
            context: "create_shader",
            detail,
        })?;
        gl.shader_source(shader, src);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            let log = gl.get_shader_info_log(shader);
            gl.delete_shader(shader);
            return Err(PickerError::Shader { stage, log });
        }
        Ok(shader)
    }
}

fn link_program(
    gl: &glow::Context,
    vert_src: &str,
    frag_src: &str,
    stage: &'static str,
) -> Result<glow::Program, PickerError> {
    unsafe {
        let vs = compile_shader(gl, glow::VERTEX_SHADER, vert_src, stage)?;
        let fs = match compile_shader(gl, glow::FRAGMENT_SHADER, frag_src, stage) {
            Ok(s) => s,
            Err(e) => {
                gl.delete_shader(vs);
                return Err(e);
            }
        };
        let prog = gl.create_program().map_err(|detail| PickerError::Gl {
            context: "create_program",
            detail,
        })?;
        gl.attach_shader(prog, vs);
        gl.attach_shader(prog, fs);
        gl.link_program(prog);
        gl.detach_shader(prog, vs);
        gl.detach_shader(prog, fs);
        gl.delete_shader(vs);
        gl.delete_shader(fs);
        if !gl.get_program_link_status(prog) {
            let log = gl.get_program_info_log(prog);
            gl.delete_program(prog);
            return Err(PickerError::Shader { stage, log });
        }
        Ok(prog)
    }
}
