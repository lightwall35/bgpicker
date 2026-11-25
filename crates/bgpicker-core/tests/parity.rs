use bgpicker_core::{
    catalog::{
        image_thumbnail_cache_key, scan_media_catalog_with, video_thumbnail_cache_key,
        CatalogBundle, CatalogError, IMAGE_EXTENSIONS, VIDEO_EXTENSIONS,
    },
    config::{
        normalize_parallax, normalize_transition, read_settings_json, resolve_transition,
        resolve_transition_with_rng, set_transition_type, TransitionType,
    },
    math::{file_url, geometry, mode_geometry},
    niri::{NiriClientConfig, NiriEvent, NiriState, NiriWatcher, Window, WindowLayout, Workspace},
    paths::{
        atomic_write_text, BackdropParallax, BgPickerPaths, EngineKind, PersistedState, VideoMode,
        WallpaperMode,
    },
    protocol::{
        read_request, write_response, DaemonStatus, IpcClient, OutputStatus, Request, Response,
        ResponseData,
    },
};
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bgpicker-core-test-{prefix}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
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

fn make_window(
    id: u64,
    workspace_id: u64,
    col: Option<i64>,
    focused: bool,
    floating: bool,
) -> Window {
    Window {
        id,
        workspace_id: Some(workspace_id),
        is_focused: focused,
        is_floating: floating,
        layout: Some(WindowLayout {
            pos_in_scrolling_layout: col.map(|c| vec![c, 1]),
        }),
    }
}

fn niri_fixture() -> NiriState {
    let s = NiriState::initial().reduce(NiriEvent::WorkspacesChanged {
        workspaces: vec![
            Workspace {
                id: 1,
                idx: 1,
                name: None,
                output: Some("DP-1".into()),
                is_urgent: false,
                is_active: true,
                is_focused: true,
                active_window_id: Some(2),
            },
            Workspace {
                id: 2,
                idx: 2,
                name: None,
                output: Some("DP-1".into()),
                is_urgent: false,
                is_active: false,
                is_focused: false,
                active_window_id: Some(4),
            },
            Workspace {
                id: 3,
                idx: 1,
                name: None,
                output: Some("DP-2".into()),
                is_urgent: false,
                is_active: true,
                is_focused: false,
                active_window_id: Some(5),
            },
        ],
    });
    s.reduce(NiriEvent::WindowsChanged {
        windows: vec![
            make_window(1, 1, Some(1), false, false),
            make_window(2, 1, Some(3), true, false),
            make_window(3, 1, Some(3), false, false),
            make_window(4, 2, Some(1), false, false),
            make_window(5, 3, Some(1), false, false),
        ],
    })
}

#[test]
fn math_panorama_covers_without_upscaling_zoom() {
    let g = geometry(1920.0, 1080.0, 7200.0, 2400.0, 1.12);
    assert_eq!(g.width, 3240.0);
    assert_eq!(g.height, 1080.0);
    assert_eq!(g.overflow_x, 1320.0);
    assert_eq!(g.y, 0.0);
}

#[test]
fn math_mode_geometry_interpolates_between_center_and_parallax() {
    let centered = mode_geometry(1920.0, 1080.0, 7200.0, 2400.0, 1.12, 0.0, 0.0);
    let halfway = mode_geometry(1920.0, 1080.0, 7200.0, 2400.0, 1.12, 0.0, 0.5);
    let parallax = mode_geometry(1920.0, 1080.0, 7200.0, 2400.0, 1.12, 0.0, 1.0);
    assert_eq!(centered.x, -660.0);
    assert_eq!(parallax.x, 0.0);
    assert_eq!(halfway.x, -330.0);
    assert_eq!(centered.width, 3240.0);
    assert_eq!(parallax.width, 3240.0);

    let ordinary = mode_geometry(1920.0, 1080.0, 1920.0, 1080.0, 1.12, 1.0, 0.0);
    let enlarged = mode_geometry(1920.0, 1080.0, 1920.0, 1080.0, 1.12, 1.0, 1.0);
    assert_eq!(ordinary.width, 1920.0);
    assert!(enlarged.width > ordinary.width);
    assert!(enlarged.x < ordinary.x && enlarged.y < ordinary.y);
}

#[test]
fn math_ordinary_and_portrait_never_expose_empty_edges() {
    for [iw, ih] in [[1920.0, 1080.0], [1000.0, 2000.0], [0.0, 0.0]] {
        let g = geometry(1920.0, 1080.0, iw, ih, 1.12);
        assert!(g.width >= 1920.0 && g.height >= 1080.0);
        assert!(g.overflow_x > 0.0 && g.y < 0.0);
        assert!(-g.overflow_x + g.width >= 1920.0);
    }
}

#[test]
fn math_file_url_escapes_special_characters() {
    assert_eq!(
        file_url("/tmp/中文 #%.png"),
        "file:///tmp/%E4%B8%AD%E6%96%87%20%23%25.png"
    );
}

#[test]
fn niri_columns_are_deduplicated_and_ranked_not_raw_indices() {
    let s = niri_fixture();
    assert!((s.progress("DP-1", 6) - 0.2).abs() < 1e-12);
    assert!((s.progress("DP-1", 0) - 1.0).abs() < 1e-12);
    assert!((s.progress("DP-2", 6) - 0.0).abs() < 1e-12);
    assert!((s.progress("unplugged", 6) - 0.5).abs() < 1e-12);
}

#[test]
fn niri_floating_focus_remembers_previous_tiled_column() {
    let mut s = niri_fixture();
    s = s.reduce(NiriEvent::WindowOpenedOrChanged {
        window: make_window(9, 1, None, true, true),
    });
    s = s.reduce(NiriEvent::WorkspaceActiveWindowChanged {
        workspace_id: 1,
        active_window_id: Some(9),
    });
    assert!((s.progress("DP-1", 6) - 0.2).abs() < 1e-12);
    s = s.reduce(NiriEvent::WindowClosed { id: 2 });
    s = s.reduce(NiriEvent::WindowClosed { id: 3 });
    assert!((s.progress("DP-1", 6) - 0.0).abs() < 1e-12);
}

#[test]
fn niri_workspace_activation_affects_only_its_output() {
    let s = niri_fixture().reduce(NiriEvent::WorkspaceActivated {
        id: 2,
        focused: true,
    });
    assert!(!s.workspaces[0].is_active);
    assert!(s.workspaces[1].is_active);
    assert!(s.workspaces[2].is_active);
    assert!((s.progress("DP-1", 6) - 0.0).abs() < 1e-12);
}

#[test]
fn niri_layout_event_moves_focus_and_empty_workspace_centers() {
    let mut s = niri_fixture();
    s = s.reduce(NiriEvent::WindowLayoutsChanged {
        changes: vec![(
            2,
            WindowLayout {
                pos_in_scrolling_layout: Some(vec![5, 1]),
            },
        )],
    });
    assert!((s.progress("DP-1", 6) - 0.4).abs() < 1e-12);
    s = s.reduce(NiriEvent::WindowsChanged { windows: vec![] });
    assert!((s.progress("DP-1", 6) - 0.5).abs() < 1e-12);
}

#[test]
fn niri_unknown_events_and_overview_tracking() {
    let mut s = niri_fixture();
    let unchanged = s
        .reduce_json_line(r#"{"NewFutureEvent":{"extra":42}}"#)
        .expect("unknown event should parse");
    assert_eq!(unchanged, s);

    s = s
        .reduce_json_line(r#"{"OverviewOpenedOrClosed":{"is_open":true}}"#)
        .unwrap();
    assert!(s.overview_open);

    s = s
        .reduce_json_line(r#"{"WorkspacesChanged":{"workspaces":[]}}"#)
        .unwrap();
    assert!(s.last_columns.is_empty());
    assert!(s.overview_open);

    s = s
        .reduce_json_line(r#"{"OverviewOpenedOrClosed":{"is_open":false}}"#)
        .unwrap();
    assert!(!s.overview_open);
}

#[test]
fn niri_socket_streams_events_resets_state_and_reconnects() {
    let tmp = TempDir::new("niri-sock");
    let sock_path = tmp.path().join("niri.sock");
    let listener = UnixListener::bind(&sock_path).expect("bind mock niri socket");

    let server = thread::spawn(move || {
        // First connection: send handshake, workspaces, overview open, then drop.
        {
            let (stream, _) = listener.accept().expect("accept 1");
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut req = String::new();
            reader.read_line(&mut req).unwrap();
            assert_eq!(req.trim(), "\"EventStream\"");
            writeln!(writer, r#"{{"Ok":"Handled"}}"#).unwrap();
            writeln!(
                writer,
                r#"{{"WorkspacesChanged":{{"workspaces":[{{"id":10,"output":"DP-1","is_active":true}}]}}}}"#
            )
            .unwrap();
            writeln!(writer, r#"{{"OverviewOpenedOrClosed":{{"is_open":true}}}}"#).unwrap();
            writer.flush().unwrap();
            thread::sleep(Duration::from_millis(80));
        }

        // Second connection (after disconnect): state must reset cleanly before new snapshot.
        let (stream, _) = listener.accept().expect("accept 2");
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut req = String::new();
        reader.read_line(&mut req).unwrap();
        assert_eq!(req.trim(), "\"EventStream\"");
        writeln!(writer, r#"{{"Ok":"Handled"}}"#).unwrap();
        writeln!(
            writer,
            r#"{{"WorkspacesChanged":{{"workspaces":[{{"id":20,"output":"eDP-1","is_active":true}}]}}}}"#
        )
        .unwrap();
        writer.flush().unwrap();
        thread::sleep(Duration::from_millis(120));
    });

    let (tx, rx) = mpsc::channel();
    let mut cfg = NiriClientConfig::new(&sock_path);
    cfg.reconnect_interval = Duration::from_millis(40);
    cfg.read_timeout = Some(Duration::from_millis(40));

    let watcher = NiriWatcher::spawn(cfg, move |connected, state| {
        let _ = tx.send((connected, state.clone()));
    });

    let deadline = Instant::now() + Duration::from_secs(4);
    let mut saw_overview_open = false;
    let mut saw_reconnect_workspace = false;

    while Instant::now() < deadline && !(saw_overview_open && saw_reconnect_workspace) {
        if let Ok((connected, state)) = rx.recv_timeout(Duration::from_millis(50)) {
            if connected && state.overview_open && state.workspaces.iter().any(|w| w.id == 10) {
                saw_overview_open = true;
            }
            if connected
                && !state.overview_open
                && state.workspaces.len() == 1
                && state.workspaces[0].id == 20
            {
                saw_reconnect_workspace = true;
            }
        }
    }

    watcher.stop();
    server.join().unwrap();

    assert!(saw_overview_open, "never observed first stream overview_open");
    assert!(
        saw_reconnect_workspace,
        "never observed state reset + reconnected workspace"
    );
}

#[test]
fn config_parallax_bounds_match_wallpaper_math() {
    let p = normalize_parallax(&json!({
        "parallax": {
            "zoom": 100,
            "durationMs": -1,
            "columnSpan": 0,
            "maxTextureSize": "bad"
        }
    }));
    assert_eq!(p.zoom, 2.0);
    assert_eq!(p.duration_ms, 0);
    assert_eq!(p.column_span, 0);
    assert_eq!(p.max_texture_size, 8192);
}

#[test]
fn config_all_choices_persist_without_overwriting_other_settings() {
    let tmp = TempDir::new("cfg-persist");
    let settings = tmp.path().join("config/bgpicker/settings.json");
    let lock = tmp.path().join("cache/bgpicker/switch.lock");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        serde_json::to_string(&json!({
            "parallax": {"zoom": 1.25},
            "custom": [1, 2],
            "transition": {"durationMs": 900, "angle": 70}
        }))
        .unwrap(),
    )
    .unwrap();

    for kind in TransitionType::ALL {
        set_transition_type(&settings, &lock, kind).unwrap();
        let raw = read_settings_json(&settings).unwrap();
        assert_eq!(raw["parallax"], json!({"zoom": 1.25}));
        assert_eq!(raw["custom"], json!([1, 2]));
        assert_eq!(
            raw["transition"],
            json!({"type": kind.as_str(), "durationMs": 900, "angle": 70})
        );
        assert_eq!(normalize_transition(&raw).transition_type, kind);
    }
}

#[test]
fn config_resolved_random_and_awww_args_describe_one_effect() {
    let raw = json!({"transition": {"type": "random"}});
    let mut rng = StdRng::seed_from_u64(42);
    for _ in 0..12 {
        let resolved = resolve_transition_with_rng(&raw, false, &mut rng);
        assert!(TransitionType::CONCRETE_EFFECTS.contains(&resolved.transition_type));
        assert_eq!(resolved.selected_type, TransitionType::Random);
        let args = resolved.awww_args();
        let type_idx = args.iter().position(|a| a == "--transition-type").unwrap();
        assert_eq!(args[type_idx + 1], resolved.transition_type.as_str());
        let pos_idx = args.iter().position(|a| a == "--transition-pos").unwrap();
        assert_eq!(args[pos_idx + 1], "0.500000,0.500000");
    }
}

#[test]
fn config_bad_json_is_reported_and_preserved() {
    let tmp = TempDir::new("cfg-bad");
    let settings = tmp.path().join("settings.json");
    let lock = tmp.path().join("switch.lock");
    fs::write(&settings, b"{invalid").unwrap();
    let before = fs::read(&settings).unwrap();
    assert!(set_transition_type(&settings, &lock, TransitionType::Wipe).is_err());
    assert_eq!(fs::read(&settings).unwrap(), before);
    assert!(read_settings_json(&settings).is_err());
}

#[test]
fn config_instant_and_zero_duration_resolve_to_none_without_mutating() {
    let raw = json!({"transition": {"type": "wave", "durationMs": 900}});
    let snap = resolve_transition(&raw, true);
    assert_eq!(snap.transition_type, TransitionType::None);
    assert_eq!(snap.duration_ms, 0);
    assert_eq!(snap.selected_type, TransitionType::Wave);
    assert_eq!(normalize_transition(&raw).transition_type, TransitionType::Wave);

    let zero_dur = resolve_transition(&json!({"transition": {"durationMs": 0}}), false);
    assert_eq!(zero_dur.transition_type, TransitionType::None);
}

#[test]
fn config_numeric_bounds_defaults_and_legacy_duration() {
    let val = normalize_transition(&json!({
        "transition": {
            "type": "bad",
            "durationMs": -20,
            "fps": 1000,
            "angle": "NaN",
            "position": [-2, 5],
            "wave": [0, 999]
        }
    }));
    assert_eq!(val.transition_type, TransitionType::Fade);
    assert_eq!(val.duration_ms, 0);
    assert_eq!(val.fps, 240);
    assert_eq!(val.angle, 45.0);
    assert_eq!(val.position, [0.0, 1.0]);
    assert_eq!(val.wave, [1.0, 500.0]);

    assert_eq!(
        normalize_transition(&json!({"parallax": {"transitionMs": 350}})).duration_ms,
        350
    );
    assert_eq!(normalize_transition(&json!({})).duration_ms, 800);
}

#[test]
fn config_concurrent_preference_writes_leave_complete_json() {
    let tmp = TempDir::new("cfg-concurrent");
    let settings = Arc::new(tmp.path().join("settings.json"));
    let lock = Arc::new(tmp.path().join("switch.lock"));
    fs::write(
        settings.as_path(),
        serde_json::to_string(&json!({"parallax": {"zoom": 1.3}, "untouched": true})).unwrap(),
    )
    .unwrap();

    let handles: Vec<_> = [TransitionType::Wave, TransitionType::Grow, TransitionType::Outer]
        .into_iter()
        .map(|kind| {
            let s = Arc::clone(&settings);
            let l = Arc::clone(&lock);
            thread::spawn(move || set_transition_type(&s, &l, kind).unwrap())
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let raw = read_settings_json(&settings).unwrap();
    assert_eq!(raw["untouched"], json!(true));
    assert_eq!(raw["parallax"], json!({"zoom": 1.3}));
    let chosen = raw["transition"]["type"].as_str().unwrap();
    assert!(["wave", "grow", "outer"].contains(&chosen));
}

#[test]
fn catalog_sorted_video_cards_and_failure_isolation() {
    let tmp = TempDir::new("cat-videos");
    let dir = tmp.path().join("videos");
    fs::create_dir_all(&dir).unwrap();
    for name in ["B.MP4", "a.webm", "雪 #100%.mov", "ignore.png", "notes.txt"] {
        fs::write(dir.join(name), b"file").unwrap();
    }
    fs::create_dir_all(dir.join("directory.mkv")).unwrap();
    let preview = tmp.path().join("预览 #1.jpg");
    fs::write(&preview, b"jpg").unwrap();

    let calls = AtomicUsize::new(0);
    let entries = scan_media_catalog_with(&dir, VIDEO_EXTENSIONS, "video", |_| {
        calls.fetch_add(1, Ordering::Relaxed);
        Ok(preview.clone())
    })
    .unwrap();

    assert_eq!(calls.load(Ordering::Relaxed), 3);
    let names: Vec<_> = entries.iter().map(|e| e.file_name.as_str()).collect();
    assert_eq!(names, vec!["a.webm", "B.MP4", "雪 #100%.mov"]);

    let card = entries.last().unwrap();
    assert_eq!(
        card.original_path,
        fs::canonicalize(dir.join("雪 #100%.mov"))
            .unwrap()
            .to_string_lossy()
    );
    assert!(card.file_url.contains("%23"));
    assert!(card.file_url.contains("%25"));
    assert_eq!(card.thumbnail_url, file_url(fs::canonicalize(&preview).unwrap()));
    assert_eq!(card.thumbnail_error, None);

    // Thumbnail failure does not hide broken video or block good ones.
    let err_dir = tmp.path().join("videos2");
    fs::create_dir_all(&err_dir).unwrap();
    fs::write(err_dir.join("broken.mp4"), b"v").unwrap();
    fs::write(err_dir.join("good.mkv"), b"v").unwrap();

    let entries = scan_media_catalog_with(&err_dir, VIDEO_EXTENSIONS, "video", |src| {
        if src.file_name().unwrap() == "broken.mp4" {
            Err(CatalogError::ThumbnailFailed("ffmpeg timeout".into()))
        } else {
            Ok(preview.clone())
        }
    })
    .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].thumbnail_url, "");
    assert!(entries[0].thumbnail_error.as_deref().unwrap().contains("ffmpeg timeout"));
    assert_eq!(entries[1].thumbnail_error, None);
    assert!(!entries[1].thumbnail_url.is_empty());
}

#[test]
fn catalog_missing_folder_is_empty_but_file_is_error_and_bundle_serializes() {
    let tmp = TempDir::new("cat-bundle");
    let missing = tmp.path().join("missing");
    assert!(scan_media_catalog_with(&missing, VIDEO_EXTENSIONS, "video", |_| unreachable!())
        .unwrap()
        .is_empty());

    let file = tmp.path().join("not_a_dir");
    fs::write(&file, b"x").unwrap();
    let err = scan_media_catalog_with(&file, VIDEO_EXTENSIONS, "video", |_| unreachable!())
        .unwrap_err();
    assert!(err.to_string().contains("not a directory"));

    let img_dir = tmp.path().join("images");
    fs::create_dir_all(&img_dir).unwrap();
    fs::write(img_dir.join("b.png"), b"png").unwrap();
    fs::write(img_dir.join("A.jpg"), b"jpg").unwrap();
    fs::write(img_dir.join("ignore.mp4"), b"mp4").unwrap();
    let img_thumb = tmp.path().join("img_thumb.jpg");
    fs::write(&img_thumb, b"t").unwrap();

    let images =
        scan_media_catalog_with(&img_dir, IMAGE_EXTENSIONS, "image", |_| Ok(img_thumb.clone()))
            .unwrap();
    let bundle = CatalogBundle {
        images,
        videos: vec![],
    };
    let serialized = serde_json::to_value(&bundle).unwrap();
    assert_eq!(serialized["images"][0]["fileName"], "A.jpg");
    assert_eq!(serialized["images"][1]["fileName"], "b.png");
    assert!(serialized["images"][0].get("thumbnailError").is_none());
}

#[test]
fn catalog_thumbnail_cache_key_changes_when_content_or_mtime_changes() {
    let tmp = TempDir::new("cat-key");
    let video = tmp.path().join("clip.mp4");
    fs::write(&video, b"one").unwrap();
    let k1 = video_thumbnail_cache_key(&video).unwrap();
    let k2 = video_thumbnail_cache_key(&video).unwrap();
    assert_eq!(k1, k2);
    assert_eq!(k1.len(), 64);

    let img_k = image_thumbnail_cache_key(&video).unwrap();
    assert_ne!(k1, img_k);

    fs::write(&video, b"changed-content").unwrap();
    let k3 = video_thumbnail_cache_key(&video).unwrap();
    assert_ne!(k1, k3);
}

#[test]
fn paths_and_persisted_state_roundtrip() {
    let tmp = TempDir::new("paths");
    let paths = BgPickerPaths::with_roots(
        tmp.path().join("home"),
        tmp.path().join("config"),
        tmp.path().join("cache"),
        tmp.path().join("state"),
        tmp.path().join("run"),
    );

    assert_eq!(
        paths.settings_file(),
        tmp.path().join("config/bgpicker/settings.json")
    );
    assert_eq!(
        paths.daemon_socket_path(),
        tmp.path().join("run/bgpicker-wayland-0.sock")
    );
    assert_eq!(
        paths.legacy_swaybg_cache_file(),
        tmp.path().join("home/.cache/current_swaybg_img")
    );

    atomic_write_text(&paths.engine_file(), "video").unwrap();
    atomic_write_text(&paths.mode_file(), "parallax").unwrap();
    atomic_write_text(&paths.backdrop_parallax_file(), "on").unwrap();
    atomic_write_text(&paths.current_wallpaper_file(), "/wallpapers/A.png").unwrap();
    atomic_write_text(&paths.video_wallpaper_file(), "/dynamic/clip.mp4").unwrap();
    atomic_write_text(&paths.video_mode_file(), "parallax").unwrap();

    let loaded = PersistedState::load(&paths);
    assert_eq!(loaded.engine, EngineKind::Video);
    assert_eq!(loaded.mode, WallpaperMode::Parallax);
    assert_eq!(loaded.backdrop_parallax, BackdropParallax::On);
    assert_eq!(
        loaded.current_wallpaper.as_deref(),
        Some(Path::new("/wallpapers/A.png"))
    );
    assert_eq!(
        loaded.video_wallpaper.as_deref(),
        Some(Path::new("/dynamic/clip.mp4"))
    );
    assert_eq!(loaded.video_mode, VideoMode::Parallax);
}

#[test]
fn protocol_ipc_roundtrip_over_unix_socket() {
    let tmp = TempDir::new("ipc");
    let sock = tmp.path().join("bgpicker.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let req = read_request(&mut reader).unwrap().unwrap();
            let resp = match req {
                Request::Ping => Response::ok(ResponseData::Pong),
                Request::Status => Response::ok(ResponseData::Status(Box::new(DaemonStatus {
                    engine: EngineKind::Full,
                    mode: WallpaperMode::Parallax,
                    current_wallpaper: Some(PathBuf::from("/tmp/wall.png")),
                    video_mode: VideoMode::Normal,
                    current_video: None,
                    backdrop_parallax: true,
                    transition: Default::default(),
                    parallax: Default::default(),
                    busy: false,
                    niri_connected: true,
                    overview_open: false,
                    outputs: vec![OutputStatus {
                        name: "DP-1".into(),
                        width: 1920,
                        height: 1080,
                        scale: 1.0,
                        progress: 0.2,
                        mode_blend: 1.0,
                        transition_active: false,
                    }],
                    error: None,
                }))),
                _ => Response::err("unexpected command"),
            };
            write_response(&mut stream, &resp).unwrap();
        }
    });

    let client = IpcClient::new(&sock);
    let ping = client.call(&Request::Ping).unwrap();
    assert!(ping.ok);
    assert_eq!(ping.data, Some(ResponseData::Pong));

    let status = client.call(&Request::Status).unwrap();
    assert!(status.ok);
    match status.data {
        Some(ResponseData::Status(st)) => {
            assert_eq!(st.mode, WallpaperMode::Parallax);
            assert!(st.niri_connected);
            assert_eq!(st.outputs.len(), 1);
            assert_eq!(st.outputs[0].name, "DP-1");
        }
        other => panic!("unexpected status payload: {other:?}"),
    }

    server.join().unwrap();
}
