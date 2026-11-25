use std::path::PathBuf;
use std::sync::Arc;

use bgpicker_gfx::{
    DualKawaseBlur, EglContext, FrameInput, KawaseSettings, LayerPipeline, LayerRole,
    ShaderPrograms, TextureSlot, TransitionEffect, TransitionSpec, compute_mode_geometry,
};
use glow::HasContext;

struct OffscreenTarget {
    fbo: glow::Framebuffer,
    tex: glow::Texture,
    width: u32,
    height: u32,
}

impl OffscreenTarget {
    fn new(gl: &glow::Context, width: u32, height: u32) -> Self {
        unsafe {
            let tex = gl.create_texture().expect("create texture");
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
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
            let fbo = gl.create_framebuffer().expect("create fbo");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(tex),
                0,
            );
            assert_eq!(
                gl.check_framebuffer_status(glow::FRAMEBUFFER),
                glow::FRAMEBUFFER_COMPLETE
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            Self {
                fbo,
                tex,
                width,
                height,
            }
        }
    }

    fn read_pixel_top_down(&self, gl: &glow::Context, nx: f32, ny: f32) -> [f32; 4] {
        let mut buf = vec![0u8; (self.width * self.height * 4) as usize];
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.read_pixels(
                0,
                0,
                self.width as i32,
                self.height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut buf)),
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        let px = ((nx * ((self.width - 1) as f32)).floor() as u32).min(self.width - 1);
        let py_top = ((ny * ((self.height - 1) as f32)).floor() as u32).min(self.height - 1);
        // OpenGL read_pixels row 0 is bottom; flip vertically to match top-left screen coords.
        let py_gl = (self.height - 1) - py_top;
        let idx = ((py_gl * self.width + px) * 4) as usize;
        [
            buf[idx] as f32 / 255.0,
            buf[idx + 1] as f32 / 255.0,
            buf[idx + 2] as f32 / 255.0,
            buf[idx + 3] as f32 / 255.0,
        ]
    }

    fn read_pixel_at_px_top_down(&self, gl: &glow::Context, px: u32, py_top: u32) -> [f32; 4] {
        let mut buf = vec![0u8; (self.width * self.height * 4) as usize];
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.read_pixels(
                0,
                0,
                self.width as i32,
                self.height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut buf)),
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        let px = px.min(self.width - 1);
        let py_top = py_top.min(self.height - 1);
        let py_gl = (self.height - 1) - py_top;
        let idx = ((py_gl * self.width + px) * 4) as usize;
        [
            buf[idx] as f32 / 255.0,
            buf[idx + 1] as f32 / 255.0,
            buf[idx + 2] as f32 / 255.0,
            buf[idx + 3] as f32 / 255.0,
        ]
    }

    fn destroy(self, gl: &glow::Context) {
        unsafe {
            gl.delete_framebuffer(self.fbo);
            gl.delete_texture(self.tex);
        }
    }
}

fn assert_near_rgb(px: [f32; 4], r: f32, g: f32, b: f32) {
    let eps = 0.035;
    assert!(
        (px[0] - r).abs() <= eps && (px[1] - g).abs() <= eps && (px[2] - b).abs() <= eps,
        "unexpected pixel {px:?}, expected ~({r}, {g}, {b})"
    );
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn solid_rgba(key: &str, generation: u64, w: u32, h: u32, rgba: [u8; 4]) -> FrameInput {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        data.extend_from_slice(&rgba);
    }
    FrameInput::from_rgba(key, generation, w, h, None, data).unwrap()
}

fn split_left_right_rgba(
    key: &str,
    generation: u64,
    w: u32,
    h: u32,
    left: [u8; 4],
    right: [u8; 4],
) -> FrameInput {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for _y in 0..h {
        for x in 0..w {
            if x < w / 2 {
                data.extend_from_slice(&left);
            } else {
                data.extend_from_slice(&right);
            }
        }
    }
    FrameInput::from_rgba(key, generation, w, h, None, data).unwrap()
}

#[test]
fn transition_effects_render_expected_pixels_on_gpu() {
    let egl = EglContext::new_headless().expect("headless EGL context");
    let programs = ShaderPrograms::compile(&egl.gl, egl.is_gles).expect("compile shaders");
    let target = OffscreenTarget::new(&egl.gl, 128, 96);

    let red_green = FrameInput::from_image_path(fixture_path("red-green.png"), 4096)
        .unwrap_or_else(|_| split_left_right_rgba("red-green", 0, 256, 96, [255, 0, 0, 255], [0, 255, 0, 255]));
    let blue = FrameInput::from_image_path(fixture_path("blue.png"), 4096)
        .unwrap_or_else(|_| solid_rgba("blue", 0, 128, 96, [0, 0, 255, 255]));

    for effect in [
        TransitionEffect::Fade,
        TransitionEffect::Wipe,
        TransitionEffect::Wave,
        TransitionEffect::Grow,
        TransitionEffect::Outer,
    ] {
        let mut layer = LayerPipeline::new(LayerRole::Wallpaper, true);
        layer.set_progress(0.0, 0);
        layer
            .apply_frame(&egl.gl, &red_green, TransitionSpec::instant(), egl.bgra_ext_supported)
            .unwrap();

        let spec = TransitionSpec {
            effect,
            duration_ms: 1000,
            angle_deg: 0.0, // direction = [-1, 0]: sweeps from right to left (x=0.95 -> 0.05)
            position: [0.5, 0.5],
            wave: [20.0, 20.0],
        };
        layer
            .apply_frame(&egl.gl, &blue, spec, egl.bgra_ext_supported)
            .unwrap();

        // At t = 0: old image (left side is red).
        layer
            .render(
                &egl.gl,
                &programs,
                Some(target.fbo),
                128,
                96,
                1.0,
                KawaseSettings::default(),
            )
            .unwrap();
        assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.1, 0.5), 1.0, 0.0, 0.0);

        // Advance in <= 0.25s steps to midpoint (ease_transition_bezier(~0.45) ~ 0.50).
        layer.tick(&egl.gl, 0.22);
        layer.tick(&egl.gl, 0.23);
        let mid_p = layer.transition_progress();
        assert!(mid_p > 0.40 && mid_p < 0.65, "expected midpoint progress, got {mid_p}");

        layer
            .render(
                &egl.gl,
                &programs,
                Some(target.fbo),
                128,
                96,
                1.0,
                KawaseSettings::default(),
            )
            .unwrap();

        match effect {
            TransitionEffect::Fade => {
                let px = target.read_pixel_top_down(&egl.gl, 0.1, 0.5);
                assert!(px[0] > 0.35 && px[2] > 0.35 && px[1] < 0.05, "fade midpoint: {px:?}");
            }
            TransitionEffect::Wipe | TransitionEffect::Wave => {
                assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.05, 0.5), 1.0, 0.0, 0.0);
                assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.95, 0.5), 0.0, 0.0, 1.0);
            }
            TransitionEffect::Grow => {
                assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.5, 0.5), 0.0, 0.0, 1.0);
                assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.02, 0.02), 1.0, 0.0, 0.0);
            }
            TransitionEffect::Outer => {
                assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.5, 0.5), 1.0, 0.0, 0.0);
                assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.98, 0.02), 0.0, 0.0, 1.0);
            }
            TransitionEffect::None => unreachable!(),
        }

        // Advance past completion (1000ms total): full new image (blue).
        for _ in 0..4 {
            layer.tick(&egl.gl, 0.20);
        }
        assert!(!layer.is_transition_active());
        layer
            .render(
                &egl.gl,
                &programs,
                Some(target.fbo),
                128,
                96,
                1.0,
                KawaseSettings::default(),
            )
            .unwrap();
        assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.9, 0.5), 0.0, 0.0, 1.0);
        layer.release_all(&egl.gl);
    }

    target.destroy(&egl.gl);
    programs.destroy(&egl.gl);
}

#[test]
fn mode_blend_and_parallax_pan_preserve_crop_without_handoff() {
    let egl = EglContext::new_headless().expect("headless EGL context");
    let programs = ShaderPrograms::compile(&egl.gl, egl.is_gles).expect("compile shaders");
    let target = OffscreenTarget::new(&egl.gl, 128, 96);

    let wide = split_left_right_rgba("wide", 0, 512, 96, [255, 0, 0, 255], [0, 255, 0, 255]);
    let mut layer = LayerPipeline::new(LayerRole::Wallpaper, false);
    layer
        .apply_frame(&egl.gl, &wide, TransitionSpec::instant(), egl.bgra_ext_supported)
        .unwrap();

    // Normal mode (mode_blend = 0): centered crop shows red on left, green on right.
    layer
        .render(
            &egl.gl,
            &programs,
            Some(target.fbo),
            128,
            96,
            1.12,
            KawaseSettings::default(),
        )
        .unwrap();
    assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.1, 0.5), 1.0, 0.0, 0.0);
    assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.9, 0.5), 0.0, 1.0, 0.0);

    // Smoothly transition into parallax mode and pan to right edge (progress = 1.0).
    layer.set_mode(true, 400);
    layer.set_progress(1.0, 400);
    layer.tick(&egl.gl, 0.2);
    assert!(layer.mode_blend() > 0.4 && layer.mode_blend() < 0.6);
    layer.tick(&egl.gl, 0.25);
    assert!((layer.mode_blend() - 1.0).abs() < 1e-4);
    assert!((layer.animated_progress() - 1.0).abs() < 1e-4);

    layer
        .render(
            &egl.gl,
            &programs,
            Some(target.fbo),
            128,
            96,
            1.12,
            KawaseSettings::default(),
        )
        .unwrap();
    // Panned all the way right: entire viewport shows green half.
    assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.1, 0.5), 0.0, 1.0, 0.0);
    assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.9, 0.5), 0.0, 1.0, 0.0);

    layer.release_all(&egl.gl);
    target.destroy(&egl.gl);
    programs.destroy(&egl.gl);
}

#[test]
fn dual_kawase_3pass_backdrop_blurs_high_contrast_edge() {
    let egl = EglContext::new_headless().expect("headless EGL context");
    let programs = ShaderPrograms::compile(&egl.gl, egl.is_gles).expect("compile shaders");
    let target = OffscreenTarget::new(&egl.gl, 256, 192);

    let split = split_left_right_rgba("split", 0, 256, 192, [255, 0, 0, 255], [0, 255, 0, 255]);
    let mut backdrop = LayerPipeline::new(LayerRole::Backdrop, false);
    backdrop
        .apply_frame(&egl.gl, &split, TransitionSpec::instant(), egl.bgra_ext_supported)
        .unwrap();

    backdrop
        .render(
            &egl.gl,
            &programs,
            Some(target.fbo),
            256,
            192,
            1.12,
            KawaseSettings::default(),
        )
        .unwrap();

    let left = target.read_pixel_top_down(&egl.gl, 0.1, 0.5);
    let mid = target.read_pixel_top_down(&egl.gl, 0.5, 0.5);
    let right = target.read_pixel_top_down(&egl.gl, 0.9, 0.5);

    assert!(left[0] > 0.7 && left[1] < 0.2, "left should remain mostly red: {left:?}");
    assert!(right[1] > 0.7 && right[0] < 0.2, "right should remain mostly green: {right:?}");
    assert!(
        mid[0] > 0.20 && mid[1] > 0.20,
        "center edge must be blurred across red & green: {mid:?}"
    );
    assert!((mid[3] - 1.0).abs() < 0.02, "backdrop alpha must be 1.0: {mid:?}");

    backdrop.release_all(&egl.gl);
    target.destroy(&egl.gl);
    programs.destroy(&egl.gl);
}

#[test]
fn unified_frame_input_supports_bgra_nv12_and_shared_video_stream() {
    let egl = EglContext::new_headless().expect("headless EGL context");
    let programs = ShaderPrograms::compile(&egl.gl, egl.is_gles).expect("compile shaders");
    let target = OffscreenTarget::new(&egl.gl, 64, 64);

    // 1. TextureSlot skips redundant upload for same (media_key, generation).
    let frame1 = solid_rgba("video://loop.mp4", 1, 64, 64, [255, 0, 0, 255]);
    let mut slot = TextureSlot::new();
    assert!(slot.upload(&egl.gl, &frame1, egl.bgra_ext_supported).unwrap());
    assert!(!slot.upload(&egl.gl, &frame1, egl.bgra_ext_supported).unwrap());
    let frame2 = solid_rgba("video://loop.mp4", 2, 64, 64, [0, 0, 255, 255]);
    assert!(slot.upload(&egl.gl, &frame2, egl.bgra_ext_supported).unwrap());
    slot.release(&egl.gl);

    // 2. Shared FrameInput feeds both Wallpaper and Backdrop simultaneously, including BGRA8.
    let mut bgra_bytes = Vec::with_capacity(64 * 64 * 4);
    for _ in 0..(64 * 64) {
        bgra_bytes.extend_from_slice(&[0, 0, 255, 255]); // B=0, G=0, R=255, A=255
    }
    let shared_bgra = FrameInput::from_bgra("video://bgra", 1, 64, 64, None, bgra_bytes).unwrap();
    let mut fg = LayerPipeline::new(LayerRole::Wallpaper, false);
    let mut bg = LayerPipeline::new(LayerRole::Backdrop, false);
    assert!(fg.push_video_frame(&egl.gl, &shared_bgra, egl.bgra_ext_supported).unwrap());
    assert!(bg.push_video_frame(&egl.gl, &shared_bgra, egl.bgra_ext_supported).unwrap());

    fg.render(&egl.gl, &programs, Some(target.fbo), 64, 64, 1.0, KawaseSettings::default())
        .unwrap();
    assert_near_rgb(target.read_pixel_top_down(&egl.gl, 0.5, 0.5), 1.0, 0.0, 0.0);

    bg.render(&egl.gl, &programs, Some(target.fbo), 64, 64, 1.0, KawaseSettings::default())
        .unwrap();
    let blurred_red = target.read_pixel_top_down(&egl.gl, 0.5, 0.5);
    assert!(blurred_red[0] > 0.8 && blurred_red[1] < 0.1 && blurred_red[2] < 0.1);

    // 3. NV12 planar upload + GPU YUV->RGB conversion (Y=81, U=90, V=240 -> ~red).
    let w = 64u32;
    let h = 64u32;
    let mut nv12 = vec![81u8; (w * h) as usize];
    for _ in 0..((w / 2) * (h / 2)) {
        nv12.push(90);
        nv12.push(240);
    }
    let nv12_frame =
        FrameInput::from_nv12("video://bgra", 2, w, h, w, w, Arc::<[u8]>::from(nv12)).unwrap();
    assert!(fg.push_video_frame(&egl.gl, &nv12_frame, egl.bgra_ext_supported).unwrap());
    fg.render(&egl.gl, &programs, Some(target.fbo), 64, 64, 1.0, KawaseSettings::default())
        .unwrap();
    let yuv_red = target.read_pixel_top_down(&egl.gl, 0.5, 0.5);
    assert!(yuv_red[0] > 0.85 && yuv_red[1] < 0.15 && yuv_red[2] < 0.15, "nv12 red: {yuv_red:?}");

    fg.release_all(&egl.gl);
    bg.release_all(&egl.gl);
    target.destroy(&egl.gl);
    programs.destroy(&egl.gl);
}

fn split_top_bottom_rgba(
    key: &str,
    generation: u64,
    w: u32,
    h: u32,
    top: [u8; 4],
    bottom: [u8; 4],
) -> FrameInput {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let px = if y < h / 2 { top } else { bottom };
        for _x in 0..w {
            data.extend_from_slice(&px);
        }
    }
    FrameInput::from_rgba(key, generation, w, h, None, data).unwrap()
}

fn split_top_bottom_bgra(
    key: &str,
    generation: u64,
    w: u32,
    h: u32,
    top_bgra: [u8; 4],
    bottom_bgra: [u8; 4],
) -> FrameInput {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let px = if y < h / 2 { top_bgra } else { bottom_bgra };
        for _x in 0..w {
            data.extend_from_slice(&px);
        }
    }
    FrameInput::from_bgra(key, generation, w, h, None, data).unwrap()
}

fn split_top_bottom_nv12(
    key: &str,
    generation: u64,
    w: u32,
    h: u32,
    top_yuv: [u8; 3],
    bottom_yuv: [u8; 3],
) -> FrameInput {
    let uv_h = h.div_ceil(2);
    let uv_w_pairs = w.div_ceil(2);
    let mut nv12 = Vec::with_capacity((w * h + uv_w_pairs * 2 * uv_h) as usize);
    for y in 0..h {
        let y_val = if y < h / 2 { top_yuv[0] } else { bottom_yuv[0] };
        nv12.resize(nv12.len() + w as usize, y_val);
    }
    for uy in 0..uv_h {
        let [_, u, v] = if uy < uv_h / 2 { top_yuv } else { bottom_yuv };
        for _ in 0..uv_w_pairs {
            nv12.push(u);
            nv12.push(v);
        }
    }
    FrameInput::from_nv12(key, generation, w, h, w, uv_w_pairs * 2, nv12).unwrap()
}

#[test]
fn backdrop_panning_reuses_cached_blur_and_preserves_upright_orientation() {
    let egl = EglContext::new_headless().expect("headless EGL context");
    let programs = ShaderPrograms::compile(&egl.gl, egl.is_gles).expect("compile shaders");
    let target = OffscreenTarget::new(&egl.gl, 256, 192);
    let kawase = KawaseSettings::default();

    assert_eq!(DualKawaseBlur::capped_image_size(3840, 2160), (1920, 1080));
    assert_eq!(DualKawaseBlur::capped_image_size(7200, 2400), (1920, 640));
    assert_eq!(DualKawaseBlur::capped_image_size(256, 192), (256, 192));

    let rgba = split_top_bottom_rgba(
        "static://rgba",
        0,
        384,
        192,
        [255, 0, 0, 255],
        [0, 0, 255, 255],
    );
    // BGRA bytes for top=red [0,0,255,255], bottom=blue [255,0,0,255].
    let bgra = split_top_bottom_bgra(
        "static://bgra",
        0,
        384,
        192,
        [0, 0, 255, 255],
        [255, 0, 0, 255],
    );
    // NV12 BT.709 limited-range: top=red (81, 90, 240), bottom=blue (41, 240, 110).
    let nv12 = split_top_bottom_nv12("static://nv12", 0, 384, 192, [81, 90, 240], [41, 240, 110]);

    for (label, frame, force_swizzle) in [
        ("rgba8", rgba, false),
        ("bgra8-swizzle", bgra.clone(), true),
        ("bgra8-ext", bgra, false),
        ("nv12", nv12, false),
    ] {
        let bgra_ext = if force_swizzle {
            false
        } else {
            egl.bgra_ext_supported
        };
        let mut backdrop = LayerPipeline::new(LayerRole::Backdrop, false);
        backdrop
            .apply_frame(&egl.gl, &frame, TransitionSpec::instant(), bgra_ext)
            .unwrap();
        assert_eq!(backdrop.blur_compute_count(), 0);

        backdrop
            .render(&egl.gl, &programs, Some(target.fbo), 256, 192, 1.12, kawase)
            .unwrap();
        assert_eq!(backdrop.blur_compute_count(), 1, "{label}: initial blur");

        let top = target.read_pixel_top_down(&egl.gl, 0.5, 0.1);
        let bottom = target.read_pixel_top_down(&egl.gl, 0.5, 0.9);
        assert!(
            top[0] > 0.7 && top[2] < 0.2,
            "{label}: expected red at screen top (tex_y=0), got {top:?}"
        );
        assert!(
            bottom[2] > 0.7 && bottom[0] < 0.2,
            "{label}: expected blue at screen bottom (tex_y=1), got {bottom:?}"
        );

        // Pan and zoom across multiple animation frames: Dual-Kawase must NOT re-run.
        backdrop.set_mode(true, 300);
        backdrop.set_progress(0.0, 0);
        backdrop
            .render(&egl.gl, &programs, Some(target.fbo), 256, 192, 1.12, kawase)
            .unwrap();
        backdrop.set_progress(1.0, 300);
        for step in 0..8 {
            backdrop.tick(&egl.gl, 0.05);
            backdrop
                .render(&egl.gl, &programs, Some(target.fbo), 256, 192, 1.12, kawase)
                .unwrap();
            assert_eq!(
                backdrop.blur_compute_count(),
                1,
                "{label}: pan/zoom frame {step} must reuse cached blur"
            );
        }

        let top_after = target.read_pixel_top_down(&egl.gl, 0.5, 0.1);
        let bottom_after = target.read_pixel_top_down(&egl.gl, 0.5, 0.9);
        assert!(
            top_after[0] > 0.7 && top_after[2] < 0.2,
            "{label}: expected red at screen top after pan, got {top_after:?}"
        );
        assert!(
            bottom_after[2] > 0.7 && bottom_after[0] < 0.2,
            "{label}: expected blue at screen bottom after pan, got {bottom_after:?}"
        );

        // Changing KawaseSettings recomputes blur once, then subsequent pan frames reuse it.
        let modified_kawase = KawaseSettings {
            offset: 2.0,
            ..kawase
        };
        backdrop
            .render(
                &egl.gl,
                &programs,
                Some(target.fbo),
                256,
                192,
                1.12,
                modified_kawase,
            )
            .unwrap();
        assert_eq!(backdrop.blur_compute_count(), 2, "{label}: kawase change recomputes");
        backdrop.set_progress(0.25, 0);
        backdrop
            .render(
                &egl.gl,
                &programs,
                Some(target.fbo),
                256,
                192,
                1.12,
                modified_kawase,
            )
            .unwrap();
        assert_eq!(backdrop.blur_compute_count(), 2);

        // Transitioning to a second static image computes blur once for the new slot only.
        let second = split_top_bottom_rgba(
            "static://second",
            0,
            384,
            192,
            [0, 255, 0, 255],
            [255, 255, 0, 255],
        );
        let spec = TransitionSpec {
            effect: TransitionEffect::Fade,
            duration_ms: 400,
            ..TransitionSpec::default()
        };
        backdrop.apply_frame(&egl.gl, &second, spec, bgra_ext).unwrap();
        for _ in 0..5 {
            backdrop.tick(&egl.gl, 0.08);
            backdrop
                .render(
                    &egl.gl,
                    &programs,
                    Some(target.fbo),
                    256,
                    192,
                    1.12,
                    modified_kawase,
                )
                .unwrap();
            assert_eq!(
                backdrop.blur_compute_count(),
                3,
                "{label}: transition must not re-run Dual-Kawase"
            );
        }

        backdrop.release_all(&egl.gl);
    }

    // Live video frames still recompute once per newly uploaded generation, not on pan.
    let mut video_bg = LayerPipeline::new(LayerRole::Backdrop, true);
    let v1 = solid_rgba("video://stream", 1, 128, 96, [255, 0, 0, 255]);
    assert!(video_bg.push_video_frame(&egl.gl, &v1, egl.bgra_ext_supported).unwrap());
    video_bg
        .render(&egl.gl, &programs, Some(target.fbo), 256, 192, 1.12, kawase)
        .unwrap();
    assert_eq!(video_bg.blur_compute_count(), 1);
    video_bg.set_progress(0.8, 0);
    video_bg
        .render(&egl.gl, &programs, Some(target.fbo), 256, 192, 1.12, kawase)
        .unwrap();
    assert_eq!(video_bg.blur_compute_count(), 1);

    let v2 = solid_rgba("video://stream", 2, 128, 96, [0, 255, 0, 255]);
    assert!(video_bg.push_video_frame(&egl.gl, &v2, egl.bgra_ext_supported).unwrap());
    video_bg
        .render(&egl.gl, &programs, Some(target.fbo), 256, 192, 1.12, kawase)
        .unwrap();
    assert_eq!(video_bg.blur_compute_count(), 2);

    video_bg.release_all(&egl.gl);
    target.destroy(&egl.gl);
    programs.destroy(&egl.gl);
}

#[test]
fn backdrop_feature_brightness_is_shift_invariant_across_subpixel_panning() {
    let egl = EglContext::new_headless().expect("headless EGL context");
    let programs = ShaderPrograms::compile(&egl.gl, egl.is_gles).expect("compile shaders");
    let vw = 256u32;
    let vh = 192u32;
    let target = OffscreenTarget::new(&egl.gl, vw, vh);

    // Wide panorama (768x192) with a bright 6px vertical stripe at the center on a dark background.
    // On a 256x192 viewport, overflow_x = 512 px and 1 image px == 1 screen px.
    let mw = 768u32;
    let mh = 192u32;
    let mut data = Vec::with_capacity((mw * mh * 4) as usize);
    for _y in 0..mh {
        for x in 0..mw {
            if (381..387).contains(&x) {
                data.extend_from_slice(&[255, 255, 255, 255]);
            } else {
                data.extend_from_slice(&[24, 24, 24, 255]);
            }
        }
    }
    let panorama = FrameInput::from_rgba("pano://stripe", 0, mw, mh, None, data).unwrap();

    let mut backdrop = LayerPipeline::new(LayerRole::Backdrop, true);
    backdrop
        .apply_frame(&egl.gl, &panorama, TransitionSpec::instant(), egl.bgra_ext_supported)
        .unwrap();

    let kawase = KawaseSettings::default();
    let zoom = 1.12;
    let mut min_luma = f32::MAX;
    let mut max_luma = f32::MIN;

    // Sweep 65 subpixel offsets spanning 16 full screen pixels (= a full 1/16 screen-grid cell).
    for k in 0..=64 {
        let progress = 0.5 + (k as f32 * 0.25) / 512.0;
        backdrop.set_progress(progress, 0);
        backdrop
            .render(&egl.gl, &programs, Some(target.fbo), vw, vh, zoom, kawase)
            .unwrap();
        assert_eq!(backdrop.blur_compute_count(), 1);

        let geom = compute_mode_geometry(
            vw as f32,
            vh as f32,
            mw as f32,
            mh as f32,
            zoom,
            progress,
            1.0,
        );
        let feature_screen_x = geom.x + 0.5 * geom.width;
        let px = feature_screen_x.floor().clamp(0.0, (vw - 1) as f32) as u32;
        let c = target.read_pixel_at_px_top_down(&egl.gl, px, vh / 2);
        let luma = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        min_luma = min_luma.min(luma);
        max_luma = max_luma.max(luma);
    }

    assert!(
        min_luma > 0.15,
        "expected visible blurred stripe peak, got min_luma={min_luma}"
    );
    assert!(
        (max_luma - min_luma) < 0.03,
        "backdrop stripe brightness shimmered across subpixel pan: min={min_luma}, max={max_luma}"
    );

    backdrop.release_all(&egl.gl);
    target.destroy(&egl.gl);
    programs.destroy(&egl.gl);
}
