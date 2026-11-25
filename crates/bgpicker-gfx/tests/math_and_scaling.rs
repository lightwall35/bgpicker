use bgpicker_gfx::{
    DualKawaseBlur, KawaseSettings, ParallaxSettings, TransitionEffect, TransitionSpec,
    compute_geometry, compute_mode_geometry, compute_scaled_size, ease_in_out_cubic,
    ease_out_cubic, ease_transition_bezier,
};

fn approx_eq(a: f32, b: f32, eps: f32) {
    assert!((a - b).abs() <= eps, "expected {a} ≈ {b} (eps {eps})");
}

#[test]
fn panorama_covers_without_upscaling_zoom() {
    let g = compute_geometry(1920.0, 1080.0, 7200.0, 2400.0, 1.12);
    approx_eq(g.width, 3240.0, 1e-3);
    approx_eq(g.height, 1080.0, 1e-3);
    approx_eq(g.overflow_x, 1320.0, 1e-3);
    approx_eq(g.y, 0.0, 1e-3);
}

#[test]
fn mode_geometry_interpolates_between_center_and_parallax() {
    let centered = compute_mode_geometry(1920.0, 1080.0, 7200.0, 2400.0, 1.12, 0.0, 0.0);
    let halfway = compute_mode_geometry(1920.0, 1080.0, 7200.0, 2400.0, 1.12, 0.0, 0.5);
    let parallax = compute_mode_geometry(1920.0, 1080.0, 7200.0, 2400.0, 1.12, 0.0, 1.0);
    approx_eq(centered.x, -660.0, 1e-3);
    approx_eq(parallax.x, 0.0, 1e-3);
    approx_eq(halfway.x, -330.0, 1e-3);
    approx_eq(centered.width, 3240.0, 1e-3);
    approx_eq(parallax.width, 3240.0, 1e-3);

    let ordinary = compute_mode_geometry(1920.0, 1080.0, 1920.0, 1080.0, 1.12, 1.0, 0.0);
    let enlarged = compute_mode_geometry(1920.0, 1080.0, 1920.0, 1080.0, 1.12, 1.0, 1.0);
    approx_eq(ordinary.width, 1920.0, 1e-3);
    assert!(enlarged.width > ordinary.width);
    assert!(enlarged.x < ordinary.x && enlarged.y < ordinary.y);
}

#[test]
fn ordinary_and_portrait_never_expose_empty_edges() {
    for (mw, mh) in [(1920.0, 1080.0), (1000.0, 2000.0), (0.0, 0.0)] {
        let g = compute_geometry(1920.0, 1080.0, mw, mh, 1.12);
        assert!(g.width >= 1920.0 && g.height >= 1080.0);
        assert!(g.overflow_x > 0.0 && g.y < 0.0);
        assert!(-g.overflow_x + g.width >= 1920.0 - 1e-3);
    }
}

#[test]
fn transition_spec_and_settings_bounds() {
    assert_eq!(TransitionEffect::from_name("wipe").shader_kind(), 1);
    assert_eq!(TransitionEffect::from_name("wave").shader_kind(), 2);
    assert_eq!(TransitionEffect::from_name("grow").shader_kind(), 3);
    assert_eq!(TransitionEffect::from_name("outer").shader_kind(), 4);
    assert_eq!(TransitionEffect::from_name("none").shader_kind(), 5);
    assert_eq!(TransitionEffect::from_name("unknown").shader_kind(), 0);

    let clamped = TransitionSpec {
        effect: TransitionEffect::Wave,
        duration_ms: 99_999,
        angle_deg: 720.0,
        position: [-2.0, 5.0],
        wave: [0.0, 999.0],
    }
    .clamped();
    assert_eq!(clamped.duration_ms, 5000);
    approx_eq(clamped.angle_deg, 360.0, 1e-5);
    assert_eq!(clamped.position, [0.0, 1.0]);
    assert_eq!(clamped.wave, [1.0, 500.0]);

    let p = ParallaxSettings {
        zoom: 100.0,
        duration_ms: 10_000,
        mode_transition_ms: 500,
        transition_ms: 350,
        max_texture_size: 100,
    }
    .clamped();
    approx_eq(p.zoom, 2.0, 1e-5);
    assert_eq!(p.duration_ms, 3000);
    assert_eq!(p.max_texture_size, 1024);
}

#[test]
fn easing_curves_are_monotonic_and_hit_endpoints() {
    for curve in [ease_out_cubic, ease_in_out_cubic, ease_transition_bezier] {
        approx_eq(curve(0.0), 0.0, 1e-5);
        approx_eq(curve(1.0), 1.0, 1e-5);
        let mut prev = 0.0;
        for i in 1..=20 {
            let t = (i as f32) / 20.0;
            let v = curve(t);
            assert!(v >= prev - 1e-5, "non-monotonic at {t}: {v} < {prev}");
            prev = v;
        }
    }
    // Bezier (0.54, 0.0, 0.34, 0.99) starts slow and accelerates strongly through middle.
    assert!(ease_transition_bezier(0.1) < 0.05);
    assert!(ease_transition_bezier(0.6) > 0.70);
}

#[test]
fn fractional_scaling_and_kawase_pyramid_sizes() {
    assert_eq!(compute_scaled_size(1920, 1080, 120), (1920, 1080));
    assert_eq!(compute_scaled_size(1920, 1080, 180), (2880, 1620));
    assert_eq!(compute_scaled_size(2560, 1440, 150), (3200, 1800));
    assert_eq!(compute_scaled_size(1921, 1081, 160), (2561, 1441));

    let pyramid = DualKawaseBlur::pyramid_sizes(1920, 1080);
    assert_eq!(
        pyramid,
        [(960, 540), (480, 270), (240, 135), (120, 68)]
    );

    let kawase = KawaseSettings::default();
    assert_eq!(kawase.passes, 3);
    approx_eq(kawase.offset, 1.25, 1e-5);
    approx_eq(kawase.saturation, 1.12, 1e-5);
    approx_eq(kawase.brightness, -0.04, 1e-5);
}
