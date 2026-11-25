//! GLSL shaders (supporting both OpenGL ES 3.0 and Desktop OpenGL 3.3 Core)
//! porting `shaders/wallpaper-transition.frag`, `shaders/kawase-down.frag`,
//! and `shaders/kawase-up.frag`.

use glow::HasContext;

use crate::error::{GfxError, Result};

const SCREEN_QUAD_VERT_BODY: &str = r#"
out vec2 v_uv;
void main() {
    // Fullscreen triangle covering NDC [-1, 1]^2.
    // Screen UV (0, 0) = top-left, (1, 1) = bottom-right, matching Qt ShaderEffect.
    vec2 pos = vec2((gl_VertexID == 1) ? 3.0 : -1.0, (gl_VertexID == 2) ? 3.0 : -1.0);
    gl_Position = vec4(pos, 0.0, 1.0);
    v_uv = vec2(pos.x * 0.5 + 0.5, 0.5 - pos.y * 0.5);
}
"#;

const FBO_QUAD_VERT_BODY: &str = r#"
out vec2 v_uv;
void main() {
    // Fullscreen triangle for sampling GL framebuffer textures upright.
    vec2 pos = vec2((gl_VertexID == 1) ? 3.0 : -1.0, (gl_VertexID == 2) ? 3.0 : -1.0);
    gl_Position = vec4(pos, 0.0, 1.0);
    v_uv = pos * 0.5 + 0.5;
}
"#;

const SAMPLE_SLOT_GLSL: &str = r#"
vec4 sampleSlot(sampler2D primaryTex, sampler2D uvTex, int mode, vec2 uv) {
    if (mode == 2) {
        // BT.709 limited-range NV12 -> RGB
        float y = texture(primaryTex, uv).r;
        vec2 cbcr = texture(uvTex, uv).rg - vec2(0.5, 0.5);
        float yScaled = (y - 16.0 / 255.0) * (255.0 / 219.0);
        float r = yScaled + 1.5748 * cbcr.y;
        float g = yScaled - 0.1873 * cbcr.x - 0.4681 * cbcr.y;
        float b = yScaled + 1.8556 * cbcr.x;
        return vec4(clamp(vec3(r, g, b), 0.0, 1.0), 1.0);
    }
    vec4 c = texture(primaryTex, uv);
    if (mode == 1) {
        c = c.bgra;
    }
    return c;
}
"#;

const SLOT_BLIT_FRAG_BODY: &str = r#"
in vec2 v_uv;
out vec4 fragColor;

uniform int u_mode; // 0 = rgba, 1 = bgra_swizzle, 2 = nv12
uniform sampler2D u_source;
uniform sampler2D u_uvSource;

void main() {
    fragColor = sampleSlot(u_source, u_uvSource, u_mode, v_uv);
}
"#;

const TRANSITION_FRAG_BODY: &str = r#"
in vec2 v_uv;
out vec4 fragColor;

uniform float u_transitionProgress;
uniform int u_effectKind;
uniform vec2 u_resolution;
uniform vec4 u_oldRect;
uniform vec4 u_newRect;
uniform vec2 u_origin;
uniform vec2 u_direction;
uniform vec2 u_wave;
uniform float u_opacity;

uniform int u_oldMode; // 0 = rgba, 1 = bgra_swizzle, 2 = nv12
uniform int u_newMode; // 0 = rgba, 1 = bgra_swizzle, 2 = nv12

uniform sampler2D u_oldSource;
uniform sampler2D u_oldUvSource;
uniform sampler2D u_newSource;
uniform sampler2D u_newUvSource;

void main() {
    vec2 pixel = v_uv * u_resolution;
    vec2 oldUV = (pixel - u_oldRect.xy) / max(u_oldRect.zw, vec2(1.0));
    vec2 newUV = (pixel - u_newRect.xy) / max(u_newRect.zw, vec2(1.0));
    vec4 before = sampleSlot(u_oldSource, u_oldUvSource, u_oldMode, oldUV);
    vec4 after = sampleSlot(u_newSource, u_newUvSource, u_newMode, newUV);

    float p = clamp(u_transitionProgress, 0.0, 1.0);
    float mask = p;

    if (u_effectKind == 1 || u_effectKind == 2) {
        float span = max(dot(abs(u_direction), u_resolution), 1.0);
        float coordinate = dot(pixel - u_resolution * 0.5, u_direction) / span + 0.5;
        if (u_effectKind == 2) {
            vec2 tangent = vec2(-u_direction.y, u_direction.x);
            coordinate += sin(dot(pixel, tangent) / max(u_wave.x, 1.0) * 6.2831853) * u_wave.y / span;
        }
        float edge = 1.5 / span;
        float extra = u_effectKind == 2 ? u_wave.y / span : 0.0;
        float threshold = mix(-extra - edge, 1.0 + extra + edge, p);
        mask = 1.0 - smoothstep(threshold - edge, threshold + edge, coordinate);
    } else if (u_effectKind == 3 || u_effectKind == 4) {
        vec2 center = u_origin * u_resolution;
        float radius = max(length(max(center, u_resolution - center)), 1.0);
        float distance = length(pixel - center) / radius;
        float edge = 1.5 / radius;
        if (u_effectKind == 3)
            mask = 1.0 - smoothstep(p - edge, p + edge, distance);
        else
            mask = smoothstep(1.0 - p - edge, 1.0 - p + edge, distance);
    }

    if (p <= 0.0) mask = 0.0;
    if (p >= 1.0) mask = 1.0;
    fragColor = mix(before, after, mask) * u_opacity;
}
"#;

const KAWASE_DOWN_FRAG_BODY: &str = r#"
in vec2 v_uv;
out vec4 fragColor;

uniform sampler2D u_source;
uniform vec2 u_halfpixel;
uniform float u_offset;
uniform float u_opacity;

void main() {
    vec2 uv = v_uv;
    vec2 o = u_halfpixel * u_offset;
    vec4 sum = texture(u_source, uv) * 4.0;
    sum += texture(u_source, uv - o);
    sum += texture(u_source, uv + o);
    sum += texture(u_source, uv + vec2(o.x, -o.y));
    sum += texture(u_source, uv - vec2(o.x, -o.y));
    fragColor = (sum / 8.0) * u_opacity;
}
"#;

const KAWASE_UP_FRAG_BODY: &str = r#"
in vec2 v_uv;
out vec4 fragColor;

uniform sampler2D u_source;
uniform vec2 u_halfpixel;
uniform float u_offset;
uniform float u_saturation;
uniform float u_brightness;
uniform float u_opacity;

void main() {
    vec2 uv = v_uv;
    vec2 o = u_halfpixel * u_offset;
    vec4 sum = texture(u_source, uv + vec2(-o.x * 2.0, 0.0));
    sum += texture(u_source, uv + vec2(-o.x, o.y)) * 2.0;
    sum += texture(u_source, uv + vec2(0.0, o.y * 2.0));
    sum += texture(u_source, uv + vec2(o.x, o.y)) * 2.0;
    sum += texture(u_source, uv + vec2(o.x * 2.0, 0.0));
    sum += texture(u_source, uv + vec2(o.x, -o.y)) * 2.0;
    sum += texture(u_source, uv + vec2(0.0, -o.y * 2.0));
    sum += texture(u_source, uv + vec2(-o.x, -o.y)) * 2.0;
    vec4 col = sum / 12.0;
    if (u_saturation != 1.0 || u_brightness != 0.0) {
        float luma = dot(col.rgb, vec3(0.2126, 0.7152, 0.0722));
        col.rgb = mix(vec3(luma), col.rgb, u_saturation) + vec3(u_brightness);
        col.rgb = clamp(col.rgb, 0.0, 1.0);
    }
    fragColor = vec4(col.rgb, 1.0) * u_opacity;
}
"#;

pub struct TransitionProgram {
    pub program: glow::Program,
    pub u_transition_progress: Option<glow::UniformLocation>,
    pub u_effect_kind: Option<glow::UniformLocation>,
    pub u_resolution: Option<glow::UniformLocation>,
    pub u_old_rect: Option<glow::UniformLocation>,
    pub u_new_rect: Option<glow::UniformLocation>,
    pub u_origin: Option<glow::UniformLocation>,
    pub u_direction: Option<glow::UniformLocation>,
    pub u_wave: Option<glow::UniformLocation>,
    pub u_opacity: Option<glow::UniformLocation>,
    pub u_old_mode: Option<glow::UniformLocation>,
    pub u_new_mode: Option<glow::UniformLocation>,
    pub u_old_source: Option<glow::UniformLocation>,
    pub u_old_uv_source: Option<glow::UniformLocation>,
    pub u_new_source: Option<glow::UniformLocation>,
    pub u_new_uv_source: Option<glow::UniformLocation>,
}

pub struct KawaseDownProgram {
    pub program: glow::Program,
    pub u_source: Option<glow::UniformLocation>,
    pub u_halfpixel: Option<glow::UniformLocation>,
    pub u_offset: Option<glow::UniformLocation>,
    pub u_opacity: Option<glow::UniformLocation>,
}

pub struct KawaseUpProgram {
    pub program: glow::Program,
    pub u_source: Option<glow::UniformLocation>,
    pub u_halfpixel: Option<glow::UniformLocation>,
    pub u_offset: Option<glow::UniformLocation>,
    pub u_saturation: Option<glow::UniformLocation>,
    pub u_brightness: Option<glow::UniformLocation>,
    pub u_opacity: Option<glow::UniformLocation>,
}

pub struct SlotBlitProgram {
    pub program: glow::Program,
    pub u_mode: Option<glow::UniformLocation>,
    pub u_source: Option<glow::UniformLocation>,
    pub u_uv_source: Option<glow::UniformLocation>,
}

pub struct ShaderPrograms {
    pub empty_vao: glow::VertexArray,
    pub transition: TransitionProgram,
    pub slot_blit: SlotBlitProgram,
    pub kawase_down: KawaseDownProgram,
    pub kawase_up: KawaseUpProgram,
}

impl ShaderPrograms {
    pub fn compile(gl: &glow::Context, is_gles: bool) -> Result<Self> {
        let header = if is_gles {
            "#version 300 es\nprecision highp float;\nprecision highp int;\n"
        } else {
            "#version 330 core\n"
        };

        let screen_vert = format!("{header}{SCREEN_QUAD_VERT_BODY}");
        let fbo_vert = format!("{header}{FBO_QUAD_VERT_BODY}");
        let trans_frag = format!("{header}{SAMPLE_SLOT_GLSL}{TRANSITION_FRAG_BODY}");
        let blit_frag = format!("{header}{SAMPLE_SLOT_GLSL}{SLOT_BLIT_FRAG_BODY}");
        let down_frag = format!("{header}{KAWASE_DOWN_FRAG_BODY}");
        let up_frag = format!("{header}{KAWASE_UP_FRAG_BODY}");

        let empty_vao = unsafe {
            gl.create_vertex_array().map_err(|e| GfxError::Gl {
                context: "create_vertex_array",
                detail: e,
            })?
        };

        let transition_prog = link_program(gl, &screen_vert, &trans_frag, "wallpaper-transition")?;
        let blit_prog = link_program(gl, &fbo_vert, &blit_frag, "slot-blit")?;
        let down_prog = link_program(gl, &fbo_vert, &down_frag, "kawase-down")?;
        let up_prog = link_program(gl, &fbo_vert, &up_frag, "kawase-up")?;

        unsafe {
            let transition = TransitionProgram {
                u_transition_progress: gl.get_uniform_location(transition_prog, "u_transitionProgress"),
                u_effect_kind: gl.get_uniform_location(transition_prog, "u_effectKind"),
                u_resolution: gl.get_uniform_location(transition_prog, "u_resolution"),
                u_old_rect: gl.get_uniform_location(transition_prog, "u_oldRect"),
                u_new_rect: gl.get_uniform_location(transition_prog, "u_newRect"),
                u_origin: gl.get_uniform_location(transition_prog, "u_origin"),
                u_direction: gl.get_uniform_location(transition_prog, "u_direction"),
                u_wave: gl.get_uniform_location(transition_prog, "u_wave"),
                u_opacity: gl.get_uniform_location(transition_prog, "u_opacity"),
                u_old_mode: gl.get_uniform_location(transition_prog, "u_oldMode"),
                u_new_mode: gl.get_uniform_location(transition_prog, "u_newMode"),
                u_old_source: gl.get_uniform_location(transition_prog, "u_oldSource"),
                u_old_uv_source: gl.get_uniform_location(transition_prog, "u_oldUvSource"),
                u_new_source: gl.get_uniform_location(transition_prog, "u_newSource"),
                u_new_uv_source: gl.get_uniform_location(transition_prog, "u_newUvSource"),
                program: transition_prog,
            };
            let slot_blit = SlotBlitProgram {
                u_mode: gl.get_uniform_location(blit_prog, "u_mode"),
                u_source: gl.get_uniform_location(blit_prog, "u_source"),
                u_uv_source: gl.get_uniform_location(blit_prog, "u_uvSource"),
                program: blit_prog,
            };
            let kawase_down = KawaseDownProgram {
                u_source: gl.get_uniform_location(down_prog, "u_source"),
                u_halfpixel: gl.get_uniform_location(down_prog, "u_halfpixel"),
                u_offset: gl.get_uniform_location(down_prog, "u_offset"),
                u_opacity: gl.get_uniform_location(down_prog, "u_opacity"),
                program: down_prog,
            };
            let kawase_up = KawaseUpProgram {
                u_source: gl.get_uniform_location(up_prog, "u_source"),
                u_halfpixel: gl.get_uniform_location(up_prog, "u_halfpixel"),
                u_offset: gl.get_uniform_location(up_prog, "u_offset"),
                u_saturation: gl.get_uniform_location(up_prog, "u_saturation"),
                u_brightness: gl.get_uniform_location(up_prog, "u_brightness"),
                u_opacity: gl.get_uniform_location(up_prog, "u_opacity"),
                program: up_prog,
            };

            Ok(Self {
                empty_vao,
                transition,
                slot_blit,
                kawase_down,
                kawase_up,
            })
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.transition.program);
            gl.delete_program(self.slot_blit.program);
            gl.delete_program(self.kawase_down.program);
            gl.delete_program(self.kawase_up.program);
            gl.delete_vertex_array(self.empty_vao);
        }
    }
}

fn compile_shader(gl: &glow::Context, shader_type: u32, src: &str, stage: &'static str) -> Result<glow::Shader> {
    unsafe {
        let shader = gl.create_shader(shader_type).map_err(|e| GfxError::Gl {
            context: "create_shader",
            detail: e,
        })?;
        gl.shader_source(shader, src);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            let log = gl.get_shader_info_log(shader);
            gl.delete_shader(shader);
            return Err(GfxError::Shader { stage, log });
        }
        Ok(shader)
    }
}

fn link_program(
    gl: &glow::Context,
    vert_src: &str,
    frag_src: &str,
    stage: &'static str,
) -> Result<glow::Program> {
    unsafe {
        let vs = compile_shader(gl, glow::VERTEX_SHADER, vert_src, stage)?;
        let fs = match compile_shader(gl, glow::FRAGMENT_SHADER, frag_src, stage) {
            Ok(s) => s,
            Err(e) => {
                gl.delete_shader(vs);
                return Err(e);
            }
        };
        let prog = gl.create_program().map_err(|e| GfxError::Gl {
            context: "create_program",
            detail: e,
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
            return Err(GfxError::Shader { stage, log });
        }
        Ok(prog)
    }
}
