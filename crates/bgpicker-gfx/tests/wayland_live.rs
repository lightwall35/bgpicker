use std::thread;
use std::time::{Duration, Instant};

use bgpicker_gfx::{
    FrameInput, FrameModeTargets, LayerRole, LayerTargets, RenderCommand, TransitionEffect,
    TransitionSpec, WaylandRenderer,
};

fn make_solid_frame(key: &str, rgba: [u8; 4]) -> FrameInput {
    let mut pixels = Vec::with_capacity(4 * 4 * 4);
    for _ in 0..16 {
        pixels.extend_from_slice(&rgba);
    }
    FrameInput::from_rgba(key, 0, 4, 4, None, pixels).expect("valid RGBA frame")
}

fn connect_and_configure() -> Option<WaylandRenderer> {
    std::env::var_os("WAYLAND_DISPLAY")?;

    let mut renderer = match WaylandRenderer::connect(true) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("skipping live Wayland test: {err}");
            return None;
        }
    };
    let handle = renderer.handle();

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        renderer.step(Some(Duration::from_millis(20))).unwrap();
        let snaps = handle.snapshots();
        if !snaps.is_empty()
            && snaps.iter().all(|o| {
                o.wallpaper.logical_size.0 > 0
                    && o.wallpaper.pixel_size.0 > 0
                    && o.backdrop.as_ref().is_some_and(|b| b.logical_size.0 > 0 && b.pixel_size.0 > 0)
            })
        {
            return Some(renderer);
        }
        thread::sleep(Duration::from_millis(10));
    }

    Some(renderer)
}

#[test]
fn wayland_layer_shell_configures_wallpaper_and_backdrop_without_mapping_buffers() {
    let Some(mut renderer) = connect_and_configure() else {
        return;
    };
    let handle = renderer.handle();

    let snaps = handle.snapshots();
    assert!(!snaps.is_empty(), "expected at least one Wayland output");
    for out in &snaps {
        assert_eq!(out.wallpaper.role, LayerRole::Wallpaper);
        assert!(out.wallpaper.logical_size.0 > 0 && out.wallpaper.logical_size.1 > 0);
        assert!(out.wallpaper.pixel_size.0 > 0 && out.wallpaper.pixel_size.1 > 0);
        assert!(out.wallpaper.scale_120 >= 60);
        assert!(!out.wallpaper.presented);

        let backdrop = out.backdrop.as_ref().expect("backdrop slot enabled");
        assert_eq!(backdrop.role, LayerRole::Backdrop);
        assert!(backdrop.logical_size.0 > 0 && backdrop.logical_size.1 > 0);
        assert!(backdrop.pixel_size.0 > 0 && backdrop.pixel_size.1 > 0);
        assert!(!backdrop.presented);
    }

    // Verify backdrop can be torn down dynamically while wallpaper remains active.
    handle.send(RenderCommand::SetBackdropEnabled(false)).unwrap();
    renderer.step(Some(Duration::from_millis(20))).unwrap();
    let snaps_after = handle.snapshots();
    assert!(snaps_after.iter().all(|o| o.backdrop.is_none()));

    handle.send(RenderCommand::Shutdown).unwrap();
    renderer.step(Some(Duration::from_millis(20))).unwrap();
}

#[test]
fn idle_animations_start_at_frame_zero_after_long_idle() {
    let Some(mut renderer) = connect_and_configure() else {
        return;
    };
    let handle = renderer.handle();

    let frame_a = make_solid_frame("test://frame-a", [32, 96, 160, 255]);
    handle
        .send(RenderCommand::ApplyFrame {
            frame: frame_a,
            transition: TransitionSpec::instant(),
            targets: LayerTargets::Both,
            modes: FrameModeTargets::default(),
        })
        .unwrap();
    handle
        .send(RenderCommand::SetProgress {
            output: None,
            progress: 0.0,
            duration_ms: Some(0),
        })
        .unwrap();
    renderer.step(Some(Duration::from_millis(20))).unwrap();

    // Simulate sitting idle longer than the 250ms dt clamp (420ms > 250ms).
    // Before the fix, `last_tick` became stale and Frame 0 jumped to 97.7% of a 350ms pan.
    thread::sleep(Duration::from_millis(420));

    handle
        .send(RenderCommand::SetProgress {
            output: None,
            progress: 1.0,
            duration_ms: Some(350),
        })
        .unwrap();
    renderer.step(Some(Duration::from_millis(20))).unwrap();

    let snaps = handle.snapshots();
    assert!(!snaps.is_empty());
    for out in &snaps {
        assert!(
            out.wallpaper.animated_progress < 0.25,
            "wallpaper SetProgress jumped on frame 0 after idle: {}",
            out.wallpaper.animated_progress
        );
        if let Some(ref b) = out.backdrop {
            assert!(
                b.animated_progress < 0.25,
                "backdrop SetProgress jumped on frame 0 after idle: {}",
                b.animated_progress
            );
        }
    }

    // Let SetProgress finish cleanly and go idle again.
    let deadline = Instant::now() + Duration::from_millis(600);
    while Instant::now() < deadline {
        renderer.step(Some(Duration::from_millis(20))).unwrap();
        let s = handle.snapshots();
        if s.iter().all(|o| (o.wallpaper.animated_progress - 1.0).abs() < 1e-3) {
            break;
        }
        thread::sleep(Duration::from_millis(16));
    }

    // Idle > 400ms again, then trigger SetMode (1.0 -> 0.0 over 500ms).
    thread::sleep(Duration::from_millis(420));
    handle
        .send(RenderCommand::SetMode {
            parallax: false,
            duration_ms: Some(500),
            targets: LayerTargets::Both,
        })
        .unwrap();
    renderer.step(Some(Duration::from_millis(20))).unwrap();

    let snaps_mode = handle.snapshots();
    for out in &snaps_mode {
        assert!(
            out.wallpaper.mode_blend > 0.75,
            "wallpaper SetMode jumped on frame 0 after idle: {}",
            out.wallpaper.mode_blend
        );
        if let Some(ref b) = out.backdrop {
            assert!(
                b.mode_blend > 0.75,
                "backdrop SetMode jumped on frame 0 after idle: {}",
                b.mode_blend
            );
        }
    }

    // Finish SetMode, stage a new frame, go idle > 400ms, then CommitStaged.
    let mode_deadline = Instant::now() + Duration::from_millis(700);
    while Instant::now() < mode_deadline {
        renderer.step(Some(Duration::from_millis(20))).unwrap();
        let s = handle.snapshots();
        if s.iter().all(|o| o.wallpaper.mode_blend < 1e-3) {
            break;
        }
        thread::sleep(Duration::from_millis(16));
    }

    let frame_b = make_solid_frame("test://frame-b", [180, 70, 50, 255]);
    handle
        .send(RenderCommand::StageFrame {
            frame: frame_b,
            targets: LayerTargets::Both,
        })
        .unwrap();
    renderer.step(Some(Duration::from_millis(20))).unwrap();

    thread::sleep(Duration::from_millis(420));
    handle
        .send(RenderCommand::CommitStaged {
            transition: TransitionSpec {
                effect: TransitionEffect::Fade,
                duration_ms: 350,
                ..TransitionSpec::default()
            },
            targets: LayerTargets::Both,
            modes: FrameModeTargets::default(),
        })
        .unwrap();
    renderer.step(Some(Duration::from_millis(20))).unwrap();

    let snaps_commit = handle.snapshots();
    for out in &snaps_commit {
        assert!(
            out.wallpaper.transition_active && out.wallpaper.transition_progress < 0.25,
            "wallpaper CommitStaged jumped on frame 0 after idle: active={} progress={}",
            out.wallpaper.transition_active,
            out.wallpaper.transition_progress
        );
        if let Some(ref b) = out.backdrop {
            assert!(
                b.transition_active && b.transition_progress < 0.25,
                "backdrop CommitStaged jumped on frame 0 after idle: active={} progress={}",
                b.transition_active,
                b.transition_progress
            );
        }
    }

    handle.send(RenderCommand::Shutdown).unwrap();
    renderer.step(Some(Duration::from_millis(20))).unwrap();
}
