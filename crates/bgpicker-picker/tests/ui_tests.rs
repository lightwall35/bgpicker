use std::sync::Arc;
use std::time::{Duration, Instant};

use bgpicker_core::{CatalogEntry, EngineKind, TransitionType, VideoMode, WallpaperMode};
use bgpicker_gfx::EglContext;
use bgpicker_picker::{
    CarouselMode, DaemonStateSnapshot, GpuTexture, KeyCode, KeyInput, LruCache, Modifiers,
    MouseButton, Page, PickerActionOut, PickerConfig, PickerInput, PickerRenderer, PickerUi,
    TextAtlas, ThumbnailCache, ThumbnailCacheConfig, page_geometry,
};
use glow::HasContext;

fn make_entry(name: &str, dir: &str) -> CatalogEntry {
    let orig = format!("{dir}/{name}");
    CatalogEntry {
        file_name: name.to_string(),
        file_url: format!("file://{orig}"),
        thumbnail_url: format!("file://{orig}"),
        original_path: orig,
        thumbnail_error: None,
    }
}

fn sample_images() -> Vec<CatalogEntry> {
    vec![
        make_entry("alpha_forest.png", "/wallpapers"),
        make_entry("beta_ocean.jpg", "/wallpapers"),
        make_entry("gamma_sunset.webp", "/wallpapers"),
        make_entry("中文壁纸_mountain.png", "/wallpapers"),
    ]
}

fn sample_videos() -> Vec<CatalogEntry> {
    vec![
        make_entry("clip_rain.mp4", "/dynamic"),
        make_entry("clip_neon.mkv", "/dynamic"),
        make_entry("clip_snow.webm", "/dynamic"),
    ]
}

fn key(code: KeyCode) -> PickerInput {
    PickerInput::KeyInput(KeyInput {
        key: code,
        modifiers: Modifiers::default(),
    })
}

fn ctrl_key(ch: char) -> PickerInput {
    PickerInput::KeyInput(KeyInput {
        key: KeyCode::Char(ch),
        modifiers: Modifiers {
            ctrl: true,
            shift: false,
            alt: false,
        },
    })
}

fn setup_open_ui() -> PickerUi {
    let mut ui = PickerUi::new(PickerConfig::default(), 1920.0, 1080.0);
    ui.handle_input(PickerInput::SetDaemonState(DaemonStateSnapshot {
        engine: EngineKind::Full,
        wallpaper_mode: WallpaperMode::Awww,
        video_mode: VideoMode::Normal,
        transition_type: TransitionType::Fade,
        current_wallpaper: "/wallpapers/beta_ocean.jpg".to_string(),
        current_video: "/dynamic/clip_neon.mkv".to_string(),
        busy: false,
        error: String::new(),
    }));
    ui.handle_input(PickerInput::SetCatalog {
        images: sample_images(),
        videos: sample_videos(),
    });
    let actions = ui.handle_input(PickerInput::StartOpen);
    assert!(actions.is_empty(), "both catalogs populated, no RequestCatalog needed");
    ui
}

// ── Unit Tests ────────────────────────────────────────────────────────────────

#[test]
fn wrap_and_standard_navigation() {
    let mut ui = setup_open_ui();
    // Initial focus matches current_wallpaper -> beta_ocean.jpg (index 1)
    assert_eq!(ui.current_index(), 1);

    ui.handle_input(key(KeyCode::Left));
    assert_eq!(ui.current_index(), 0);

    // Wrap mode: Left at 0 wraps to last (3)
    ui.handle_input(key(KeyCode::Left));
    assert_eq!(ui.current_index(), 3);

    // Wrap mode: Right at 3 wraps to 0
    ui.handle_input(key(KeyCode::Right));
    assert_eq!(ui.current_index(), 0);

    // Vim keys h/l & Home/End
    ui.handle_input(key(KeyCode::End));
    assert_eq!(ui.current_index(), 3);
    ui.handle_input(key(KeyCode::Home));
    assert_eq!(ui.current_index(), 0);
    ui.handle_input(key(KeyCode::Char('l')));
    assert_eq!(ui.current_index(), 1);
    ui.handle_input(key(KeyCode::Char('h')));
    assert_eq!(ui.current_index(), 0);

    // Standard mode clamps without wrapping
    ui.config_mut().carousel_mode = CarouselMode::Standard;
    ui.handle_input(key(KeyCode::Left));
    assert_eq!(ui.current_index(), 0);
    ui.handle_input(key(KeyCode::End));
    assert_eq!(ui.current_index(), 3);
    ui.handle_input(key(KeyCode::Right));
    assert_eq!(ui.current_index(), 3);
}

#[test]
fn search_filter_and_escape_cascade() {
    let mut ui = setup_open_ui();
    let mut tick_acts = Vec::new();
    for _ in 0..16 {
        ui.tick(0.016, &mut tick_acts);
    }
    assert!(tick_acts.is_empty());
    assert!(!ui.search_visible());

    // Ctrl+F opens search bar
    ui.handle_input(ctrl_key('f'));
    assert!(ui.search_visible());

    // Case-insensitive substring filter
    ui.handle_input(PickerInput::TextInput("SUN".to_string()));
    assert_eq!(ui.search_text(), "SUN");
    assert_eq!(ui.active_entries().len(), 1);
    assert_eq!(ui.active_entries()[0].file_name, "gamma_sunset.webp");

    // Backspace narrows query back
    ui.handle_input(key(KeyCode::Backspace));
    assert_eq!(ui.search_text(), "SU");
    assert_eq!(ui.active_entries().len(), 1);

    // 1st Escape clears search text but keeps search bar visible
    let acts = ui.handle_input(key(KeyCode::Escape));
    assert!(acts.is_empty());
    assert!(ui.search_visible());
    assert!(ui.search_text().is_empty());
    assert_eq!(ui.active_entries().len(), 4);

    // 2nd Escape closes search bar without closing picker
    let acts = ui.handle_input(key(KeyCode::Escape));
    assert!(acts.is_empty());
    assert!(!ui.search_visible());
    assert!(ui.is_visible());
    assert!(!ui.is_closing());

    // 3rd Escape starts animated panel close, then emits Exit on completion
    let acts = ui.handle_input(key(KeyCode::Escape));
    assert!(acts.is_empty(), "panel close should animate before emitting Exit");
    assert!(ui.is_closing());
    assert!(ui.is_visible());
    for _ in 0..15 {
        ui.tick(0.016, &mut tick_acts);
    }
    assert_eq!(tick_acts, vec![PickerActionOut::Exit]);
    assert!(!ui.is_visible());
    assert!(!ui.is_closing());
}

#[test]
fn dual_page_wheel_switch_preserves_selection_and_does_not_apply() {
    let mut ui = setup_open_ui();
    assert_eq!(ui.page(), Page::Static);
    // Navigate to index 2 (gamma_sunset.webp) on Static page
    ui.handle_input(key(KeyCode::Right));
    assert_eq!(ui.current_entry().unwrap().file_name, "gamma_sunset.webp");

    // Small wheel delta below threshold (40) does not switch yet
    let acts = ui.handle_input(PickerInput::PointerWheel { dx: 0.0, dy: -20.0 });
    assert!(acts.is_empty());
    assert_eq!(ui.page(), Page::Static);

    // Accumulate past threshold -> switches to Video page without applying wallpaper
    let acts = ui.handle_input(PickerInput::PointerWheel { dx: 0.0, dy: -25.0 });
    assert!(acts.is_empty());
    assert_eq!(ui.page(), Page::Video);
    assert!(ui.is_page_animating());
    assert_eq!(ui.current_entry().unwrap().file_name, "clip_neon.mkv");

    // Finish page animation (300ms) + cooldown (180ms)
    let mut tick_acts = Vec::new();
    for _ in 0..40 {
        ui.tick(0.016, &mut tick_acts);
    }
    assert!(tick_acts.is_empty(), "page switch must NOT emit apply action");
    assert!(!ui.is_page_animating());

    // Select clip_snow.webm (index 2) on Video page
    ui.handle_input(key(KeyCode::Right));
    assert_eq!(ui.current_entry().unwrap().file_name, "clip_snow.webm");

    // Scroll up (+50) -> returns to Static page with remembered gamma_sunset.webp!
    let acts = ui.handle_input(PickerInput::PointerWheel { dx: 0.0, dy: 50.0 });
    assert!(acts.is_empty());
    assert_eq!(ui.page(), Page::Static);
    assert_eq!(ui.current_entry().unwrap().file_name, "gamma_sunset.webp");

    // Finish animation + cooldown and switch back to Video -> remembers clip_snow.webm!
    for _ in 0..40 {
        ui.tick(0.016, &mut tick_acts);
    }
    ui.handle_input(PickerInput::PointerWheel { dx: 0.0, dy: -50.0 });
    assert_eq!(ui.page(), Page::Video);
    assert_eq!(ui.current_entry().unwrap().file_name, "clip_snow.webm");
    assert!(tick_acts.is_empty());
}

#[test]
fn page_geometry_matches_carousel_media_js() {
    let g0 = page_geometry(0.0, -1.0, 180.0);
    assert!((g0.outgoing_y - 0.0).abs() < 1e-4);
    assert!((g0.incoming_y - 180.0).abs() < 1e-4);
    assert!((g0.outgoing_opacity - 1.0).abs() < 1e-4);
    assert!((g0.incoming_opacity - 0.0).abs() < 1e-4);

    let g5 = page_geometry(0.5, -1.0, 180.0);
    assert!((g5.outgoing_y - (-90.0)).abs() < 1e-4);
    assert!((g5.incoming_y - 90.0).abs() < 1e-4);
    assert!((g5.outgoing_opacity - 0.5).abs() < 1e-4);
    assert!((g5.incoming_opacity - 0.5).abs() < 1e-4);

    let g1 = page_geometry(1.0, 1.0, 180.0);
    assert!((g1.outgoing_y - 180.0).abs() < 1e-4);
    assert!((g1.incoming_y - 0.0).abs() < 1e-4);
    assert!((g1.outgoing_opacity - 0.0).abs() < 1e-4);
    assert!((g1.incoming_opacity - 1.0).abs() < 1e-4);
}

#[test]
fn hold_expand_after_1_5s_and_neighbor_push_aside() {
    let mut ui = setup_open_ui();
    let mut acts = Vec::new();
    // Settle initial panel open animation (240ms)
    for _ in 0..16 {
        ui.tick(0.016, &mut acts);
    }
    assert!(!ui.is_animating(), "idle before hold should not continuously animate");
    assert!(ui.held_index().is_none());
    assert!(ui.active_hires_path().is_none());

    // Advance remaining time up to 1.5s hold threshold
    for _ in 0..15 {
        ui.tick(0.10, &mut acts);
    }
    assert_eq!(ui.held_index(), Some(1));
    assert_eq!(ui.active_hires_path(), Some("/wallpapers/beta_ocean.jpg"));

    // Complete 400ms InOutCubic expand animation
    for _ in 0..30 {
        ui.tick(0.016, &mut acts);
    }
    assert!(!ui.is_animating(), "should settle to 0% idle GPU after hold expand completes");

    let cards = ui.card_layouts();
    let held = cards.iter().find(|c| c.index == 1).unwrap();
    let left_neighbor = cards.iter().find(|c| c.index == 0).unwrap();
    let right_neighbor = cards.iter().find(|c| c.index == 2).unwrap();

    let expected_w = 1920.0 * 0.35;
    let expected_h = 1080.0 * 0.35;
    let expected_push = (expected_w - 300.0) * 0.5;
    assert!((held.width - expected_w).abs() < 0.5, "held width {}", held.width);
    assert!((held.height - expected_h).abs() < 0.5, "held height {}", held.height);
    assert!((left_neighbor.visual_x_offset - (-expected_push)).abs() < 0.5);
    assert!((right_neighbor.visual_x_offset - expected_push).abs() < 0.5);

    // Moving selection clears hold & hi-res immediately
    ui.handle_input(key(KeyCode::Right));
    assert!(ui.held_index().is_none());
    assert!(ui.active_hires_path().is_none());
}

#[test]
fn confirm_pick_animates_then_emits_apply_and_exit() {
    let mut ui = setup_open_ui();
    let mut acts = Vec::new();
    ui.handle_input(key(KeyCode::Enter));
    assert_eq!(ui.confirming_index(), Some(1));

    // Mid-animation (~150ms): confirmed card scales up toward 1.6, others scale down
    for _ in 0..10 {
        ui.tick(0.015, &mut acts);
    }
    assert!(acts.is_empty());
    let mid_cards = ui.card_layouts();
    let confirmed = mid_cards.iter().find(|c| c.index == 1).unwrap();
    let other = mid_cards.iter().find(|c| c.index == 0).unwrap();
    assert!(confirmed.scale > 1.25, "confirmed should grow toward 1.6, got {}", confirmed.scale);
    assert!(confirmed.opacity < 0.65);
    assert!(other.opacity < 0.65);

    // Finish 300ms confirm timer -> emits ApplyStatic + Exit
    for _ in 0..15 {
        ui.tick(0.015, &mut acts);
    }
    assert_eq!(
        acts,
        vec![
            PickerActionOut::ApplyStatic("/wallpapers/beta_ocean.jpg".to_string()),
            PickerActionOut::Exit,
        ]
    );
    assert!(!ui.is_visible());

    // Video page emits ApplyVideo(mode, path) + Exit
    ui.handle_input(PickerInput::StartOpen);
    ui.handle_input(PickerInput::ShowPage(Page::Video));
    let mut v_acts = Vec::new();
    for _ in 0..40 {
        ui.tick(0.016, &mut v_acts);
    }
    ui.handle_input(key(KeyCode::Enter));
    for _ in 0..25 {
        ui.tick(0.016, &mut v_acts);
    }
    assert_eq!(
        v_acts,
        vec![
            PickerActionOut::ApplyVideo(VideoMode::Normal, "/dynamic/clip_neon.mkv".to_string()),
            PickerActionOut::Exit,
        ]
    );
}

#[test]
fn skew_selector_hit_testing_and_actions() {
    let mut ui = setup_open_ui();
    let sel = ui.selector_layout();
    let skew = ui.config().skew_factor; // -0.35

    // Mode button 1 ("视差"): click near top-left skewed corner vs unskewed top-left
    let parallax_btn = &sel.mode_buttons[1];
    let dy = -parallax_btn.height * 0.45; // near top edge
    // Centered skew shifts top edge by skew * dy = (-0.35)*(-17.1) ≈ +6.0px right
    let inside_top_right_x = parallax_btn.center_x + parallax_btn.width * 0.48 + skew * dy;
    let inside_top_y = parallax_btn.center_y + dy;
    assert_eq!(
        ui.hit_test_mode_button(inside_top_right_x, inside_top_y),
        Some(1)
    );
    // Just outside the slanted left edge at the top (which shifted right by +6px)
    let outside_top_left_x = parallax_btn.center_x - parallax_btn.width * 0.49;
    assert_eq!(
        ui.hit_test_mode_button(outside_top_left_x, inside_top_y),
        None
    );

    // Clicking unselected mode button emits ToggleMode
    let acts = ui.handle_input(PickerInput::PointerClick {
        x: parallax_btn.center_x,
        y: parallax_btn.center_y,
        button: MouseButton::Left,
    });
    assert_eq!(acts, vec![PickerActionOut::ToggleMode]);

    // Clicking transition button 2 ("波浪" -> Wave) emits SelectTransition(Wave)
    let wave_btn = &sel.transition_buttons[2];
    assert_eq!(wave_btn.label, "波浪");
    assert_eq!(
        ui.hit_test_transition_button(wave_btn.center_x, wave_btn.center_y),
        Some(2)
    );
    let acts = ui.handle_input(PickerInput::PointerClick {
        x: wave_btn.center_x,
        y: wave_btn.center_y,
        button: MouseButton::Left,
    });
    assert_eq!(
        acts,
        vec![PickerActionOut::SelectTransition(TransitionType::Wave)]
    );

    // Clicking empty backdrop scrim starts animated panel close -> Exit on completion
    let mut tick_acts = Vec::new();
    for _ in 0..16 {
        ui.tick(0.016, &mut tick_acts);
    }
    let acts = ui.handle_input(PickerInput::PointerClick {
        x: 10.0,
        y: 10.0,
        button: MouseButton::Left,
    });
    assert!(acts.is_empty());
    assert!(ui.is_closing());
    for _ in 0..15 {
        ui.tick(0.016, &mut tick_acts);
    }
    assert_eq!(tick_acts, vec![PickerActionOut::Exit]);
    assert!(!ui.is_visible());
    assert!(!ui.is_closing());
}

#[test]
fn lru_cache_strictly_bounds_capacity_and_refreshes_recency() {
    let mut cache: LruCache<String, u32> = LruCache::new(24);
    for i in 0..24 {
        assert!(cache.insert(format!("k{i}"), i).is_empty());
    }
    assert_eq!(cache.len(), 24);

    // Touch k0 so k1 becomes the least-recently-used key
    assert_eq!(cache.get("k0"), Some(&0));

    let evicted = cache.insert("k24".to_string(), 24);
    assert_eq!(evicted, vec![("k1".to_string(), 1)]);
    assert_eq!(cache.len(), 24);
    assert!(cache.contains_key("k0"));
    assert!(!cache.contains_key("k1"));
    assert!(cache.contains_key("k24"));
}

#[test]
fn text_atlas_lazy_init_is_fast_and_bounded_to_latin_plus_cjk() {
    let t0 = Instant::now();
    let mut atlas = TextAtlas::new_system();
    let init_ms = t0.elapsed().as_millis();
    assert!(
        (1..=2).contains(&atlas.font_count()),
        "must load at most 1 Latin + 1 CJK font, got {}",
        atlas.font_count()
    );
    assert!(
        init_ms < 250,
        "lazy swash TextAtlas::new_system() should initialize quickly, took {init_ms}ms"
    );
    let t1 = Instant::now();
    for label in ["普通", "视差", "淡化", "斜扫", "波浪", "圆形展开", "圆形收拢", "随机", "直接"] {
        let layout = atlas.layout_text(label, 13.0, 1.5, 1.0);
        assert!(layout.width > 0.0 && !layout.quads.is_empty());
    }
    let layout_ms = t1.elapsed().as_millis();
    assert!(
        layout_ms < 150,
        "on-demand CJK rasterization should stay fast, took {layout_ms}ms"
    );
}

// ── Headless EGL GPU Tests ────────────────────────────────────────────────────

struct OffscreenFbo {
    fbo: glow::Framebuffer,
    tex: glow::Texture,
    width: u32,
    height: u32,
}

impl OffscreenFbo {
    fn new(gl: &glow::Context, width: u32, height: u32) -> Self {
        unsafe {
            let tex = gl.create_texture().expect("create fbo tex");
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

    fn read_rgba_top_down(&self, gl: &glow::Context) -> Vec<u8> {
        let mut raw = vec![0u8; (self.width * self.height * 4) as usize];
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.read_pixels(
                0,
                0,
                self.width as i32,
                self.height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut raw)),
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        // Flip rows so row 0 is top of viewport
        let stride = (self.width * 4) as usize;
        let mut flipped = vec![0u8; raw.len()];
        for y in 0..self.height as usize {
            let src_y = (self.height as usize - 1) - y;
            flipped[y * stride..(y + 1) * stride]
                .copy_from_slice(&raw[src_y * stride..(src_y + 1) * stride]);
        }
        flipped
    }

    fn pixel_at(&self, img: &[u8], x: u32, y: u32) -> [u8; 4] {
        let idx = ((y * self.width + x) * 4) as usize;
        [img[idx], img[idx + 1], img[idx + 2], img[idx + 3]]
    }

    fn destroy(self, gl: &glow::Context) {
        unsafe {
            gl.delete_framebuffer(self.fbo);
            gl.delete_texture(self.tex);
        }
    }
}

fn create_solid_texture(gl: &glow::Context, w: u32, h: u32, rgba: [u8; 4]) -> GpuTexture {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        data.extend_from_slice(&rgba);
    }
    unsafe {
        let tex = gl.create_texture().expect("create solid tex");
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w as i32,
            h as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&data)),
        );
        gl.bind_texture(glow::TEXTURE_2D, None);
        GpuTexture {
            texture: tex,
            width: w,
            height: h,
        }
    }
}

#[test]
fn headless_gpu_single_pass_skewed_card_cjk_atlas_and_teardown() {
    let ctx = EglContext::new_headless().expect("headless EGL context");
    let gl = Arc::clone(&ctx.gl);
    let mut renderer = PickerRenderer::new(Arc::clone(&gl), ctx.is_gles)
        .expect("compile picker GLES/GL shaders");
    let mut text = TextAtlas::new_system();
    let mut thumbs = ThumbnailCache::new(ThumbnailCacheConfig {
        max_thumbnails: 4,
        visible_radius: 2,
        worker_threads: 2,
        ..ThumbnailCacheConfig::default()
    });

    // Verify at most 1 Latin + 1 CJK font is loaded and CJK rasterization works at 1.5x HiDPI
    assert!(
        (1..=2).contains(&text.font_count()),
        "expected at most 1 Latin + 1 CJK font, got {}",
        text.font_count()
    );
    for label in [
        "Search wallpapers...",
        "普通",
        "视差",
        "淡化",
        "斜扫",
        "波浪",
        "圆形展开",
        "圆形收拢",
        "随机",
        "直接",
    ] {
        let layout = text.layout_text(label, 13.0, 1.5, 1.0);
        assert!(
            layout.width > 0.0 && !layout.quads.is_empty(),
            "label '{label}' must produce rasterized quads at 1.5x DPR"
        );
    }
    let nonzero_atlas_pixels = text.atlas_pixels().iter().filter(|&&b| b > 16).count();
    assert!(
        nonzero_atlas_pixels > 200,
        "R8 glyph atlas should contain rasterized CJK coverage pixels, got {nonzero_atlas_pixels}"
    );

    // Prepare a single card with transparent scrim (overlay_opacity = 0) so we can verify
    // exact rounded-corner & skewed-parallelogram alpha cutouts!
    let cfg = PickerConfig {
        overlay_opacity: 0.0,
        corner_radius: 24.0,
        enable_hold_expand: false,
        ..PickerConfig::default()
    };
    let mut ui = PickerUi::new(cfg, 1280.0, 720.0);
    ui.handle_input(PickerInput::SetCatalog {
        images: vec![CatalogEntry {
            file_name: "card.png".to_string(),
            file_url: "file:///virtual/card.png".to_string(),
            thumbnail_url: "file:///virtual/card.png".to_string(),
            original_path: "/virtual/card.png".to_string(),
            thumbnail_error: None,
        }],
        videos: vec![],
    });
    ui.handle_input(PickerInput::StartOpen);
    let mut open_acts = Vec::new();
    for _ in 0..16 {
        ui.tick(0.016, &mut open_acts);
    }
    assert!(open_acts.is_empty());
    assert!((ui.panel_progress() - 1.0).abs() < 1e-4);

    let green_tex = create_solid_texture(&gl, 64, 64, [30, 220, 90, 255]);
    thumbs.insert_thumbnail_gpu(&gl, "/virtual/card.png", green_tex);

    let fbo = OffscreenFbo::new(&gl, 1280, 720);
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo.fbo));
    }
    renderer.render(&mut ui, &mut thumbs, &mut text, (1280, 720), 1.0);
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
    }

    let img = fbo.read_rgba_top_down(&gl);

    // 1. Center of card (640, 360) must be bright green and opaque
    let center = fbo.pixel_at(&img, 640, 360);
    assert!(
        center[1] > 180 && center[3] > 240,
        "center pixel should be solid green card texture, got {center:?}"
    );

    // 2. Just inside the skewed top-left bounding box corner (which is cut away by radius=24):
    // Card scaled size: w = 300 * 1.08 = 324, h = 420 * 1.08 = 453.6
    // Local top-left corner is (-162, -226.8), skewed screen pos = (640 - 162 + (-0.35)*(-226.8), 360 - 226.8)
    // = (557.4, 133.2). Because corner_radius * scale = 25.92, that exact corner pixel MUST be transparent!
    let rounded_corner_outside = fbo.pixel_at(&img, 558, 134);
    assert_eq!(
        rounded_corner_outside[3], 0,
        "pixel outside SDF rounded corner must be transparent, got {rounded_corner_outside:?}"
    );

    // 3. Far outside card on left (100, 360) must be transparent (scrim = 0)
    let outside = fbo.pixel_at(&img, 100, 360);
    assert_eq!(
        outside,
        [0, 0, 0, 0],
        "background outside card should be transparent, got {outside:?}"
    );

    // 4. Verify bottom SkewSelector rendered non-empty pixels (buttons + Chinese text)
    let sel = ui.selector_layout();
    let mut selector_lit_pixels = 0usize;
    let sy0 = (sel.mode_pill_center_y - 20.0).max(0.0) as u32;
    let sy1 = (sel.mode_pill_center_y + 20.0).min(719.0) as u32;
    let sx0 = (sel.mode_buttons[0].center_x - 60.0).max(0.0) as u32;
    let sx1 = (sel.transition_buttons.last().unwrap().center_x + 40.0).min(1279.0) as u32;
    for y in sy0..=sy1 {
        for x in sx0..=sx1 {
            if fbo.pixel_at(&img, x, y)[3] > 20 {
                selector_lit_pixels += 1;
            }
        }
    }
    assert!(
        selector_lit_pixels > 500,
        "SkewSelector + Chinese labels should render visible pixels, got {selector_lit_pixels}"
    );

    // 5. Async real-file thumbnail + hi-res decode & immediate release on unhold
    let tmp_dir = std::env::temp_dir().join(format!("bgpicker_picker_test_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);
    let thumb_path = tmp_dir.join("thumb.png");
    let orig_path = tmp_dir.join("orig.png");
    let thumb_img = image::RgbaImage::from_pixel(64, 64, image::Rgba([220, 40, 40, 255]));
    let orig_img = image::RgbaImage::from_pixel(128, 128, image::Rgba([40, 90, 240, 255]));
    thumb_img.save(&thumb_path).unwrap();
    orig_img.save(&orig_path).unwrap();

    thumbs.clear_all_gpu(&gl);
    assert_eq!(thumbs.thumbnail_count(), 0);
    assert!(!thumbs.has_hires_texture());

    let thumb_str = thumb_path.to_string_lossy().into_owned();
    let orig_str = orig_path.to_string_lossy().into_owned();
    let entry = bgpicker_picker::MediaEntry {
        file_name: "orig.png".to_string(),
        thumbnail_path: thumb_str.clone(),
        original_path: orig_str.clone(),
    };
    thumbs.sync_visible_window(&[entry], 0);
    thumbs.sync_hires(&gl, Some(&orig_str), 1280);

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        thumbs.upload_ready(&gl, 4);
        if thumbs.get_thumbnail(&thumb_str).is_some() && thumbs.get_hires(&orig_str).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(thumbs.get_thumbnail(&thumb_str).is_some(), "async thumb should decode & upload");
    assert!(thumbs.get_hires(&orig_str).is_some(), "async hi-res should decode & upload");

    // Unhold immediately deletes hi-res GPU texture
    thumbs.sync_hires(&gl, None, 1280);
    assert!(!thumbs.has_hires_texture());

    let _ = std::fs::remove_dir_all(&tmp_dir);

    // 6. Clean GL resource teardown & zero GL errors
    thumbs.clear_all_gpu(&gl);
    text.destroy_gpu(&gl);
    fbo.destroy(&gl);
    renderer.destroy();
    let gl_err = unsafe { gl.get_error() };
    assert_eq!(gl_err, glow::NO_ERROR, "unexpected GL error after teardown: 0x{gl_err:x}");
}

#[test]
fn card_hover_boost_and_search_box_click_do_not_close() {
    let mut ui = setup_open_ui();
    let mut acts = Vec::new();
    for _ in 0..16 {
        ui.tick(0.016, &mut acts);
    }

    // Hover center card (index 1 at 960, 540)
    ui.handle_input(PickerInput::PointerMove { x: 960.0, y: 540.0 });
    assert_eq!(ui.hovered_card(), Some(1));
    for _ in 0..25 {
        ui.tick(0.016, &mut acts);
    }
    let cards = ui.card_layouts();
    let hovered = cards.iter().find(|c| c.index == 1).unwrap();
    // Base selected_scale (1.08) + hover boost (+0.20 * 1.0) = 1.28
    assert!(
        (hovered.scale - 1.28).abs() < 0.02,
        "hovered center card scale should reach ~1.28, got {}",
        hovered.scale
    );
    assert!((hovered.opacity - 1.0).abs() < 0.01);

    // Clicking inside open search box must NOT close the picker
    ui.handle_input(ctrl_key('f'));
    let sb = ui.search_box_rect();
    assert!(ui.hit_test_search_box(sb.x + 20.0, sb.y + 20.0));
    let click_acts = ui.handle_input(PickerInput::PointerClick {
        x: sb.x + 20.0,
        y: sb.y + 20.0,
        button: MouseButton::Left,
    });
    assert!(click_acts.is_empty());
    assert!(ui.is_visible());
}

#[test]
fn headless_gpu_hires_crossfade_and_bounded_eviction() {
    let ctx = EglContext::new_headless().expect("headless EGL context");
    let gl = Arc::clone(&ctx.gl);
    let mut renderer = PickerRenderer::new(Arc::clone(&gl), ctx.is_gles)
        .expect("compile picker shaders");
    let mut text = TextAtlas::new_system();
    let mut thumbs = ThumbnailCache::new(ThumbnailCacheConfig {
        max_thumbnails: 2,
        visible_radius: 2,
        worker_threads: 2,
        ..ThumbnailCacheConfig::default()
    });

    // Insert 3 GPU textures into a capacity-2 cache -> strictly bounded to 2 with clean eviction
    let t1 = create_solid_texture(&gl, 16, 16, [255, 0, 0, 255]);
    let t2 = create_solid_texture(&gl, 16, 16, [0, 255, 0, 255]);
    let t3 = create_solid_texture(&gl, 16, 16, [0, 0, 255, 255]);
    thumbs.insert_thumbnail_gpu(&gl, "a", t1);
    thumbs.insert_thumbnail_gpu(&gl, "b", t2);
    thumbs.insert_thumbnail_gpu(&gl, "c", t3);
    assert_eq!(thumbs.thumbnail_count(), 2);
    assert!(thumbs.get_thumbnail("a").is_none());
    assert!(thumbs.get_thumbnail("b").is_some());
    assert!(thumbs.get_thumbnail("c").is_some());

    // Render with real temp files: red thumbnail -> 1.5s hold -> blue hi-res cross-fade!
    let tmp_dir = std::env::temp_dir().join(format!("bgpicker_hires_test_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);
    let thumb_path = tmp_dir.join("red_thumb.png");
    let hires_path = tmp_dir.join("blue_orig.png");
    image::RgbaImage::from_pixel(64, 64, image::Rgba([230, 30, 30, 255]))
        .save(&thumb_path)
        .unwrap();
    image::RgbaImage::from_pixel(128, 128, image::Rgba([30, 60, 240, 255]))
        .save(&hires_path)
        .unwrap();

    let thumb_str = thumb_path.to_string_lossy().into_owned();
    let hires_str = hires_path.to_string_lossy().into_owned();
    let cfg = PickerConfig {
        overlay_opacity: 0.0,
        enable_hold_expand: true,
        hold_delay_s: 1.5,
        ..PickerConfig::default()
    };
    let mut ui = PickerUi::new(cfg, 1280.0, 720.0);
    ui.handle_input(PickerInput::SetCatalog {
        images: vec![CatalogEntry {
            file_name: "blue_orig.png".to_string(),
            file_url: format!("file://{hires_str}"),
            thumbnail_url: format!("file://{thumb_str}"),
            original_path: hires_str.clone(),
            thumbnail_error: None,
        }],
        videos: vec![],
    });
    ui.handle_input(PickerInput::StartOpen);

    // Advance 1.6s to trigger hold expand & hi-res request
    let mut acts = Vec::new();
    for _ in 0..16 {
        ui.tick(0.10, &mut acts);
    }
    assert_eq!(ui.held_index(), Some(0));

    let fbo = OffscreenFbo::new(&gl, 1280, 720);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo.fbo)) };
        renderer.render(&mut ui, &mut thumbs, &mut text, (1280, 720), 1.0);
        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, None) };
        if thumbs.get_thumbnail(&thumb_str).is_some() && thumbs.get_hires(&hires_str).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(thumbs.get_hires(&hires_str).is_some());

    // Advance 350ms so 300ms InOutQuad hi-res cross-fade reaches 1.0, then re-render
    for _ in 0..25 {
        ui.tick(0.016, &mut acts);
    }
    unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo.fbo)) };
    renderer.render(&mut ui, &mut thumbs, &mut text, (1280, 720), 1.0);
    unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, None) };

    let img = fbo.read_rgba_top_down(&gl);
    let center = fbo.pixel_at(&img, 640, 360);
    assert!(
        center[2] > 200 && center[0] < 80,
        "center pixel after hi-res cross-fade should be blue hi-res texture, got {center:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp_dir);
    thumbs.clear_all_gpu(&gl);
    text.destroy_gpu(&gl);
    fbo.destroy(&gl);
    renderer.destroy();
    assert_eq!(unsafe { gl.get_error() }, glow::NO_ERROR);
}

#[test]
fn mouse_horizontal_drag_scrolls_snaps_and_never_picks_or_exits() {
    let mut ui = setup_open_ui();
    let mut acts = Vec::new();
    for _ in 0..16 {
        ui.tick(0.016, &mut acts);
    }
    assert_eq!(ui.current_index(), 1);
    assert!((ui.scroll_pos() - 1.0).abs() < 1e-4);

    // Press left mouse button over center card (960, 540)
    let press_acts = ui.handle_input(PickerInput::PointerPress {
        x: 960.0,
        y: 540.0,
        button: MouseButton::Left,
    });
    assert!(press_acts.is_empty());
    assert!(!ui.is_dragging());

    // Drag left by 300px (1 full card step) in 16ms increments
    let mut x = 960.0;
    for step_idx in 0..20 {
        x -= 15.0;
        let move_acts = ui.handle_input(PickerInput::PointerMove { x, y: 540.0 });
        assert!(move_acts.is_empty());
        ui.tick(0.016, &mut acts);
        if step_idx >= 1 {
            assert!(ui.is_dragging(), "should enter active drag past 6px threshold");
        }
    }
    assert!(
        (ui.scroll_pos() - 2.0).abs() < 0.05,
        "dragging -300px should move scroll_pos from 1.0 to ~2.0, got {}",
        ui.scroll_pos()
    );

    // Pause mid-drag for >1.6s: hold timer must stay cancelled and velocity decays to 0
    for _ in 0..20 {
        ui.tick(0.08, &mut acts);
    }
    assert!(ui.held_index().is_none(), "dragging must never trigger hold expand");
    assert!(acts.is_empty());

    // Release left mouse button -> snaps cleanly to card 2, NEVER picks or closes
    let rel_acts = ui.handle_input(PickerInput::PointerRelease {
        x,
        y: 540.0,
        button: MouseButton::Left,
    });
    assert!(rel_acts.is_empty(), "drag release must not emit apply/exit");
    assert!(!ui.is_dragging());
    assert!(ui.confirming_index().is_none());
    assert!(ui.is_visible());

    for _ in 0..35 {
        ui.tick(0.016, &mut acts);
    }
    assert!(acts.is_empty(), "drag snap completion must not emit apply/exit");
    assert_eq!(ui.current_index(), 2);
    assert!((ui.scroll_pos() - 2.0).abs() < 1e-3);

    // Fast flick left from card 2 -> advances to card 3 via gentle flick momentum
    ui.handle_input(PickerInput::PointerPress {
        x: 960.0,
        y: 540.0,
        button: MouseButton::Left,
    });
    for i in 1..=5 {
        ui.tick(0.012, &mut acts);
        ui.handle_input(PickerInput::PointerMove {
            x: 960.0 - (i as f32) * 24.0, // -120px in 60ms (~-2000 px/s)
            y: 540.0,
        });
    }
    let flick_rel_acts = ui.handle_input(PickerInput::PointerRelease {
        x: 840.0,
        y: 540.0,
        button: MouseButton::Left,
    });
    assert!(flick_rel_acts.is_empty());
    for _ in 0..40 {
        ui.tick(0.016, &mut acts);
    }
    assert!(acts.is_empty());
    assert_eq!(ui.current_index(), 3, "firm flick left should advance to card 3");

    // Pressing on bottom SkewSelector and moving horizontally must NOT start carousel drag
    let sel = ui.selector_layout();
    let btn = &sel.mode_buttons[1];
    ui.handle_input(PickerInput::PointerPress {
        x: btn.center_x,
        y: btn.center_y,
        button: MouseButton::Left,
    });
    ui.handle_input(PickerInput::PointerMove {
        x: btn.center_x - 200.0,
        y: btn.center_y - 100.0,
    });
    assert!(!ui.is_dragging(), "pressing on SkewSelector must not drag carousel");
    let ctrl_rel = ui.handle_input(PickerInput::PointerRelease {
        x: btn.center_x - 200.0,
        y: btn.center_y - 100.0,
        button: MouseButton::Left,
    });
    assert!(ctrl_rel.is_empty(), "releasing off control must not close picker");
    assert!(ui.is_visible());

    // Edge case 1: PointerLeave mid-drag on last index (3) with small drag must NOT falsely wrap to 0
    assert_eq!(ui.current_index(), 3);
    ui.handle_input(PickerInput::PointerPress {
        x: 960.0,
        y: 540.0,
        button: MouseButton::Left,
    });
    ui.handle_input(PickerInput::PointerMove { x: 930.0, y: 540.0 });
    assert!(ui.is_dragging());
    let leave_acts = ui.handle_input(PickerInput::PointerLeave);
    assert!(leave_acts.is_empty());
    assert!(!ui.is_dragging());
    for _ in 0..30 {
        ui.tick(0.016, &mut acts);
    }
    assert_eq!(
        ui.current_index(),
        3,
        "PointerLeave after small drag on last card must snap to 3, not falsely wrap to 0"
    );

    // Edge case 2: Press while scroll_index is animating, let scroll finish before crossing drag threshold
    ui.handle_input(key(KeyCode::Left)); // 3 -> 2 animating
    ui.tick(0.032, &mut acts); // mid-scroll (~2.6)
    ui.handle_input(PickerInput::PointerPress {
        x: 960.0,
        y: 540.0,
        button: MouseButton::Left,
    });
    // Let scroll finish to 2.0 before moving past drag threshold
    for _ in 0..25 {
        ui.tick(0.016, &mut acts);
    }
    assert!((ui.scroll_pos() - 2.0).abs() < 1e-3);
    ui.handle_input(PickerInput::PointerMove { x: 950.0, y: 540.0 }); // crosses 6px threshold, re-anchors at 2.0
    assert!(ui.is_dragging());
    assert!(
        (ui.scroll_pos() - 2.0).abs() < 0.05,
        "drag must re-anchor at current scroll_pos (2.0), not jump back to stale press scroll, got {}",
        ui.scroll_pos()
    );
    ui.handle_input(PickerInput::PointerRelease {
        x: 950.0,
        y: 540.0,
        button: MouseButton::Left,
    });
}

#[test]
fn short_left_press_release_without_drag_confirms_pick_and_scrim_closes() {
    let mut ui = setup_open_ui();
    let mut acts = Vec::new();
    for _ in 0..16 {
        ui.tick(0.016, &mut acts);
    }

    // Press and release on center card with tiny 2px jitter (< 6px threshold) -> confirms pick!
    assert!(
        ui.handle_input(PickerInput::PointerPress {
            x: 960.0,
            y: 540.0,
            button: MouseButton::Left,
        })
        .is_empty()
    );
    ui.handle_input(PickerInput::PointerMove { x: 962.0, y: 541.0 });
    assert!(!ui.is_dragging());
    let rel_acts = ui.handle_input(PickerInput::PointerRelease {
        x: 962.0,
        y: 541.0,
        button: MouseButton::Left,
    });
    assert!(rel_acts.is_empty());
    assert_eq!(ui.confirming_index(), Some(1));

    for _ in 0..25 {
        ui.tick(0.016, &mut acts);
    }
    assert_eq!(
        acts,
        vec![
            PickerActionOut::ApplyStatic("/wallpapers/beta_ocean.jpg".to_string()),
            PickerActionOut::Exit,
        ]
    );
    assert!(!ui.is_visible());

    // Re-open, settle, and click empty scrim without dragging -> smoothly closes picker
    ui.handle_input(PickerInput::StartOpen);
    assert!(ui.is_visible());
    let mut close_tick_acts = Vec::new();
    for _ in 0..16 {
        ui.tick(0.016, &mut close_tick_acts);
    }
    assert!(close_tick_acts.is_empty());
    ui.handle_input(PickerInput::PointerPress {
        x: 10.0,
        y: 10.0,
        button: MouseButton::Left,
    });
    let close_acts = ui.handle_input(PickerInput::PointerRelease {
        x: 11.0,
        y: 10.0,
        button: MouseButton::Left,
    });
    assert!(close_acts.is_empty());
    assert!(ui.is_closing());
    for _ in 0..15 {
        ui.tick(0.016, &mut close_tick_acts);
    }
    assert_eq!(close_tick_acts, vec![PickerActionOut::Exit]);
    assert!(!ui.is_visible());
    assert!(!ui.is_closing());
}

#[test]
fn hover_while_switching_unexpanded_has_continuous_scale_without_jumps() {
    let mut ui = setup_open_ui();
    let mut acts = Vec::new();
    for _ in 0..16 {
        ui.tick(0.016, &mut acts);
    }
    assert!(ui.held_index().is_none());

    // Helper to record scale per card index
    let snapshot_scales = |u: &PickerUi| -> Vec<(usize, f32, f32, f32, f32)> {
        let mut v: Vec<_> = u
            .card_layouts()
            .into_iter()
            .map(|c| (c.index, c.scale, c.opacity, c.width, c.height))
            .collect();
        v.sort_by_key(|x| x.0);
        v
    };

    // 1. Hover center card (1) and let hover animation settle (unexpanded)
    ui.handle_input(PickerInput::PointerMove { x: 960.0, y: 540.0 });
    let mut prev = snapshot_scales(&ui);
    for _ in 0..35 {
        ui.tick(0.016, &mut acts);
        let next = snapshot_scales(&ui);
        for (a, b) in prev.iter().zip(next.iter()) {
            assert!(
                (b.1 - a.1).abs() < 0.045,
                "card {} scale jumped abruptly on hover enter: {} -> {}",
                a.0,
                a.1,
                b.1
            );
        }
        prev = next;
    }
    assert_eq!(ui.hovered_card(), Some(1));
    assert!(ui.held_index().is_none());

    // 2. Switch Right (1 -> 2), then Right (2 -> 3), then Left (3 -> 2) with pointer stationary at center
    for step_key in [KeyCode::Right, KeyCode::Right, KeyCode::Left] {
        ui.handle_input(key(step_key));
        for _ in 0..40 {
            ui.tick(0.016, &mut acts);
            let next = snapshot_scales(&ui);
            for (a, b) in prev.iter().zip(next.iter()) {
                assert!(
                    (b.1 - a.1).abs() < 0.045,
                    "card {} scale jumped abruptly while switching ({step_key:?}): {} -> {}",
                    a.0,
                    a.1,
                    b.1
                );
                assert!(
                    (b.2 - a.2).abs() < 0.08,
                    "card {} opacity jumped abruptly while switching ({step_key:?}): {} -> {}",
                    a.0,
                    a.2,
                    b.2
                );
                assert!((b.3 - 300.0).abs() < 1e-3 && (b.4 - 420.0).abs() < 1e-3);
            }
            prev = next;
        }
    }
    assert_eq!(ui.current_index(), 2);
    // Stationary pointer at center must now hover the newly centered card (2)
    assert_eq!(ui.hovered_card(), Some(2));
    assert!(
        (prev[2].1 - 1.28).abs() < 0.02,
        "hovered center card 2 should settle smoothly at ~1.28, got {}",
        prev[2].1
    );
    assert!(!ui.is_animating(), "springs must settle cleanly to 0% idle GPU");

    // 3. Hover right neighbor card (3 at ~1260, 540), then switch Right (2 -> 3)
    ui.handle_input(PickerInput::PointerMove { x: 1260.0, y: 540.0 });
    assert_eq!(ui.hovered_card(), Some(3));
    for _ in 0..10 {
        ui.tick(0.016, &mut acts);
        prev = snapshot_scales(&ui);
    }
    ui.handle_input(key(KeyCode::Right));
    for _ in 0..35 {
        ui.tick(0.016, &mut acts);
        let next = snapshot_scales(&ui);
        for (a, b) in prev.iter().zip(next.iter()) {
            assert!(
                (b.1 - a.1).abs() < 0.045,
                "card {} scale jumped abruptly when hovered neighbor switched to center: {} -> {}",
                a.0,
                a.1,
                b.1
            );
        }
        prev = next;
    }
    assert_eq!(ui.current_index(), 3);
    assert!(ui.held_index().is_none());
}

#[test]
fn video_engine_opens_on_video_page_syncs_pill_and_toggles_mode() {
    // 1. Fresh picker with EngineKind::Video & VideoMode::Parallax (while static wallpaper_mode is Awww):
    //    SetDaemonState -> StartOpen -> SetCatalog (exact on-demand startup order).
    let mut ui = PickerUi::new(PickerConfig::default(), 1920.0, 1080.0);
    let snap = DaemonStateSnapshot {
        engine: EngineKind::Video,
        wallpaper_mode: WallpaperMode::Awww,
        video_mode: VideoMode::Parallax,
        transition_type: TransitionType::Fade,
        current_wallpaper: "/wallpapers/beta_ocean.jpg".to_string(),
        current_video: "/dynamic/clip_neon.mkv".to_string(),
        busy: false,
        error: String::new(),
    };
    ui.handle_input(PickerInput::SetDaemonState(snap.clone()));

    let open_acts = ui.handle_input(PickerInput::StartOpen);
    assert_eq!(open_acts, vec![PickerActionOut::RequestCatalog]);
    assert_eq!(ui.page(), Page::Video);

    // Mode pill must snap immediately to video_mode (Parallax = index 1), NOT wallpaper_mode (0)
    let sel = ui.selector_layout();
    assert!(sel.mode_available);
    assert!(!sel.mode_buttons[0].selected);
    assert!(sel.mode_buttons[1].selected);
    assert!(
        (sel.mode_pill_center_x - sel.mode_buttons[1].center_x).abs() < 0.5,
        "mode pill should snap to 视差 (index 1), got pill_x={} vs btn1_x={}",
        sel.mode_pill_center_x,
        sel.mode_buttons[1].center_x
    );

    // Once async catalog arrives, current_video (clip_neon.mkv, index 1) is selected on Page::Video
    let cat_acts = ui.handle_input(PickerInput::SetCatalog {
        images: sample_images(),
        videos: sample_videos(),
    });
    assert!(cat_acts.is_empty());
    assert_eq!(ui.page(), Page::Video);
    assert_eq!(ui.current_index(), 1);
    assert_eq!(ui.current_entry().unwrap().file_name, "clip_neon.mkv");

    // Clicking the already-selected mode button ("视差") is a no-op
    let noop = ui.handle_input(PickerInput::PointerClick {
        x: sel.mode_buttons[1].center_x,
        y: sel.mode_buttons[1].center_y,
        button: MouseButton::Left,
    });
    assert!(noop.is_empty());

    // Clicking the opposite mode button ("普通") emits ToggleMode while staying on Page::Video
    let toggle_acts = ui.handle_input(PickerInput::PointerClick {
        x: sel.mode_buttons[0].center_x,
        y: sel.mode_buttons[0].center_y,
        button: MouseButton::Left,
    });
    assert_eq!(toggle_acts, vec![PickerActionOut::ToggleMode]);
    assert_eq!(ui.page(), Page::Video);

    // While daemon is busy (after wayland sets local busy), duplicate clicks are ignored
    let mut busy_snap = snap.clone();
    busy_snap.busy = true;
    ui.handle_input(PickerInput::SetDaemonState(busy_snap));
    let dup_acts = ui.handle_input(PickerInput::PointerClick {
        x: sel.mode_buttons[0].center_x,
        y: sel.mode_buttons[0].center_y,
        button: MouseButton::Left,
    });
    assert!(dup_acts.is_empty(), "clicks while busy must not double-toggle");

    // 2. VideoMode::Normal with WallpaperMode::Parallax snaps pill to index 0 ("普通")
    let mut ui_normal = PickerUi::new(PickerConfig::default(), 1920.0, 1080.0);
    ui_normal.handle_input(PickerInput::SetDaemonState(DaemonStateSnapshot {
        engine: EngineKind::Video,
        wallpaper_mode: WallpaperMode::Parallax,
        video_mode: VideoMode::Normal,
        transition_type: TransitionType::Wave,
        current_wallpaper: "/wallpapers/alpha_forest.png".to_string(),
        current_video: "/dynamic/clip_snow.webm".to_string(),
        busy: false,
        error: String::new(),
    }));
    ui_normal.handle_input(PickerInput::StartOpen);
    ui_normal.handle_input(PickerInput::SetCatalog {
        images: sample_images(),
        videos: sample_videos(),
    });
    assert_eq!(ui_normal.page(), Page::Video);
    assert_eq!(ui_normal.current_entry().unwrap().file_name, "clip_snow.webm");
    let sel_n = ui_normal.selector_layout();
    assert!(sel_n.mode_buttons[0].selected);
    assert!(!sel_n.mode_buttons[1].selected);
    assert!((sel_n.mode_pill_center_x - sel_n.mode_buttons[0].center_x).abs() < 0.5);
    let to_parallax = ui_normal.handle_input(PickerInput::PointerClick {
        x: sel_n.mode_buttons[1].center_x,
        y: sel_n.mode_buttons[1].center_y,
        button: MouseButton::Left,
    });
    assert_eq!(to_parallax, vec![PickerActionOut::ToggleMode]);

    // 3. Opening via CycleNext / CyclePrevious when closed also lands on Page::Video
    for trigger in [PickerInput::CycleNext, PickerInput::CyclePrevious] {
        let mut ui_cycle = PickerUi::new(PickerConfig::default(), 1920.0, 1080.0);
        ui_cycle.handle_input(PickerInput::SetDaemonState(snap.clone()));
        ui_cycle.handle_input(trigger);
        assert!(ui_cycle.is_visible());
        assert_eq!(ui_cycle.page(), Page::Video);
    }
}

#[test]
fn static_engines_open_on_static_page_and_daemon_state_does_not_yank_manual_page_switch() {
    // EngineKind::Full and EngineKind::Simple open on Page::Static with wallpaper_mode pill
    for engine in [EngineKind::Full, EngineKind::Simple] {
        let mut ui = PickerUi::new(PickerConfig::default(), 1920.0, 1080.0);
        ui.handle_input(PickerInput::SetDaemonState(DaemonStateSnapshot {
            engine,
            wallpaper_mode: WallpaperMode::Parallax,
            video_mode: VideoMode::Normal,
            transition_type: TransitionType::Fade,
            current_wallpaper: "/wallpapers/gamma_sunset.webp".to_string(),
            current_video: "/dynamic/clip_rain.mp4".to_string(),
            busy: false,
            error: String::new(),
        }));
        ui.handle_input(PickerInput::StartOpen);
        ui.handle_input(PickerInput::SetCatalog {
            images: sample_images(),
            videos: sample_videos(),
        });
        assert_eq!(ui.page(), Page::Static);
        assert_eq!(ui.current_entry().unwrap().file_name, "gamma_sunset.webp");
        let sel = ui.selector_layout();
        assert!(!sel.mode_buttons[0].selected);
        assert!(sel.mode_buttons[1].selected);
        assert!((sel.mode_pill_center_x - sel.mode_buttons[1].center_x).abs() < 0.5);
    }

    // Open on Video page, manually scroll to Static page, then receive SetDaemonState(EngineKind::Video):
    // must NOT forcibly yank the user back to Page::Video.
    let mut ui = PickerUi::new(PickerConfig::default(), 1920.0, 1080.0);
    let video_snap = DaemonStateSnapshot {
        engine: EngineKind::Video,
        wallpaper_mode: WallpaperMode::Awww,
        video_mode: VideoMode::Parallax,
        transition_type: TransitionType::Fade,
        current_wallpaper: "/wallpapers/beta_ocean.jpg".to_string(),
        current_video: "/dynamic/clip_neon.mkv".to_string(),
        busy: false,
        error: String::new(),
    };
    ui.handle_input(PickerInput::SetDaemonState(video_snap.clone()));
    ui.handle_input(PickerInput::StartOpen);
    ui.handle_input(PickerInput::SetCatalog {
        images: sample_images(),
        videos: sample_videos(),
    });
    assert_eq!(ui.page(), Page::Video);

    // Manually scroll up (+60) to Page::Static
    ui.handle_input(PickerInput::PointerWheel { dx: 0.0, dy: 60.0 });
    assert_eq!(ui.page(), Page::Static);
    let mut acts = Vec::new();
    for _ in 0..40 {
        ui.tick(0.016, &mut acts);
    }
    assert!(!ui.is_page_animating());

    // Push a fresh SetDaemonState with EngineKind::Video while visible on Page::Static
    let mut updated_snap = video_snap;
    updated_snap.transition_type = TransitionType::Wipe;
    ui.handle_input(PickerInput::SetDaemonState(updated_snap));
    assert_eq!(
        ui.page(),
        Page::Static,
        "SetDaemonState while already visible must not yank user off manually selected page"
    );
    assert_eq!(ui.current_entry().unwrap().file_name, "beta_ocean.jpg");

    // Scrolling back down to Page::Video still works identically
    ui.handle_input(PickerInput::PointerWheel { dx: 0.0, dy: -60.0 });
    assert_eq!(ui.page(), Page::Video);
    assert_eq!(ui.current_entry().unwrap().file_name, "clip_neon.mkv");
}

#[test]
fn panel_open_close_animation_and_interruptible_reopen() {
    let mut ui = setup_open_ui();
    let mut acts = Vec::new();

    // Fresh open starts at panel_progress == 0.0 and smoothly ramps via OutCubic over 240ms
    assert!(ui.is_visible());
    assert!(!ui.is_closing());
    assert!(ui.is_animating());
    assert!((ui.panel_progress() - 0.0).abs() < 1e-4);
    assert!((ui.overlay_alpha() - 0.0).abs() < 1e-4);
    assert!((ui.scrim_opacity() - 0.0).abs() < 1e-4);

    let mut prev = ui.panel_progress();
    let mut mid_sample = 0.0;
    for step in 1..=16 {
        ui.tick(0.016, &mut acts);
        let cur = ui.panel_progress();
        assert!(
            cur >= prev - 1e-5,
            "panel_progress must increase monotonically on open: {prev} -> {cur}"
        );
        if step == 8 {
            // 128ms into 240ms OutCubic -> strictly between 0 and 1, front-loaded > 0.75
            mid_sample = cur;
        }
        prev = cur;
    }
    assert!(
        mid_sample > 0.75 && mid_sample < 0.99,
        "expected OutCubic mid-open progress in (0.75, 0.99), got {mid_sample}"
    );
    assert!((ui.panel_progress() - 1.0).abs() < 1e-4);
    assert!((ui.overlay_alpha() - 1.0).abs() < 1e-4);
    assert!((ui.scrim_opacity() - 0.80).abs() < 1e-4);
    assert!(!ui.is_animating());
    assert!(acts.is_empty());

    // Move selection to index 2 so we can verify interruptible reopen preserves selection
    ui.handle_input(key(KeyCode::Right));
    assert_eq!(ui.current_index(), 2);
    for _ in 0..20 {
        ui.tick(0.016, &mut acts);
    }

    // Start close via Esc -> enters closing state without emitting Exit yet
    let esc_acts = ui.handle_input(key(KeyCode::Escape));
    assert!(esc_acts.is_empty());
    assert!(ui.is_closing());
    assert!(ui.is_visible());
    assert!(ui.is_animating());

    // While closing, clicks, drags, wheel, selectors, and navigation keys must be ignored
    let sel = ui.selector_layout();
    assert!(
        ui.handle_input(PickerInput::PointerClick {
            x: 960.0,
            y: 540.0,
            button: MouseButton::Left,
        })
        .is_empty()
    );
    assert!(ui.confirming_index().is_none());
    assert!(
        ui.handle_input(PickerInput::PointerClick {
            x: sel.mode_buttons[1].center_x,
            y: sel.mode_buttons[1].center_y,
            button: MouseButton::Left,
        })
        .is_empty()
    );
    assert!(
        ui.handle_input(PickerInput::PointerClick {
            x: sel.transition_buttons[2].center_x,
            y: sel.transition_buttons[2].center_y,
            button: MouseButton::Left,
        })
        .is_empty()
    );
    ui.handle_input(PickerInput::PointerPress {
        x: 960.0,
        y: 540.0,
        button: MouseButton::Left,
    });
    ui.handle_input(PickerInput::PointerMove { x: 800.0, y: 540.0 });
    assert!(!ui.is_dragging(), "drags must be ignored while closing");
    ui.handle_input(PickerInput::PointerRelease {
        x: 800.0,
        y: 540.0,
        button: MouseButton::Left,
    });
    ui.handle_input(PickerInput::PointerWheel { dx: 0.0, dy: -120.0 });
    assert_eq!(ui.page(), Page::Static, "wheel page switch must be ignored while closing");
    ui.handle_input(key(KeyCode::Left));
    ui.handle_input(key(KeyCode::Enter));
    ui.handle_input(ctrl_key('f'));
    assert_eq!(ui.current_index(), 2, "nav keys must be ignored while closing");
    assert!(ui.confirming_index().is_none());
    assert!(!ui.search_visible());

    // Step midway through 180ms close (~48ms)
    let mut prev_close = ui.panel_progress();
    for _ in 0..3 {
        ui.tick(0.016, &mut acts);
        let cur = ui.panel_progress();
        assert!(cur < prev_close, "panel_progress must decrease on close: {prev_close} -> {cur}");
        prev_close = cur;
    }
    assert!(acts.is_empty(), "Exit must not fire mid-close");
    assert!(ui.is_closing());
    let mid_close = ui.panel_progress();
    assert!(
        mid_close > 0.1 && mid_close < 0.9,
        "expected mid-close progress in (0.1, 0.9), got {mid_close}"
    );

    // Interrupt close mid-way with StartOpen -> zero jump, preserves selection, no Exit!
    let reopen_acts = ui.handle_input(PickerInput::StartOpen);
    assert!(reopen_acts.is_empty());
    assert!(!ui.is_closing());
    assert!(ui.is_visible());
    assert_eq!(ui.current_index(), 2, "reopen must preserve current selection");
    assert!(
        (ui.panel_progress() - mid_close).abs() < 1e-4,
        "interrupting close must not jump panel_progress: {mid_close} vs {}",
        ui.panel_progress()
    );

    let mut prev_reopen = ui.panel_progress();
    for _ in 0..16 {
        ui.tick(0.016, &mut acts);
        let cur = ui.panel_progress();
        assert!(cur >= prev_reopen - 1e-5);
        prev_reopen = cur;
    }
    assert!(acts.is_empty());
    assert!((ui.panel_progress() - 1.0).abs() < 1e-4);
    // Allow any re-evaluated card hover spring to settle to 0% idle GPU
    for _ in 0..25 {
        ui.tick(0.016, &mut acts);
    }
    assert!(acts.is_empty());
    assert!(!ui.is_animating());

    // Now close completely via StartClose and verify Exit fires only at completion
    let start_close_acts = ui.handle_input(PickerInput::StartClose);
    assert!(start_close_acts.is_empty());
    assert!(ui.is_closing());
    // Duplicate StartClose while already closing is a no-op
    assert!(ui.handle_input(PickerInput::StartClose).is_empty());

    let mut exit_step = None;
    for step in 1..=15 {
        ui.tick(0.016, &mut acts);
        if !acts.is_empty() && exit_step.is_none() {
            exit_step = Some(step);
        }
    }
    assert!(
        exit_step.is_some_and(|s| s >= 8),
        "Exit should only fire after animated close finishes, fired at step {exit_step:?}"
    );
    assert_eq!(acts, vec![PickerActionOut::Exit]);
    assert!(!ui.is_visible());
    assert!(!ui.is_closing());
    assert!((ui.panel_progress() - 0.0).abs() < 1e-4);
    assert!((ui.overlay_alpha() - 0.0).abs() < 1e-4);
    assert!((ui.scrim_opacity() - 0.0).abs() < 1e-4);

    // StartClose while already closed immediately emits Exit (--action close)
    let already_closed_acts = ui.handle_input(PickerInput::StartClose);
    assert_eq!(already_closed_acts, vec![PickerActionOut::Exit]);
}

#[test]
fn confirm_pick_dissolves_overlay_and_scrim_smoothly() {
    let mut ui = setup_open_ui();
    let mut acts = Vec::new();
    for _ in 0..16 {
        ui.tick(0.016, &mut acts);
    }
    assert!((ui.overlay_alpha() - 1.0).abs() < 1e-4);
    assert!((ui.scrim_opacity() - 0.80).abs() < 1e-4);

    ui.handle_input(key(KeyCode::Enter));
    assert_eq!(ui.confirming_index(), Some(1));

    let mut prev_alpha = ui.overlay_alpha();
    let mut prev_scrim = ui.scrim_opacity();
    let mut mid_alpha = 1.0;
    for step in 1..=15 {
        ui.tick(0.015, &mut acts);
        let a = ui.overlay_alpha();
        let s = ui.scrim_opacity();
        assert!(a < prev_alpha, "overlay_alpha must dissolve during confirm: {prev_alpha} -> {a}");
        assert!(s < prev_scrim, "scrim_opacity must dissolve during confirm: {prev_scrim} -> {s}");
        if step == 10 {
            // 150ms into 300ms OutCubic dissolve: 1 - ease_out_cubic(0.5) = 0.125
            mid_alpha = a;
        }
        prev_alpha = a;
        prev_scrim = s;
    }
    assert!(
        mid_alpha > 0.05 && mid_alpha < 0.25,
        "expected mid-confirm overlay_alpha near 0.125, got {mid_alpha}"
    );
    assert!(acts.is_empty());

    for _ in 0..10 {
        ui.tick(0.015, &mut acts);
    }
    assert_eq!(
        acts,
        vec![
            PickerActionOut::ApplyStatic("/wallpapers/beta_ocean.jpg".to_string()),
            PickerActionOut::Exit,
        ]
    );
    assert!(!ui.is_visible());
    assert!((ui.overlay_alpha() - 0.0).abs() < 1e-4);
    assert!((ui.scrim_opacity() - 0.0).abs() < 1e-4);
}

#[test]
fn search_box_open_close_animation_interrupt_and_page_switch_snapshot() {
    let mut ui = setup_open_ui();
    let mut acts = Vec::new();
    for _ in 0..16 {
        ui.tick(0.016, &mut acts);
    }
    assert!(!ui.search_visible());
    assert!((ui.search_progress() - 0.0).abs() < 1e-4);
    assert_eq!(ui.search_render_text(), "");

    // Open search with Ctrl+F -> ramps 0 -> 1 over 220ms
    ui.handle_input(ctrl_key('f'));
    assert!(ui.search_visible());
    assert!(ui.is_animating());
    assert!((ui.search_progress() - 0.0).abs() < 1e-4);

    let mut prev = ui.search_progress();
    let mut mid_open = 0.0;
    for step in 1..=15 {
        ui.tick(0.016, &mut acts);
        let cur = ui.search_progress();
        assert!(cur >= prev - 1e-5);
        if step == 7 {
            mid_open = cur;
        }
        prev = cur;
    }
    assert!(
        mid_open > 0.70 && mid_open < 0.99,
        "expected mid search open progress in (0.70, 0.99), got {mid_open}"
    );
    assert!((ui.search_progress() - 1.0).abs() < 1e-4);
    assert!(!ui.is_animating());

    // Close empty search with Esc -> stops capturing keys & hit-testing immediately, animates 1 -> 0 over 160ms
    let sb = ui.search_box_rect();
    assert!(ui.hit_test_search_box(sb.x + 10.0, sb.y + 10.0));
    ui.handle_input(key(KeyCode::Escape));
    assert!(!ui.search_visible());
    assert!(!ui.hit_test_search_box(sb.x + 10.0, sb.y + 10.0));
    assert!(ui.is_animating());
    // TextInput must NOT be captured while closing
    ui.handle_input(PickerInput::TextInput("ignored".to_string()));
    assert!(ui.search_text().is_empty());

    // Step midway through 160ms close (~48ms)
    for _ in 0..3 {
        ui.tick(0.016, &mut acts);
    }
    let mid_close = ui.search_progress();
    assert!(
        mid_close > 0.1 && mid_close < 0.9,
        "expected mid search close progress in (0.1, 0.9), got {mid_close}"
    );

    // Interrupt close mid-way with Ctrl+F -> no jump, reverses smoothly back to 1.0
    ui.handle_input(ctrl_key('f'));
    assert!(ui.search_visible());
    assert!(
        (ui.search_progress() - mid_close).abs() < 1e-4,
        "interrupting search close must not jump: {mid_close} vs {}",
        ui.search_progress()
    );
    for _ in 0..15 {
        ui.tick(0.016, &mut acts);
    }
    assert!((ui.search_progress() - 1.0).abs() < 1e-4);

    // Type query then switch page -> preserves display text during fade-out without flashing placeholder
    ui.handle_input(PickerInput::TextInput("SUN".to_string()));
    assert_eq!(ui.search_text(), "SUN");
    assert_eq!(ui.search_render_text(), "SUN");
    assert_eq!(ui.active_entries().len(), 1);

    ui.handle_input(PickerInput::ShowPage(Page::Video));
    assert_eq!(ui.page(), Page::Video);
    assert!(!ui.search_visible());
    assert_eq!(ui.search_text(), "");
    assert_eq!(ui.active_entries().len(), 3, "new page should not be filtered by old query");
    assert_eq!(
        ui.search_render_text(),
        "SUN",
        "search_render_text must preserve snapshot during page-switch fade-out"
    );
    assert!((ui.search_progress() - 1.0).abs() < 1e-4);

    // Mid-fade-out: still renders "SUN" while search_progress decreases smoothly
    for _ in 0..4 {
        ui.tick(0.016, &mut acts);
    }
    assert!(ui.search_progress() > 0.05 && ui.search_progress() < 0.95);
    assert_eq!(ui.search_render_text(), "SUN");

    // Finish fade-out: search_progress hits 0.0 and snapshot clears
    for _ in 0..12 {
        ui.tick(0.016, &mut acts);
    }
    assert!((ui.search_progress() - 0.0).abs() < 1e-4);
    assert_eq!(ui.search_render_text(), "");

    // Finish page cooldown, open search with text, then panel StartClose -> smoothly dismisses search too
    for _ in 0..20 {
        ui.tick(0.016, &mut acts);
    }
    ui.handle_input(ctrl_key('f'));
    for _ in 0..15 {
        ui.tick(0.016, &mut acts);
    }
    ui.handle_input(PickerInput::TextInput("rain".to_string()));
    assert_eq!(ui.search_render_text(), "rain");
    ui.handle_input(PickerInput::StartClose);
    assert!(ui.is_closing());
    assert!(!ui.search_visible());
    assert_eq!(ui.search_render_text(), "rain");
    for _ in 0..3 {
        ui.tick(0.016, &mut acts);
    }
    assert!(ui.search_progress() < 0.95);
    for _ in 0..15 {
        ui.tick(0.016, &mut acts);
    }
    assert_eq!(acts, vec![PickerActionOut::Exit]);
    assert!((ui.search_progress() - 0.0).abs() < 1e-4);
    assert_eq!(ui.search_render_text(), "");
}
