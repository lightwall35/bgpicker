use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bgpicker_core::config::{set_transition_type, TransitionType};
use bgpicker_core::paths::{
    atomic_write_text, BackdropParallax, BgPickerPaths, EngineKind, PersistedState, VideoMode,
    WallpaperMode,
};
use bgpicker_core::protocol::{
    BackdropAction, BackdropOverride, EngineAction, IpcClient, ModeAction, Request, ResponseData,
    VideoModeAction,
};
use bgpicker_daemon::ctl::{parse_args, CtlCommand, PickerAction, VideoSubcommand};
use bgpicker_daemon::daemon::{DaemonInstance, DaemonOptions};
use bgpicker_daemon::renderer::RendererBackend;
use bgpicker_daemon::video::{RealVideoBackend, VideoBackend, VideoReadyFrame};
use bgpicker_gfx::{
    FrameInput, KawaseSettings, LayerRole, LayerTargets, OutputSnapshot, ParallaxSettings,
    SurfaceSnapshot, TransitionEffect, TransitionSpec,
};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "bgpicker-daemon-test-{prefix}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        Self { path: dir }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn isolated_paths(root: &Path) -> BgPickerPaths {
    let home = root.join("home");
    let config = root.join("config");
    let cache = root.join("cache");
    let state = root.join("state");
    let runtime = root.join("runtime");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    BgPickerPaths::with_roots(home, config, cache, state, runtime)
}

fn write_tiny_png(path: &Path) {
    // 1x1 red PNG
    const PNG_BYTES: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    fs::write(path, PNG_BYTES).expect("write tiny png");
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
enum RecordedRenderOp {
    ApplyFrame {
        media_key: String,
        effect: TransitionEffect,
        duration_ms: u32,
        targets: LayerTargets,
    },
    SetProgress {
        output: Option<String>,
        progress: f32,
        duration_ms: Option<u32>,
    },
    SetMode {
        parallax: bool,
        duration_ms: Option<u32>,
        targets: LayerTargets,
    },
    UpdateSettings {
        zoom: f32,
    },
}

struct MockRenderer {
    ops: Mutex<Vec<RecordedRenderOp>>,
    outputs: Mutex<Vec<OutputSnapshot>>,
}

impl MockRenderer {
    fn new() -> Self {
        Self {
            ops: Mutex::new(Vec::new()),
            outputs: Mutex::new(vec![OutputSnapshot {
                name: "DP-1".into(),
                wallpaper: SurfaceSnapshot {
                    role: LayerRole::Wallpaper,
                    logical_size: (1920, 1080),
                    pixel_size: (1920, 1080),
                    scale_120: 120,
                    presented: false,
                    transition_active: false,
                    transition_progress: 1.0,
                    animated_progress: 0.5,
                    mode_blend: 0.0,
                },
                backdrop: Some(SurfaceSnapshot {
                    role: LayerRole::Backdrop,
                    logical_size: (1920, 1080),
                    pixel_size: (1920, 1080),
                    scale_120: 120,
                    presented: false,
                    transition_active: false,
                    transition_progress: 1.0,
                    animated_progress: 0.5,
                    mode_blend: 0.0,
                }),
            }]),
        }
    }

    fn take_ops(&self) -> Vec<RecordedRenderOp> {
        std::mem::take(&mut *self.ops.lock().unwrap())
    }
}

impl RendererBackend for MockRenderer {
    fn decode_image(&self, path: &Path, max_texture_size: u32) -> Result<FrameInput, String> {
        FrameInput::from_image_path(path, max_texture_size).map_err(|e| e.to_string())
    }

    fn apply_frame(
        &self,
        frame: FrameInput,
        transition: TransitionSpec,
        targets: LayerTargets,
    ) -> Result<(), String> {
        self.ops.lock().unwrap().push(RecordedRenderOp::ApplyFrame {
            media_key: frame.media_key,
            effect: transition.effect,
            duration_ms: transition.duration_ms,
            targets,
        });
        if let Some(out) = self.outputs.lock().unwrap().first_mut() {
            out.wallpaper.presented = true;
            if let Some(ref mut b) = out.backdrop {
                b.presented = true;
            }
        }
        Ok(())
    }

    fn push_video_frame(&self, _frame: FrameInput, _sync_backdrop: bool) -> Result<(), String> {
        Ok(())
    }

    fn set_progress(
        &self,
        output: Option<String>,
        progress: f32,
        duration_ms: Option<u32>,
    ) -> Result<(), String> {
        self.ops.lock().unwrap().push(RecordedRenderOp::SetProgress {
            output,
            progress,
            duration_ms,
        });
        if let Some(out) = self.outputs.lock().unwrap().first_mut() {
            out.wallpaper.animated_progress = progress;
        }
        Ok(())
    }

    fn set_mode(
        &self,
        parallax: bool,
        duration_ms: Option<u32>,
        targets: LayerTargets,
    ) -> Result<(), String> {
        self.ops.lock().unwrap().push(RecordedRenderOp::SetMode {
            parallax,
            duration_ms,
            targets,
        });
        if let Some(out) = self.outputs.lock().unwrap().first_mut() {
            let blend = if parallax { 1.0 } else { 0.0 };
            if targets.includes_wallpaper() {
                out.wallpaper.mode_blend = blend;
            }
            if targets.includes_backdrop() {
                if let Some(ref mut b) = out.backdrop {
                    b.mode_blend = blend;
                }
            }
        }
        Ok(())
    }

    fn update_settings(
        &self,
        parallax: ParallaxSettings,
        _kawase: KawaseSettings,
    ) -> Result<(), String> {
        self.ops
            .lock()
            .unwrap()
            .push(RecordedRenderOp::UpdateSettings {
                zoom: parallax.zoom,
            });
        Ok(())
    }

    fn clear(&self) -> Result<(), String> {
        Ok(())
    }

    fn shutdown(&self) {}

    fn snapshots(&self) -> Vec<OutputSnapshot> {
        self.outputs.lock().unwrap().clone()
    }
}

struct MockVideo {
    open_calls: Mutex<Vec<PathBuf>>,
    overview_history: Mutex<Vec<bool>>,
    active_path: Mutex<Option<PathBuf>>,
    next_id: AtomicU64,
    fail_next: Mutex<bool>,
}

impl MockVideo {
    fn new() -> Self {
        Self {
            open_calls: Mutex::new(Vec::new()),
            overview_history: Mutex::new(Vec::new()),
            active_path: Mutex::new(None),
            next_id: AtomicU64::new(1),
            fail_next: Mutex::new(false),
        }
    }
}

impl VideoBackend for MockVideo {
    fn open_and_wait(
        &self,
        path: &Path,
        overview_open: bool,
        _timeout: Duration,
    ) -> Result<VideoReadyFrame, String> {
        self.open_calls.lock().unwrap().push(path.to_path_buf());
        self.overview_history.lock().unwrap().push(overview_open);
        if *self.fail_next.lock().unwrap() {
            return Err("mock video decode error".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let data: Arc<[u8]> = Arc::from(vec![255u8; 4 * 2 * 2].into_boxed_slice());
        let frame =
            FrameInput::from_rgba(path.to_string_lossy(), 1, 2, 2, None, data).unwrap();
        *self.active_path.lock().unwrap() = Some(path.to_path_buf());
        Ok(VideoReadyFrame {
            stream_id: id,
            fg_frame: frame,
            bg_frame: None,
        })
    }

    fn activate_stream(&self, _stream_id: u64) {}

    fn stop(&self) {
        *self.active_path.lock().unwrap() = None;
    }

    fn set_overview_open(&self, open: bool) {
        self.overview_history.lock().unwrap().push(open);
    }

    fn is_playing_path(&self, path: &Path) -> bool {
        self.active_path.lock().unwrap().as_deref() == Some(path)
    }

    fn shutdown(&self) {}
}

#[test]
fn cli_parser_covers_full_contract() {
    let (g, c) = parse_args(["--socket", "/tmp/a.sock", "status", "--json", "--no-start"]).unwrap();
    assert_eq!(g.socket, Some(PathBuf::from("/tmp/a.sock")));
    assert_eq!(
        c,
        CtlCommand::Status {
            json: true,
            no_start: true
        }
    );

    let (_, c) = parse_args(["apply-static", "wall.png", "--mode", "parallax"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::ApplyStatic {
            path: PathBuf::from("wall.png"),
            mode: Some(WallpaperMode::Parallax),
            backdrop: BackdropOverride::Auto,
        }
    );

    let (_, c) = parse_args([
        "apply-static",
        "--keep-backdrop",
        "wall.png",
        "--mode",
        "awww",
    ])
    .unwrap();
    assert_eq!(
        c,
        CtlCommand::ApplyStatic {
            path: PathBuf::from("wall.png"),
            mode: Some(WallpaperMode::Awww),
            backdrop: BackdropOverride::Keep,
        }
    );

    let (_, c) = parse_args(["mode", "toggle"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::Mode {
            action: Some(ModeAction::Toggle),
            backdrop: BackdropOverride::Auto,
        }
    );

    let (_, c) = parse_args(["mode", "parallax", "--backdrop", "off"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::Mode {
            action: Some(ModeAction::Parallax),
            backdrop: BackdropOverride::Off,
        }
    );

    let (_, c) = parse_args(["backdrop-parallax", "on"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::BackdropParallax {
            action: Some(BackdropAction::On)
        }
    );

    let (_, c) = parse_args(["apply-video", "parallax", "clip.mp4"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::ApplyVideo {
            mode: Some(VideoModeAction::Parallax),
            path: Some(PathBuf::from("clip.mp4")),
            backdrop: BackdropOverride::Auto,
        }
    );

    let (_, c) = parse_args(["apply-video", "--backdrop=on", "normal"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::ApplyVideo {
            mode: Some(VideoModeAction::Normal),
            path: None,
            backdrop: BackdropOverride::On,
        }
    );

    let (_, c) = parse_args(["video"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::Video {
            sub: VideoSubcommand::Status,
            backdrop: BackdropOverride::Auto,
        }
    );

    let (_, c) = parse_args(["video", "next", "--keep-backdrop"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::Video {
            sub: VideoSubcommand::Next,
            backdrop: BackdropOverride::Keep,
        }
    );

    // Invalid flag combinations are rejected cleanly.
    assert!(parse_args(["mode", "--keep-backdrop"]).is_err());
    assert!(parse_args(["video", "status", "--backdrop", "on"]).is_err());
    assert!(parse_args(["mode", "parallax", "--keep-backdrop", "--backdrop", "on"]).is_err());
    assert!(parse_args(["apply-static", "wall.png", "--backdrop", "bogus"]).is_err());

    let (_, c) = parse_args(["engine", "simple"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::Engine {
            action: Some(EngineAction::Simple)
        }
    );

    let (_, c) = parse_args(["transition", "wave"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::Transition {
            transition_type: Some(TransitionType::Wave)
        }
    );

    let (_, c) = parse_args(["catalog", "--images", "w", "--videos", "d"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::Catalog {
            images: Some(PathBuf::from("w")),
            videos: Some(PathBuf::from("d")),
        }
    );

    let (_, c) = parse_args(["picker", "next"]).unwrap();
    assert_eq!(
        c,
        CtlCommand::Picker {
            action: PickerAction::Next
        }
    );
}

#[test]
fn daemon_static_mode_backdrop_engine_and_error_rollback() {
    let tmp = TempDir::new("actor");
    let paths = isolated_paths(tmp.path());
    let proj = tmp.path().join("proj");
    fs::create_dir_all(proj.join("wallpapers")).unwrap();
    fs::create_dir_all(proj.join("dynamic")).unwrap();

    let img_a = proj.join("wallpapers/a.png");
    let img_b = proj.join("wallpapers/b.png");
    write_tiny_png(&img_a);
    write_tiny_png(&img_b);
    let vid_a = proj.join("dynamic/loop.mp4");
    fs::write(&vid_a, b"dummy").unwrap();

    let renderer = Arc::new(MockRenderer::new());
    let video = Arc::new(MockVideo::new());

    let opts = DaemonOptions {
        paths: paths.clone(),
        project_dir: proj.clone(),
        restore_on_start: true,
        enable_niri: false,
        enable_settings_watch: false,
    };
    let mut daemon = DaemonInstance::start_with_backends(opts, renderer.clone(), video.clone())
        .expect("start daemon");
    let client = IpcClient::new(daemon.socket_path());

    // Fresh install startup was a clean no-op.
    assert!(renderer.take_ops().iter().all(|op| !matches!(op, RecordedRenderOp::ApplyFrame { .. })));

    // Apply static with random transition + parallax mode -> syncs backdrop_parallax=on.
    let resp = client
        .call(&Request::ApplyStatic {
            path: img_a.clone(),
            mode: Some(WallpaperMode::Parallax),
            instant: false,
            backdrop: BackdropOverride::Auto,
        })
        .unwrap();
    assert!(resp.ok, "{resp:?}");

    let state = PersistedState::load(&paths);
    assert_eq!(state.engine, EngineKind::Full);
    assert_eq!(state.mode, WallpaperMode::Parallax);
    assert_eq!(state.backdrop_parallax, BackdropParallax::On);
    assert_eq!(
        state.current_wallpaper.as_deref(),
        Some(fs::canonicalize(&img_a).unwrap().as_path())
    );

    // Invalid image preserves prior state and produces zero ApplyFrame ops.
    let _ = renderer.take_ops();
    let bad = client
        .call(&Request::ApplyStatic {
            path: proj.join("wallpapers/missing.png"),
            mode: Some(WallpaperMode::Awww),
            instant: false,
            backdrop: BackdropOverride::Auto,
        })
        .unwrap();
    assert!(!bad.ok);
    assert!(renderer.take_ops().is_empty());
    assert_eq!(PersistedState::load(&paths).mode, WallpaperMode::Parallax);
    assert_eq!(
        PersistedState::load(&paths).backdrop_parallax,
        BackdropParallax::On
    );

    // Toggle static mode smoothly -> syncs both foreground and backdrop via LayerTargets::Both.
    let resp = client
        .call(&Request::SetMode {
            mode: ModeAction::Toggle,
            backdrop: BackdropOverride::Auto,
        })
        .unwrap();
    assert!(resp.ok);
    let state = PersistedState::load(&paths);
    assert_eq!(state.mode, WallpaperMode::Awww);
    assert_eq!(state.backdrop_parallax, BackdropParallax::Off);
    assert!(renderer.take_ops().iter().any(|op| matches!(
        op,
        RecordedRenderOp::SetMode {
            parallax: false,
            duration_ms: Some(500),
            targets: LayerTargets::Both
        }
    )));

    // Toggle backdrop parallax independently.
    let resp = client
        .call(&Request::BackdropParallax {
            action: BackdropAction::Toggle,
        })
        .unwrap();
    assert!(resp.ok);
    assert_eq!(
        PersistedState::load(&paths).backdrop_parallax,
        BackdropParallax::On
    );
    assert!(renderer.take_ops().iter().any(|op| matches!(
        op,
        RecordedRenderOp::SetMode {
            parallax: true,
            targets: LayerTargets::BackdropOnly,
            ..
        }
    )));

    // Switch to video (normal), then toggle video mode -> syncs backdrop_parallax too.
    let resp = client
        .call(&Request::ApplyVideo {
            mode: Some(VideoModeAction::Normal),
            path: Some(vid_a.clone()),
            backdrop: BackdropOverride::Auto,
        })
        .unwrap();
    assert!(resp.ok);
    let state = PersistedState::load(&paths);
    assert_eq!(state.engine, EngineKind::Video);
    assert_eq!(state.backdrop_parallax, BackdropParallax::Off);
    assert_eq!(video.open_calls.lock().unwrap().len(), 1);

    let _ = renderer.take_ops();
    let resp = client
        .call(&Request::ApplyVideo {
            mode: Some(VideoModeAction::Parallax),
            path: Some(vid_a.clone()),
            backdrop: BackdropOverride::Auto,
        })
        .unwrap();
    assert!(resp.ok);
    assert_eq!(video.open_calls.lock().unwrap().len(), 1);
    let state = PersistedState::load(&paths);
    assert_eq!(state.video_mode, VideoMode::Parallax);
    assert_eq!(state.backdrop_parallax, BackdropParallax::On);
    assert!(renderer.take_ops().iter().any(|op| matches!(
        op,
        RecordedRenderOp::SetMode {
            parallax: true,
            duration_ms: Some(500),
            targets: LayerTargets::Both
        }
    )));

    // Switch to simple engine -> forces parallax off + instant static frame.
    let resp = client
        .call(&Request::SetEngine {
            engine: EngineAction::Simple,
        })
        .unwrap();
    assert!(resp.ok);
    assert_eq!(PersistedState::load(&paths).engine, EngineKind::Simple);
    let simple_ops = renderer.take_ops();
    assert!(simple_ops.iter().any(|op| matches!(
        op,
        RecordedRenderOp::ApplyFrame {
            effect: TransitionEffect::None,
            duration_ms: 0,
            ..
        }
    )));

    // Simple engine rejects mode change.
    let err_mode = client
        .call(&Request::SetMode {
            mode: ModeAction::Parallax,
            backdrop: BackdropOverride::Auto,
        })
        .unwrap();
    assert!(!err_mode.ok);

    // Stop removes socket cleanly.
    let sock_path = daemon.socket_path().to_path_buf();
    let stop_resp = client.call(&Request::Stop).unwrap();
    assert!(stop_resp.ok);
    daemon.shutdown();
    assert!(!sock_path.exists());
}

#[test]
fn daemon_niri_stream_and_inotify_settings_reload() {
    let tmp = TempDir::new("niri-inotify");
    let paths = isolated_paths(tmp.path());
    let proj = tmp.path().join("proj");
    fs::create_dir_all(&proj).unwrap();

    // Fake niri socket emitting workspace + window + overview events.
    let niri_sock = tmp.path().join("niri.sock");
    let listener = UnixListener::bind(&niri_sock).unwrap();
    let niri_thread = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut req = String::new();
            let _ = reader.read_line(&mut req);
            let lines = [
                r#"{"Ok":"Handled"}"#,
                r#"{"WorkspacesChanged":{"workspaces":[{"id":1,"idx":1,"output":"DP-1","is_active":true,"is_focused":true,"active_window_id":2}]}}"#,
                r#"{"WindowsChanged":{"windows":[{"id":1,"workspace_id":1,"is_focused":false,"is_floating":false,"layout":{"pos_in_scrolling_layout":[1,1]}},{"id":2,"workspace_id":1,"is_focused":true,"is_floating":false,"layout":{"pos_in_scrolling_layout":[3,1]}}]}}"#,
                r#"{"OverviewOpenedOrClosed":{"is_open":true}}"#,
            ];
            for line in lines {
                let _ = writeln!(stream, "{line}");
            }
            let _ = stream.flush();
            thread::sleep(Duration::from_millis(600));
        }
    });

    // Note: NiriClientConfig::from_env reads NIRI_SOCKET, so we set it scoped here
    // or test NiriUpdate via actor handle + SettingsWatcher directly.
    let renderer = Arc::new(MockRenderer::new());
    let video = Arc::new(MockVideo::new());

    let opts = DaemonOptions {
        paths: paths.clone(),
        project_dir: proj,
        restore_on_start: false,
        enable_niri: false,
        enable_settings_watch: true,
    };
    let mut daemon = DaemonInstance::start_with_backends(opts, renderer.clone(), video.clone())
        .expect("start daemon");
    let actor = daemon.actor_handle();
    let client = IpcClient::new(daemon.socket_path());

    // Drive niri events through NiriWatcher explicitly targeting our test socket.
    let watcher = bgpicker_core::niri::NiriWatcher::spawn(
        bgpicker_core::niri::NiriClientConfig::new(&niri_sock),
        move |connected, state| {
            actor.send_niri_update(connected, state.clone());
        },
    );

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut overview_seen = false;
    while Instant::now() < deadline {
        if let Ok(resp) = client.call(&Request::Status) {
            if let Some(ResponseData::Status(st)) = resp.data {
                if st.niri_connected && st.overview_open {
                    overview_seen = true;
                    break;
                }
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(overview_seen, "expected niri overview_open=true in status");
    assert!(video.overview_history.lock().unwrap().contains(&true));
    assert!(renderer.take_ops().iter().any(|op| matches!(
        op,
        RecordedRenderOp::SetProgress {
            output: Some(out),
            ..
        } if out == "DP-1"
    )));

    // Modify settings.json atomically and verify inotify triggers automatic reload.
    set_transition_type(
        &paths.settings_file(),
        &paths.switch_lock_file(),
        TransitionType::Grow,
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut reloaded = false;
    while Instant::now() < deadline {
        if let Ok(resp) = client.call(&Request::Status) {
            if let Some(ResponseData::Status(st)) = resp.data {
                if st.transition.transition_type == TransitionType::Grow {
                    reloaded = true;
                    break;
                }
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(reloaded, "expected inotify to reload settings.json");

    watcher.stop();
    daemon.shutdown();
    let _ = niri_thread.join();
}

#[test]
fn startup_video_failure_falls_back_to_static_simple() {
    let tmp = TempDir::new("fallback");
    let paths = isolated_paths(tmp.path());
    let proj = tmp.path().join("proj");
    fs::create_dir_all(proj.join("wallpapers")).unwrap();
    fs::create_dir_all(proj.join("dynamic")).unwrap();

    let wp = proj.join("wallpapers/fallback.png");
    write_tiny_png(&wp);
    let bad_vid = proj.join("dynamic/broken.mp4");
    fs::write(&bad_vid, b"not a video").unwrap();

    atomic_write_text(&paths.engine_file(), "video").unwrap();
    atomic_write_text(&paths.current_wallpaper_file(), &wp.to_string_lossy()).unwrap();
    atomic_write_text(&paths.video_wallpaper_file(), &bad_vid.to_string_lossy()).unwrap();

    let renderer = Arc::new(MockRenderer::new());
    let video = Arc::new(MockVideo::new());
    *video.fail_next.lock().unwrap() = true;

    let opts = DaemonOptions {
        paths: paths.clone(),
        project_dir: proj,
        restore_on_start: true,
        enable_niri: false,
        enable_settings_watch: false,
    };
    let mut daemon = DaemonInstance::start_with_backends(opts, renderer.clone(), video)
        .expect("start daemon");

    let state = PersistedState::load(&paths);
    assert_eq!(state.engine, EngineKind::Simple);
    assert!(renderer.take_ops().iter().any(|op| matches!(
        op,
        RecordedRenderOp::ApplyFrame {
            effect: TransitionEffect::None,
            duration_ms: 0,
            ..
        }
    )));
    daemon.shutdown();
}

#[test]
fn real_video_backend_decodes_mp4_to_frame_input() {
    let tmp = TempDir::new("real-video");
    let mp4 = tmp.path().join("test.mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=160x90:r=15:d=0.4",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            mp4.to_str().unwrap(),
        ])
        .status()
        .expect("run ffmpeg");
    assert!(status.success());

    let renderer = Arc::new(MockRenderer::new());
    let video = RealVideoBackend::new(renderer);
    let ready = video
        .open_and_wait(&mp4, false, Duration::from_secs(10))
        .expect("open real mp4");
    assert_eq!(ready.fg_frame.width, 160);
    assert_eq!(ready.fg_frame.height, 90);
    video.activate_stream(ready.stream_id);
    assert!(video.is_playing_path(&mp4));
    video.shutdown();
}

#[test]
fn end_to_end_bgpickerctl_and_live_wayland_daemon() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return;
    }

    let tmp = TempDir::new("e2e-live");
    let root = tmp.path();
    let proj = root.join("proj");
    fs::create_dir_all(proj.join("wallpapers")).unwrap();
    fs::create_dir_all(proj.join("dynamic")).unwrap();

    let img = proj.join("wallpapers/e2e.png");
    write_tiny_png(&img);
    let mp4 = proj.join("dynamic/e2e.mp4");
    let ff = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=green:s=128x72:r=15:d=0.4",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            mp4.to_str().unwrap(),
        ])
        .status()
        .expect("run ffmpeg");
    assert!(ff.success());

    let sock = root.join("e2e.sock");
    let ctl_bin = env!("CARGO_BIN_EXE_bgpickerctl");
    let daemon_bin = env!("CARGO_BIN_EXE_bgpicker-daemon");

    let run_ctl = |args: &[&str]| {
        Command::new(ctl_bin)
            .args(["--socket", sock.to_str().unwrap(), "--project-dir", proj.to_str().unwrap()])
            .args(args)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env_remove("NIRI_SOCKET")
            .env("BGPICKER_DAEMON", daemon_bin)
            .output()
            .expect("run bgpickerctl")
    };

    // Ensure cleanup even on panic.
    struct StopGuard<'a>(&'a str, &'a Path, &'a Path, &'a Path);
    impl Drop for StopGuard<'_> {
        fn drop(&mut self) {
            let _ = Command::new(self.0)
                .args(["--socket", self.1.to_str().unwrap(), "stop"])
                .env("HOME", self.2.join("home"))
                .env("XDG_CACHE_HOME", self.2.join("cache"))
                .env("XDG_STATE_HOME", self.2.join("state"))
                .env("XDG_CONFIG_HOME", self.2.join("config"))
                .env("BGPICKER_DAEMON", self.3)
                .status();
        }
    }
    let _guard = StopGuard(ctl_bin, &sock, root, Path::new(daemon_bin));

    // status --no-start fails cleanly while daemon is not running.
    let st_off = run_ctl(&["status", "--no-start"]);
    assert!(!st_off.status.success());

    // apply-static auto-starts daemon and renders on live Wayland outputs.
    let out = run_ctl(&["apply-static", img.to_str().unwrap(), "--mode", "parallax"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let mode_out = run_ctl(&["mode"]);
    assert!(mode_out.status.success());
    assert_eq!(String::from_utf8_lossy(&mode_out.stdout).trim(), "parallax");

    let bp_out = run_ctl(&["backdrop-parallax", "on"]);
    assert!(bp_out.status.success());

    let tr_set = run_ctl(&["transition", "wipe"]);
    assert!(tr_set.status.success());
    let tr_get = run_ctl(&["transition"]);
    assert!(tr_get.status.success());
    let tr_val: serde_json::Value = serde_json::from_slice(&tr_get.stdout).unwrap();
    assert_eq!(tr_val["type"], "wipe");

    // Switch to live video and check video status + next.
    let vid_out = run_ctl(&["apply-video", "normal", mp4.to_str().unwrap()]);
    assert!(vid_out.status.success(), "stderr: {}", String::from_utf8_lossy(&vid_out.stderr));

    let st_json = run_ctl(&["status", "--json", "--no-start"]);
    assert!(st_json.status.success());
    let st_val: serde_json::Value = serde_json::from_slice(&st_json.stdout).unwrap();
    assert_eq!(st_val["engine"], "video");
    assert_eq!(st_val["videoMode"], "normal");
    assert!(!st_val["outputs"].as_array().unwrap().is_empty());

    // Catalog prints valid JSON matching Carousel.qml contract.
    let cat_out = run_ctl(&["catalog"]);
    assert!(cat_out.status.success(), "stderr: {}", String::from_utf8_lossy(&cat_out.stderr));
    let cat_val: serde_json::Value = serde_json::from_slice(&cat_out.stdout).unwrap();
    assert_eq!(cat_val["images"].as_array().unwrap().len(), 1);
    assert_eq!(cat_val["videos"].as_array().unwrap().len(), 1);

    // Stop cleanly and verify socket is gone.
    let stop_out = run_ctl(&["stop"]);
    assert!(stop_out.status.success(), "stderr: {}", String::from_utf8_lossy(&stop_out.stderr));
    assert!(!sock.exists());
}

#[test]
fn foreground_backdrop_sync_custom_combos_and_overrides() {
    let tmp = TempDir::new("backdrop-sync");
    let paths = isolated_paths(tmp.path());
    let proj = tmp.path().join("proj");
    fs::create_dir_all(proj.join("wallpapers")).unwrap();
    fs::create_dir_all(proj.join("dynamic")).unwrap();

    let img_a = proj.join("wallpapers/a.png");
    let img_b = proj.join("wallpapers/b.png");
    write_tiny_png(&img_a);
    write_tiny_png(&img_b);
    let vid_a = proj.join("dynamic/a.mp4");
    let vid_b = proj.join("dynamic/b.mp4");
    fs::write(&vid_a, b"dummy-a").unwrap();
    fs::write(&vid_b, b"dummy-b").unwrap();

    let renderer = Arc::new(MockRenderer::new());
    let video = Arc::new(MockVideo::new());

    let opts = DaemonOptions {
        paths: paths.clone(),
        project_dir: proj.clone(),
        restore_on_start: false,
        enable_niri: false,
        enable_settings_watch: false,
    };
    let mut daemon = DaemonInstance::start_with_backends(opts, renderer.clone(), video.clone())
        .expect("start daemon");
    let client = IpcClient::new(daemon.socket_path());
    let ctl_bin = env!("CARGO_BIN_EXE_bgpickerctl");

    let run_ctl_sock = |args: &[&str]| {
        Command::new(ctl_bin)
            .args([
                "--socket",
                daemon.socket_path().to_str().unwrap(),
                "--project-dir",
                proj.to_str().unwrap(),
            ])
            .args(args)
            .env("HOME", tmp.path().join("home"))
            .env("XDG_CONFIG_HOME", tmp.path().join("config"))
            .env("XDG_CACHE_HOME", tmp.path().join("cache"))
            .env("XDG_STATE_HOME", tmp.path().join("state"))
            .env_remove("NIRI_SOCKET")
            .output()
            .expect("run bgpickerctl")
    };

    let fetch_status = || -> bgpicker_core::protocol::DaemonStatus {
        match client.call(&Request::Status).unwrap().data.unwrap() {
            ResponseData::Status(st) => *st,
            other => panic!("unexpected status: {other:?}"),
        }
    };

    // 1. Initial apply-static with explicit mode=awww -> backdrop=off.
    assert!(run_ctl_sock(&["apply-static", img_a.to_str().unwrap(), "--mode", "awww"])
        .status
        .success());
    let st = fetch_status();
    assert_eq!(st.mode, WallpaperMode::Awww);
    assert!(!st.backdrop_parallax);

    // 2. Default mode sync: awww <-> parallax animates LayerTargets::Both and persists both files.
    let _ = renderer.take_ops();
    assert!(run_ctl_sock(&["mode", "parallax"]).status.success());
    let st = fetch_status();
    assert_eq!(st.mode, WallpaperMode::Parallax);
    assert!(st.backdrop_parallax);
    assert_eq!(
        PersistedState::load(&paths).backdrop_parallax,
        BackdropParallax::On
    );
    assert!(renderer.take_ops().iter().any(|op| matches!(
        op,
        RecordedRenderOp::SetMode {
            parallax: true,
            duration_ms: Some(500),
            targets: LayerTargets::Both
        }
    )));

    // 3. Re-running `mode parallax` after manual `backdrop-parallax off` re-syncs backdrop=on.
    assert!(run_ctl_sock(&["backdrop-parallax", "off"]).status.success());
    assert!(!fetch_status().backdrop_parallax);
    let _ = renderer.take_ops();
    assert!(run_ctl_sock(&["mode", "parallax"]).status.success());
    assert!(fetch_status().backdrop_parallax);
    assert!(renderer.take_ops().iter().any(|op| matches!(
        op,
        RecordedRenderOp::SetMode {
            parallax: true,
            duration_ms: Some(500),
            targets: LayerTargets::BackdropOnly
        }
    )));

    // 4. Switch to awww via toggle -> both return to off.
    assert!(run_ctl_sock(&["mode", "toggle"]).status.success());
    let st = fetch_status();
    assert_eq!(st.mode, WallpaperMode::Awww);
    assert!(!st.backdrop_parallax);

    // 5. Custom combo `fg=awww + bg=on` survives pure `apply-static <new_img>` AND `restore`.
    assert!(run_ctl_sock(&["backdrop-parallax", "on"]).status.success());
    let _ = renderer.take_ops();
    assert!(run_ctl_sock(&["apply-static", img_b.to_str().unwrap()]).status.success());
    let st = fetch_status();
    assert_eq!(st.mode, WallpaperMode::Awww);
    assert!(st.backdrop_parallax);
    assert_eq!(
        PersistedState::load(&paths).backdrop_parallax,
        BackdropParallax::On
    );
    let ops = renderer.take_ops();
    assert!(ops.iter().any(|op| matches!(
        op,
        RecordedRenderOp::SetMode {
            parallax: false,
            targets: LayerTargets::WallpaperOnly,
            ..
        }
    )));
    assert!(ops.iter().any(|op| matches!(
        op,
        RecordedRenderOp::SetMode {
            parallax: true,
            targets: LayerTargets::BackdropOnly,
            ..
        }
    )));

    assert!(client.call(&Request::Restore).unwrap().ok);
    let st = fetch_status();
    assert_eq!(st.mode, WallpaperMode::Awww);
    assert!(st.backdrop_parallax);

    // 6. `--keep-backdrop` on `mode` and `apply-static` preserves custom combos in both directions.
    assert!(run_ctl_sock(&["backdrop-parallax", "off"]).status.success());
    assert!(run_ctl_sock(&["mode", "parallax", "--keep-backdrop"]).status.success());
    let st = fetch_status();
    assert_eq!(st.mode, WallpaperMode::Parallax);
    assert!(!st.backdrop_parallax);

    assert!(run_ctl_sock(&["mode", "toggle", "--keep-backdrop"]).status.success());
    let st = fetch_status();
    assert_eq!(st.mode, WallpaperMode::Awww);
    assert!(!st.backdrop_parallax);

    assert!(run_ctl_sock(&["backdrop-parallax", "on"]).status.success());
    assert!(run_ctl_sock(&["mode", "parallax", "--keep-backdrop"]).status.success());
    let st = fetch_status();
    assert_eq!(st.mode, WallpaperMode::Parallax);
    assert!(st.backdrop_parallax);

    assert!(run_ctl_sock(&[
        "apply-static",
        img_a.to_str().unwrap(),
        "--mode",
        "awww",
        "--backdrop",
        "on"
    ])
    .status
    .success());
    let st = fetch_status();
    assert_eq!(st.mode, WallpaperMode::Awww);
    assert!(st.backdrop_parallax);

    assert!(run_ctl_sock(&["apply-static", img_b.to_str().unwrap(), "--backdrop", "off"])
        .status
        .success());
    let st = fetch_status();
    assert_eq!(st.mode, WallpaperMode::Awww);
    assert!(!st.backdrop_parallax);

    // 7. Video mode sync + custom combo preservation + overrides.
    assert!(run_ctl_sock(&["video", "parallax", vid_a.to_str().unwrap()])
        .status
        .success());
    let st = fetch_status();
    assert_eq!(st.engine, EngineKind::Video);
    assert_eq!(st.video_mode, VideoMode::Parallax);
    assert!(st.backdrop_parallax);

    // `apply-video normal` (fast path) syncs backdrop_parallax=off.
    let _ = renderer.take_ops();
    assert!(run_ctl_sock(&["apply-video", "normal"]).status.success());
    let st = fetch_status();
    assert_eq!(st.video_mode, VideoMode::Normal);
    assert!(!st.backdrop_parallax);
    assert!(renderer.take_ops().iter().any(|op| matches!(
        op,
        RecordedRenderOp::SetMode {
            parallax: false,
            duration_ms: Some(500),
            targets: LayerTargets::Both
        }
    )));

    // Custom video combo `video normal + backdrop on` survives `video next` and `apply-video <new_vid>`.
    assert!(run_ctl_sock(&["backdrop-parallax", "on"]).status.success());
    assert!(run_ctl_sock(&["video", "next"]).status.success());
    let st = fetch_status();
    assert_eq!(st.video_mode, VideoMode::Normal);
    assert!(st.backdrop_parallax);
    assert_eq!(
        st.current_video.as_deref(),
        Some(fs::canonicalize(&vid_b).unwrap().as_path())
    );

    assert!(run_ctl_sock(&["apply-video", vid_a.to_str().unwrap()]).status.success());
    let st = fetch_status();
    assert_eq!(st.video_mode, VideoMode::Normal);
    assert!(st.backdrop_parallax);

    // `apply-video parallax --backdrop off` and `video normal --keep-backdrop`.
    assert!(run_ctl_sock(&["apply-video", "parallax", "--backdrop", "off"])
        .status
        .success());
    let st = fetch_status();
    assert_eq!(st.video_mode, VideoMode::Parallax);
    assert!(!st.backdrop_parallax);

    assert!(run_ctl_sock(&["video", "normal", "--keep-backdrop"]).status.success());
    let st = fetch_status();
    assert_eq!(st.video_mode, VideoMode::Normal);
    assert!(!st.backdrop_parallax);

    // 8. Switching Video -> Full via `SetMode` syncs backdrop_parallax to target_mode (unless overridden).
    assert!(run_ctl_sock(&["mode", "parallax"]).status.success());
    let st = fetch_status();
    assert_eq!(st.engine, EngineKind::Full);
    assert_eq!(st.mode, WallpaperMode::Parallax);
    assert!(st.backdrop_parallax);

    // And switching Video -> Full via `SetMode --backdrop off` honors explicit override.
    assert!(run_ctl_sock(&["apply-video", "parallax"]).status.success());
    assert!(run_ctl_sock(&["mode", "parallax", "--backdrop", "off"])
        .status
        .success());
    let st = fetch_status();
    assert_eq!(st.engine, EngineKind::Full);
    assert_eq!(st.mode, WallpaperMode::Parallax);
    assert!(!st.backdrop_parallax);

    daemon.shutdown();
}

#[test]
fn ctl_simple_mode_lifecycle_rules_headless() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new("ctl-simple");
    let root = tmp.path();
    let paths = isolated_paths(root);
    let proj = root.join("proj");
    fs::create_dir_all(proj.join("wallpapers")).unwrap();
    fs::create_dir_all(proj.join("dynamic")).unwrap();
    let bin_dir = root.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let img_a = proj.join("wallpapers/a.png");
    let img_b = proj.join("wallpapers/b.png");
    write_tiny_png(&img_a);
    write_tiny_png(&img_b);
    let vid_a = proj.join("dynamic/a.mp4");
    fs::write(&vid_a, b"dummy").unwrap();

    let script_log = root.join("simple_calls.log");
    let qs_alive = root.join("qs.alive");
    fs::write(&qs_alive, "1").unwrap();

    let ctl_bin = env!("CARGO_BIN_EXE_bgpickerctl");
    let sock = paths.daemon_socket_path();

    // Fake `qs` binary so stop_quickshell_best_effort is observable without a real Quickshell session.
    let fake_qs = bin_dir.join("qs");
    fs::write(
        &fake_qs,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"ipc\" ] && [ \"$4\" = \"show\" ]; then\n  [ -f \"{0}\" ] && exit 0 || exit 1\nfi\nif [ \"$1\" = \"kill\" ]; then\n  rm -f \"{0}\"\n  exit 0\nfi\nexit 0\n",
            qs_alive.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_qs, fs::Permissions::from_mode(0o755)).unwrap();

    // Mock simple_wallpaper.sh recording args and whether the daemon socket was live during invocation.
    let simple_script = proj.join("simple_wallpaper.sh");
    let write_simple_script = |exit_code: i32| {
        let body = format!(
            "#!/bin/sh\nif [ -S \"{sock}\" ]; then live=1; else live=0; fi\nprintf '%s|live=%s\\n' \"$*\" \"$live\" >> \"{log}\"\nexit {code}\n",
            sock = sock.display(),
            log = script_log.display(),
            code = exit_code,
        );
        fs::write(&simple_script, body).unwrap();
        fs::set_permissions(&simple_script, fs::Permissions::from_mode(0o755)).unwrap();
    };
    write_simple_script(0);

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run_ctl = |args: &[&str]| {
        Command::new(ctl_bin)
            .args([
                "--socket",
                sock.to_str().unwrap(),
                "--project-dir",
                proj.to_str().unwrap(),
            ])
            .args(args)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_RUNTIME_DIR", root.join("runtime"))
            .env("PATH", &path_env)
            .env_remove("NIRI_SOCKET")
            .output()
            .expect("run bgpickerctl")
    };

    let spawn_mock_daemon = |restore_on_start: bool| {
        let renderer = Arc::new(MockRenderer::new());
        let video = Arc::new(MockVideo::new());
        let opts = DaemonOptions {
            paths: paths.clone(),
            project_dir: proj.clone(),
            restore_on_start,
            enable_niri: false,
            enable_settings_watch: false,
        };
        let mut daemon = DaemonInstance::start_with_backends(opts, renderer.clone(), video)
            .expect("start mock daemon");
        let stop_sig = daemon.stop_notifier();
        let handle = thread::spawn(move || {
            let (lock, cv) = &*stop_sig;
            let mut stopped = lock.lock().unwrap();
            while !*stopped {
                stopped = cv.wait(stopped).unwrap();
            }
            drop(stopped);
            daemon.shutdown();
        });
        (renderer, handle)
    };

    // Start daemon in Full mode and apply img_a.
    let (_renderer, daemon_thread) = spawn_mock_daemon(false);
    let out = run_ctl(&["apply-static", img_a.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    // Rule (3) failure case: failing simple_wallpaper.sh leaves engine=Full and daemon live.
    write_simple_script(2);
    let fail_simple = run_ctl(&["engine", "simple"]);
    assert!(!fail_simple.status.success());
    assert_eq!(PersistedState::load(&paths).engine, EngineKind::Full);
    assert!(run_ctl(&["status", "--no-start"]).status.success());

    // Rule (3) success case: `engine simple` stops qs, runs simple_wallpaper.sh WHILE daemon is live,
    // persists engine=simple, then stops bgpicker-daemon cleanly.
    write_simple_script(0);
    fs::write(&qs_alive, "1").unwrap();
    let _ = fs::remove_file(&script_log);
    let out = run_ctl(&["engine", "simple"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    daemon_thread.join().unwrap();

    assert!(!qs_alive.exists(), "expected qs to be stopped");
    assert!(!sock.exists(), "expected daemon socket to be removed");
    assert!(!run_ctl(&["status", "--no-start"]).status.success());
    assert_eq!(PersistedState::load(&paths).engine, EngineKind::Simple);
    let canonical_a = fs::canonicalize(&img_a).unwrap();
    let canonical_b = fs::canonicalize(&img_b).unwrap();
    let log_text = fs::read_to_string(&script_log).unwrap();
    assert_eq!(
        log_text.trim(),
        format!("{}|live=1", canonical_a.display()),
        "simple_wallpaper.sh must run BEFORE stopping daemon"
    );

    // Rule (1): `engine status` when daemon is not live and engine==Simple returns "simple" without starting daemon.
    let st_out = run_ctl(&["engine", "status"]);
    assert!(st_out.status.success());
    assert_eq!(String::from_utf8_lossy(&st_out.stdout).trim(), "simple");
    assert!(!sock.exists(), "engine status must not start daemon in simple mode");

    // Rule (2): `startup` when engine==Simple runs simple_wallpaper.sh without starting daemon.
    let _ = fs::remove_file(&script_log);
    let startup_out = run_ctl(&["startup"]);
    assert!(startup_out.status.success(), "stderr: {}", String::from_utf8_lossy(&startup_out.stderr));
    assert!(!sock.exists(), "startup in simple mode must not start daemon");
    assert_eq!(
        fs::read_to_string(&script_log).unwrap().trim(),
        format!("{}|live=0", canonical_a.display())
    );

    // Rule (2): `startup` with missing current_wallpaper skips simple_wallpaper.sh and does not start daemon.
    let _ = fs::remove_file(&script_log);
    atomic_write_text(&paths.current_wallpaper_file(), "/nonexistent/missing.png").unwrap();
    let startup_missing = run_ctl(&["startup"]);
    assert!(startup_missing.status.success());
    assert!(!sock.exists());
    assert!(!script_log.exists());

    // Rule (3): `engine simple` with invalid current_wallpaper fails cleanly without touching state or script.
    atomic_write_text(&paths.engine_file(), "full").unwrap();
    let fail_missing = run_ctl(&["engine", "simple"]);
    assert!(!fail_missing.status.success());
    assert_eq!(PersistedState::load(&paths).engine, EngineKind::Full);
    assert!(!script_log.exists());

    // Restore valid current_wallpaper and engine=simple.
    atomic_write_text(&paths.current_wallpaper_file(), &canonical_a.to_string_lossy()).unwrap();
    atomic_write_text(&paths.engine_file(), "simple").unwrap();

    // Rule (5): `stop` also runs `simple_wallpaper.sh --stop`.
    let stop_out = run_ctl(&["stop"]);
    assert!(stop_out.status.success());
    assert_eq!(
        fs::read_to_string(&script_log).unwrap().trim(),
        "--stop|live=0"
    );

    // Rule (4): Leaving simple (`engine toggle` when Simple, `engine full`, `engine video`, `apply-static`, `apply-video`)
    // ensures daemon applies wallpaper first, then calls `simple_wallpaper.sh --stop`.
    let (renderer, daemon_thread) = spawn_mock_daemon(false);
    let _ = fs::remove_file(&script_log);
    let toggle_out = run_ctl(&["engine", "toggle"]);
    assert!(toggle_out.status.success(), "stderr: {}", String::from_utf8_lossy(&toggle_out.stderr));
    assert_eq!(PersistedState::load(&paths).engine, EngineKind::Full);
    assert!(renderer
        .take_ops()
        .iter()
        .any(|op| matches!(op, RecordedRenderOp::ApplyFrame { .. })));
    assert_eq!(
        fs::read_to_string(&script_log).unwrap().trim(),
        "--stop|live=1"
    );

    // Rule (3) via `engine toggle` from Full -> Simple.
    let _ = fs::remove_file(&script_log);
    let toggle_back = run_ctl(&["engine", "toggle"]);
    assert!(toggle_back.status.success(), "stderr: {}", String::from_utf8_lossy(&toggle_back.stderr));
    daemon_thread.join().unwrap();
    assert!(!sock.exists());
    assert_eq!(PersistedState::load(&paths).engine, EngineKind::Simple);
    assert_eq!(
        fs::read_to_string(&script_log).unwrap().trim(),
        format!("{}|live=1", canonical_a.display())
    );

    // Rule (4) via `apply-static` and `apply-video` from Simple.
    let (renderer, daemon_thread) = spawn_mock_daemon(false);
    let _ = fs::remove_file(&script_log);
    assert!(run_ctl(&["apply-static", img_b.to_str().unwrap()]).status.success());
    assert!(renderer
        .take_ops()
        .iter()
        .any(|op| matches!(op, RecordedRenderOp::ApplyFrame { .. })));
    assert_eq!(
        fs::read_to_string(&script_log).unwrap().trim(),
        "--stop|live=1"
    );

    // Enter simple again from Full, then leave via `apply-video` and `engine video`.
    assert!(run_ctl(&["engine", "simple"]).status.success());
    daemon_thread.join().unwrap();
    assert_eq!(PersistedState::load(&paths).engine, EngineKind::Simple);

    let (renderer, daemon_thread) = spawn_mock_daemon(false);
    let _ = fs::remove_file(&script_log);
    assert!(run_ctl(&["apply-video", "normal", vid_a.to_str().unwrap()]).status.success());
    assert!(renderer
        .take_ops()
        .iter()
        .any(|op| matches!(op, RecordedRenderOp::ApplyFrame { .. })));
    assert_eq!(
        fs::read_to_string(&script_log).unwrap().trim(),
        "--stop|live=1"
    );

    // Enter simple from Video via `engine toggle`, then leave via `engine video`.
    let _ = fs::remove_file(&script_log);
    assert!(run_ctl(&["engine", "toggle"]).status.success());
    daemon_thread.join().unwrap();
    assert_eq!(PersistedState::load(&paths).engine, EngineKind::Simple);
    assert_eq!(
        fs::read_to_string(&script_log).unwrap().trim(),
        format!("{}|live=1", canonical_b.display())
    );

    let (renderer, daemon_thread) = spawn_mock_daemon(false);
    let _ = fs::remove_file(&script_log);
    assert!(run_ctl(&["engine", "video"]).status.success());
    assert!(renderer
        .take_ops()
        .iter()
        .any(|op| matches!(op, RecordedRenderOp::ApplyFrame { .. })));
    assert_eq!(
        fs::read_to_string(&script_log).unwrap().trim(),
        "--stop|live=1"
    );

    // Cleanly stop daemon without simple_wallpaper.sh script present (allowed skip case).
    fs::remove_file(&simple_script).unwrap();
    assert!(run_ctl(&["engine", "simple"]).status.success());
    daemon_thread.join().unwrap();
    assert!(!sock.exists());
    assert_eq!(PersistedState::load(&paths).engine, EngineKind::Simple);
}

#[test]
fn end_to_end_live_wayland_simple_engine_handoff() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return;
    }

    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new("e2e-simple");
    let root = tmp.path();
    let proj = root.join("proj");
    fs::create_dir_all(proj.join("wallpapers")).unwrap();
    fs::create_dir_all(proj.join("dynamic")).unwrap();

    let img = proj.join("wallpapers/e2e.png");
    write_tiny_png(&img);
    let canonical_img = fs::canonicalize(&img).unwrap();

    let sock = root.join("e2e-simple.sock");
    let script_log = root.join("simple_calls.log");
    let simple_script = proj.join("simple_wallpaper.sh");
    fs::write(
        &simple_script,
        format!(
            "#!/bin/sh\nif [ -S \"{sock}\" ]; then live=1; else live=0; fi\nprintf '%s|live=%s\\n' \"$*\" \"$live\" >> \"{log}\"\n",
            sock = sock.display(),
            log = script_log.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&simple_script, fs::Permissions::from_mode(0o755)).unwrap();

    let ctl_bin = env!("CARGO_BIN_EXE_bgpickerctl");
    let daemon_bin = env!("CARGO_BIN_EXE_bgpicker-daemon");

    struct StopGuard<'a>(&'a str, &'a Path, &'a Path, &'a Path, &'a Path);
    impl Drop for StopGuard<'_> {
        fn drop(&mut self) {
            let _ = Command::new(self.0)
                .args([
                    "--socket",
                    self.1.to_str().unwrap(),
                    "--project-dir",
                    self.2.to_str().unwrap(),
                    "stop",
                ])
                .env("HOME", self.3.join("home"))
                .env("XDG_CACHE_HOME", self.3.join("cache"))
                .env("XDG_STATE_HOME", self.3.join("state"))
                .env("XDG_CONFIG_HOME", self.3.join("config"))
                .env("BGPICKER_DAEMON", self.4)
                .status();
        }
    }
    let _guard = StopGuard(ctl_bin, &sock, &proj, root, Path::new(daemon_bin));

    let run_ctl = |args: &[&str]| {
        Command::new(ctl_bin)
            .args([
                "--socket",
                sock.to_str().unwrap(),
                "--project-dir",
                proj.to_str().unwrap(),
            ])
            .args(args)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env_remove("NIRI_SOCKET")
            .env("BGPICKER_DAEMON", daemon_bin)
            .output()
            .expect("run bgpickerctl")
    };

    // Start in full mode, switch to simple, verify daemon exited and status reports simple without waking daemon.
    assert!(run_ctl(&["apply-static", img.to_str().unwrap()]).status.success());
    let _ = fs::remove_file(&script_log);
    assert!(run_ctl(&["engine", "simple"]).status.success());
    assert!(!sock.exists());
    assert_eq!(
        fs::read_to_string(&script_log).unwrap().trim(),
        format!("{}|live=1", canonical_img.display())
    );
    let st = run_ctl(&["engine", "status"]);
    assert!(st.status.success());
    assert_eq!(String::from_utf8_lossy(&st.stdout).trim(), "simple");
    assert!(!sock.exists());

    // Leave simple via `engine full` -> auto-starts real daemon, presents wallpaper, then stops simple script.
    let _ = fs::remove_file(&script_log);
    let full_out = run_ctl(&["engine", "full"]);
    assert!(full_out.status.success(), "stderr: {}", String::from_utf8_lossy(&full_out.stderr));
    assert!(run_ctl(&["status", "--no-start"]).status.success());
    assert_eq!(
        fs::read_to_string(&script_log).unwrap().trim(),
        "--stop|live=1"
    );

    assert!(run_ctl(&["stop"]).status.success());
    assert!(!sock.exists());
}
