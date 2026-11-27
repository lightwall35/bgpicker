use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bgpicker_core::{
    BgPickerPaths, CatalogEntry, EngineKind, PickerCommand, PickerIpcClient, ResponseData,
    TransitionType, VideoMode, VideoModeAction, WallpaperMode,
};
use bgpicker_picker::ipc::{
    CliParseError, PickerIpcListener, PickerStartAction, SingleInstanceOutcome,
    default_picker_response, is_picker_live, parse_cli_args,
};
use bgpicker_picker::wayland::{
    DaemonMutationJob, map_key_event_to_input, map_pointer_button, resolve_mode_toggle_job,
    resolve_picker_ipc_input, wayland_axis_to_qt_delta,
};
use bgpicker_picker::{
    DaemonStateSnapshot, KeyCode, KeyInput, Modifiers, MouseButton, Page, PickerActionOut,
    PickerConfig, PickerInput, PickerUi,
};
use calloop::EventLoop;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};
use smithay_client_toolkit::seat::pointer::{AxisScroll, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};

fn unique_test_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "bgpicker_shell_test_{tag}_{}_{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create temp test dir");
    dir
}

#[test]
fn cli_arg_parsing_defaults_overrides_and_errors() {
    let dir = unique_test_dir("cli");
    let paths = BgPickerPaths::with_roots(
        dir.join("home"),
        dir.join("config"),
        dir.join("cache"),
        dir.join("state"),
        dir.join("run"),
    );

    // Defaults when no flags provided
    let default_opts = parse_cli_args(Vec::<String>::new(), &paths).expect("default parse");
    assert_eq!(default_opts.daemon_socket, paths.daemon_socket_path());
    assert_eq!(default_opts.picker_socket, paths.picker_socket_path());
    assert_eq!(default_opts.action, PickerStartAction::Open);

    // Explicit space-separated flags matching bgpickerctl spawn contract (#54)
    let explicit = parse_cli_args(
        [
            "--project-dir",
            "/opt/bgpicker",
            "--daemon-socket",
            "/run/user/1000/bg.sock",
            "--picker-socket",
            "/run/user/1000/picker.sock",
            "--action",
            "cycle-next",
        ],
        &paths,
    )
    .expect("explicit flags");
    assert_eq!(explicit.project_dir, PathBuf::from("/opt/bgpicker"));
    assert_eq!(explicit.daemon_socket, PathBuf::from("/run/user/1000/bg.sock"));
    assert_eq!(explicit.picker_socket, PathBuf::from("/run/user/1000/picker.sock"));
    assert_eq!(explicit.action, PickerStartAction::CycleNext);
    assert_eq!(explicit.action.to_command(), PickerCommand::CycleNext);

    // Equals-form flags and remaining actions
    for (raw, expected_action, expected_cmd) in [
        ("open", PickerStartAction::Open, PickerCommand::Open),
        ("toggle", PickerStartAction::Toggle, PickerCommand::Toggle),
        ("cycle-next", PickerStartAction::CycleNext, PickerCommand::CycleNext),
        (
            "cycle-prev",
            PickerStartAction::CyclePrevious,
            PickerCommand::CyclePrevious,
        ),
        ("close", PickerStartAction::Close, PickerCommand::Close),
    ] {
        let parsed = parse_cli_args(
            [
                "--project-dir=/tmp/proj",
                "--daemon-socket=/tmp/d.sock",
                "--picker-socket=/tmp/p.sock",
                &format!("--action={raw}"),
            ],
            &paths,
        )
        .expect("equals flags");
        assert_eq!(parsed.action, expected_action);
        assert_eq!(parsed.action.to_command(), expected_cmd);
        assert_eq!(parsed.action.as_str(), raw);
    }

    // Help & invalid inputs
    assert!(matches!(
        parse_cli_args(["--help"], &paths),
        Err(CliParseError::Help(_))
    ));
    assert!(matches!(
        parse_cli_args(["--action", "bogus"], &paths),
        Err(CliParseError::Invalid(_))
    ));
    assert!(matches!(
        parse_cli_args(["--picker-socket"], &paths),
        Err(CliParseError::Invalid(_))
    ));
    assert!(matches!(
        parse_cli_args(["--unknown-flag"], &paths),
        Err(CliParseError::Invalid(_))
    ));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn picker_ipc_server_handles_all_commands_mode_0600_and_cleans_up_socket() {
    let dir = unique_test_dir("ipc_server");
    let sock = dir.join("picker.sock");

    let listener = match PickerIpcListener::acquire_or_forward(sock.clone(), PickerCommand::Open)
        .expect("acquire fresh socket")
    {
        SingleInstanceOutcome::Acquired(l) => l,
        _ => panic!("expected Acquired on fresh socket"),
    };

    // Socket permissions must be strictly 0600
    let mode = fs::metadata(&sock).expect("socket metadata").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "picker socket must be mode 0600, got {mode:o}");

    let received = Arc::new(Mutex::new(Vec::<PickerCommand>::new()));
    let recv_thread = Arc::clone(&received);

    let server = thread::spawn(move || {
        let mut event_loop = EventLoop::<bool>::try_new().expect("calloop");
        let signal = event_loop.get_signal();
        let (_token, _guard) = listener
            .register_in_calloop(&event_loop.handle(), move |should_stop, cmd| {
                recv_thread.lock().unwrap().push(cmd);
                let resp = default_picker_response(cmd);
                if cmd == PickerCommand::Close {
                    *should_stop = true;
                    signal.stop();
                }
                resp
            })
            .expect("register IPC listener in calloop");

        let mut should_stop = false;
        event_loop
            .run(Some(Duration::from_millis(50)), &mut should_stop, |_| {})
            .expect("calloop run");
        // `_guard` drops here and removes the socket file.
    });

    let client = PickerIpcClient::new(&sock).with_timeout(Duration::from_secs(2));

    // Ping -> Pong
    let pong = client.call(&PickerCommand::Ping).expect("ping");
    assert!(pong.ok);
    assert_eq!(pong.data, Some(ResponseData::Pong));

    // All control commands -> ok_empty()
    for cmd in [
        PickerCommand::Open,
        PickerCommand::Toggle,
        PickerCommand::CycleNext,
        PickerCommand::CyclePrevious,
        PickerCommand::Close,
    ] {
        let resp = client.call(&cmd).expect("send picker command");
        assert!(resp.ok, "expected ok for {cmd:?}, got {resp:?}");
        assert_eq!(resp.data, None);
    }

    server.join().expect("server thread exited cleanly");

    assert_eq!(
        *received.lock().unwrap(),
        vec![
            PickerCommand::Ping,
            PickerCommand::Open,
            PickerCommand::Toggle,
            PickerCommand::CycleNext,
            PickerCommand::CyclePrevious,
            PickerCommand::Close,
        ]
    );

    // Socket file must be unlinked on Drop
    assert!(
        !sock.exists(),
        "picker socket should be removed after server exit: {}",
        sock.display()
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn stale_socket_is_removed_and_close_on_dead_socket_exits_cleanly() {
    let dir = unique_test_dir("stale");
    let sock = dir.join("stale-picker.sock");

    // Create a dead Unix socket file (listener dropped without unlinking)
    {
        let dead = UnixListener::bind(&sock).expect("bind dead socket");
        drop(dead);
    }
    assert!(sock.exists());
    assert!(!is_picker_live(&sock));

    // Close on dead socket should clean up stale file and return AlreadyClosed
    match PickerIpcListener::acquire_or_forward(sock.clone(), PickerCommand::Close)
        .expect("close on stale socket")
    {
        SingleInstanceOutcome::AlreadyClosed => {}
        _ => panic!("expected AlreadyClosed when no live picker is running"),
    }
    assert!(!sock.exists(), "stale socket should be unlinked by close");

    // Create another stale file and verify Open removes it and binds cleanly
    fs::write(&sock, b"stale").expect("write stale file");
    let listener = match PickerIpcListener::acquire_or_forward(sock.clone(), PickerCommand::Open)
        .expect("acquire over stale file")
    {
        SingleInstanceOutcome::Acquired(l) => l,
        _ => panic!("expected Acquired after removing stale socket"),
    };
    assert_eq!(listener.socket_path(), sock.as_path());
    let mode = fs::metadata(&sock).expect("socket meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    // Dropping listener unlinks socket
    drop(listener);
    assert!(!sock.exists());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn single_instance_forwards_to_running_picker_socket() {
    let dir = unique_test_dir("forward");
    let sock = dir.join("live-picker.sock");

    let listener = PickerIpcListener::bind_fresh(sock.clone()).expect("bind live socket");
    let received = Arc::new(Mutex::new(Vec::<PickerCommand>::new()));
    let recv_thread = Arc::clone(&received);

    let server = thread::spawn(move || {
        let mut event_loop = EventLoop::<()>::try_new().expect("calloop");
        let signal = event_loop.get_signal();
        let (_token, _guard) = listener
            .register_in_calloop(&event_loop.handle(), move |_, cmd| {
                recv_thread.lock().unwrap().push(cmd);
                let resp = default_picker_response(cmd);
                if cmd == PickerCommand::Close {
                    signal.stop();
                }
                resp
            })
            .expect("register listener");
        event_loop
            .run(Some(Duration::from_millis(50)), &mut (), |_| {})
            .expect("run loop");
    });

    // Second instance checks Ping -> forwards CycleNext -> returns Forwarded(ok)
    match PickerIpcListener::acquire_or_forward(sock.clone(), PickerCommand::CycleNext)
        .expect("forward cycle-next")
    {
        SingleInstanceOutcome::Forwarded(resp) => assert!(resp.ok),
        _ => panic!("expected Forwarded for CycleNext"),
    }

    // Third instance forwards Toggle -> returns Forwarded(ok)
    match PickerIpcListener::acquire_or_forward(sock.clone(), PickerCommand::Toggle)
        .expect("forward toggle")
    {
        SingleInstanceOutcome::Forwarded(resp) => assert!(resp.ok),
        _ => panic!("expected Forwarded for Toggle"),
    }

    // Close forwards Close -> stops live server cleanly
    match PickerIpcListener::acquire_or_forward(sock.clone(), PickerCommand::Close)
        .expect("forward close")
    {
        SingleInstanceOutcome::Forwarded(resp) => assert!(resp.ok),
        _ => panic!("expected Forwarded for Close"),
    }

    server.join().expect("server thread exit");

    assert_eq!(
        *received.lock().unwrap(),
        vec![
            PickerCommand::Ping,
            PickerCommand::CycleNext,
            PickerCommand::Ping,
            PickerCommand::Toggle,
            PickerCommand::Ping,
            PickerCommand::Close,
        ]
    );
    assert!(!sock.exists());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn wayland_scroll_sign_and_key_mapping_match_picker_ui() {
    // Wayland scroll DOWN (+value120 / +discrete / +absolute) -> negative Qt dy -> Page::Video
    let down_v120 = AxisScroll {
        value120: 120,
        ..AxisScroll::default()
    };
    assert_eq!(wayland_axis_to_qt_delta(&down_v120), -120.0);

    let up_discrete = AxisScroll {
        discrete: -1,
        ..AxisScroll::default()
    };
    assert_eq!(wayland_axis_to_qt_delta(&up_discrete), 120.0);

    let down_touchpad = AxisScroll {
        absolute: 50.0,
        ..AxisScroll::default()
    };
    assert_eq!(wayland_axis_to_qt_delta(&down_touchpad), -50.0);

    // End-to-end check against PickerUi page switching
    let mut ui = PickerUi::new(PickerConfig::default(), 1920.0, 1080.0);
    ui.handle_input(PickerInput::SetCatalog {
        images: vec![CatalogEntry {
            file_name: "a.png".into(),
            file_url: "file:///a.png".into(),
            thumbnail_url: "file:///a.png".into(),
            original_path: "/a.png".into(),
            thumbnail_error: None,
        }],
        videos: vec![CatalogEntry {
            file_name: "v.mp4".into(),
            file_url: "file:///v.mp4".into(),
            thumbnail_url: "file:///v.jpg".into(),
            original_path: "/v.mp4".into(),
            thumbnail_error: None,
        }],
    });
    ui.handle_input(PickerInput::StartOpen);
    assert_eq!(ui.page(), Page::Static);

    ui.handle_input(PickerInput::PointerWheel {
        dx: 0.0,
        dy: wayland_axis_to_qt_delta(&down_v120),
    });
    assert_eq!(ui.page(), Page::Video);

    let mut actions = Vec::new();
    for _ in 0..40 {
        ui.tick(0.016, &mut actions);
    }

    ui.handle_input(PickerInput::PointerWheel {
        dx: 0.0,
        dy: wayland_axis_to_qt_delta(&up_discrete),
    });
    assert_eq!(ui.page(), Page::Static);

    // Ctrl+F with control-char utf8 maps cleanly to KeyInput(Char('f'), ctrl=true)
    let ctrl_f = KeyEvent {
        time: 0,
        raw_code: 33,
        keysym: Keysym::f,
        utf8: Some("\x06".into()),
    };
    let ctrl_mods = Modifiers {
        ctrl: true,
        shift: false,
        alt: false,
    };
    assert_eq!(
        map_key_event_to_input(&ctrl_f, ctrl_mods, false),
        Some(PickerInput::KeyInput(KeyInput {
            key: KeyCode::Char('f'),
            modifiers: ctrl_mods,
        }))
    );

    // Printable UTF-8 text in search mode produces TextInput (no double-entry)
    let cjk_evt = KeyEvent {
        time: 0,
        raw_code: 0,
        keysym: Keysym::NoSymbol,
        utf8: Some("壁纸".into()),
    };
    assert_eq!(
        map_key_event_to_input(&cjk_evt, Modifiers::default(), true),
        Some(PickerInput::TextInput("壁纸".into()))
    );

    // PageUp / PageDown switch Static / Video pages when search is closed
    let pgdn = KeyEvent {
        time: 0,
        raw_code: 0,
        keysym: Keysym::Page_Down,
        utf8: None,
    };
    let pgup = KeyEvent {
        time: 0,
        raw_code: 0,
        keysym: Keysym::Page_Up,
        utf8: None,
    };
    assert_eq!(
        map_key_event_to_input(&pgdn, Modifiers::default(), false),
        Some(PickerInput::ShowPage(Page::Video))
    );
    assert_eq!(
        map_key_event_to_input(&pgup, Modifiers::default(), false),
        Some(PickerInput::ShowPage(Page::Static))
    );

    // Pointer button mapping (evdev BTN_* -> MouseButton)
    assert_eq!(map_pointer_button(BTN_LEFT), Some(MouseButton::Left));
    assert_eq!(map_pointer_button(BTN_RIGHT), Some(MouseButton::Right));
    assert_eq!(map_pointer_button(BTN_MIDDLE), Some(MouseButton::Middle));
    assert_eq!(map_pointer_button(0xFFFF), None);
}

#[test]
fn resolve_mode_toggle_job_dispatches_video_vs_static_correctly() {
    let base = DaemonStateSnapshot {
        engine: EngineKind::Video,
        wallpaper_mode: WallpaperMode::Awww,
        video_mode: VideoMode::Parallax,
        transition_type: TransitionType::Fade,
        current_wallpaper: "/wallpapers/img.jpg".into(),
        current_video: "/dynamic/vid.mp4".into(),
        busy: false,
        error: String::new(),
    };

    // Video page with VideoMode::Parallax -> ApplyVideo(Normal, current_video), never ApplyStatic/ToggleStaticMode
    assert_eq!(
        resolve_mode_toggle_job(Page::Video, &base),
        Some(DaemonMutationJob::ApplyVideo {
            mode: VideoModeAction::Normal,
            path: PathBuf::from("/dynamic/vid.mp4"),
        })
    );

    // Video page with VideoMode::Normal -> ApplyVideo(Parallax, current_video)
    let mut video_normal = base.clone();
    video_normal.video_mode = VideoMode::Normal;
    assert_eq!(
        resolve_mode_toggle_job(Page::Video, &video_normal),
        Some(DaemonMutationJob::ApplyVideo {
            mode: VideoModeAction::Parallax,
            path: PathBuf::from("/dynamic/vid.mp4"),
        })
    );

    // Video page while static engine is active still dispatches ApplyVideo with toggled video_mode
    let mut full_on_video_page = video_normal.clone();
    full_on_video_page.engine = EngineKind::Full;
    assert_eq!(
        resolve_mode_toggle_job(Page::Video, &full_on_video_page),
        Some(DaemonMutationJob::ApplyVideo {
            mode: VideoModeAction::Parallax,
            path: PathBuf::from("/dynamic/vid.mp4"),
        })
    );

    // Video page with empty current_video -> None
    let mut empty_video = base.clone();
    empty_video.current_video.clear();
    assert_eq!(resolve_mode_toggle_job(Page::Video, &empty_video), None);

    // Static page with EngineKind::Full -> ToggleStaticMode
    let mut full_static = base.clone();
    full_static.engine = EngineKind::Full;
    assert_eq!(
        resolve_mode_toggle_job(Page::Static, &full_static),
        Some(DaemonMutationJob::ToggleStaticMode)
    );
    full_static.wallpaper_mode = WallpaperMode::Parallax;
    assert_eq!(
        resolve_mode_toggle_job(Page::Static, &full_static),
        Some(DaemonMutationJob::ToggleStaticMode)
    );

    // Static page while EngineKind::Video is active -> ApplyStatic with toggled wallpaper_mode
    assert_eq!(
        resolve_mode_toggle_job(Page::Static, &base),
        Some(DaemonMutationJob::ApplyStatic {
            path: PathBuf::from("/wallpapers/img.jpg"),
            mode: Some(WallpaperMode::Parallax),
        })
    );

    // Static page while EngineKind::Simple is active -> ApplyStatic with toggled wallpaper_mode
    let mut simple_static = base.clone();
    simple_static.engine = EngineKind::Simple;
    simple_static.wallpaper_mode = WallpaperMode::Parallax;
    assert_eq!(
        resolve_mode_toggle_job(Page::Static, &simple_static),
        Some(DaemonMutationJob::ApplyStatic {
            path: PathBuf::from("/wallpapers/img.jpg"),
            mode: Some(WallpaperMode::Awww),
        })
    );

    // Static page with empty current_wallpaper -> None
    let mut empty_static = full_static.clone();
    empty_static.current_wallpaper.clear();
    assert_eq!(resolve_mode_toggle_job(Page::Static, &empty_static), None);

    // End-to-end regression: opening picker during video wallpaper and clicking "普通"
    // emits ToggleMode on Page::Video -> resolves to ApplyVideo(Normal, current_video), NOT ApplyStatic!
    let mut ui = PickerUi::new(PickerConfig::default(), 1920.0, 1080.0);
    ui.handle_input(PickerInput::SetDaemonState(base.clone()));
    ui.handle_input(PickerInput::StartOpen);
    ui.handle_input(PickerInput::SetCatalog {
        images: vec![CatalogEntry {
            file_name: "img.jpg".into(),
            file_url: "file:///wallpapers/img.jpg".into(),
            thumbnail_url: "file:///wallpapers/img.jpg".into(),
            original_path: "/wallpapers/img.jpg".into(),
            thumbnail_error: None,
        }],
        videos: vec![CatalogEntry {
            file_name: "vid.mp4".into(),
            file_url: "file:///dynamic/vid.mp4".into(),
            thumbnail_url: "file:///dynamic/vid.jpg".into(),
            original_path: "/dynamic/vid.mp4".into(),
            thumbnail_error: None,
        }],
    });
    assert_eq!(ui.page(), Page::Video);
    let sel = ui.selector_layout();
    let acts = ui.handle_input(PickerInput::PointerClick {
        x: sel.mode_buttons[0].center_x,
        y: sel.mode_buttons[0].center_y,
        button: MouseButton::Left,
    });
    assert_eq!(acts, vec![PickerActionOut::ToggleMode]);
    assert_eq!(
        resolve_mode_toggle_job(ui.page(), &base),
        Some(DaemonMutationJob::ApplyVideo {
            mode: VideoModeAction::Normal,
            path: PathBuf::from("/dynamic/vid.mp4"),
        })
    );
}

#[test]
fn picker_ipc_command_resolution_supports_animated_close_and_interruptible_reopen() {
    // Closed state
    assert_eq!(resolve_picker_ipc_input(PickerCommand::Ping, false, false), None);
    assert_eq!(
        resolve_picker_ipc_input(PickerCommand::Open, false, false),
        Some(PickerInput::StartOpen)
    );
    assert_eq!(
        resolve_picker_ipc_input(PickerCommand::Toggle, false, false),
        Some(PickerInput::StartOpen)
    );
    assert_eq!(
        resolve_picker_ipc_input(PickerCommand::Close, false, false),
        Some(PickerInput::StartClose)
    );

    // Open (not closing) state
    assert_eq!(resolve_picker_ipc_input(PickerCommand::Open, true, false), None);
    assert_eq!(
        resolve_picker_ipc_input(PickerCommand::Toggle, true, false),
        Some(PickerInput::StartClose)
    );
    assert_eq!(
        resolve_picker_ipc_input(PickerCommand::Close, true, false),
        Some(PickerInput::StartClose)
    );

    // Closing state: both Open and Toggle smoothly interrupt close and reopen!
    assert_eq!(
        resolve_picker_ipc_input(PickerCommand::Open, true, true),
        Some(PickerInput::StartOpen)
    );
    assert_eq!(
        resolve_picker_ipc_input(PickerCommand::Toggle, true, true),
        Some(PickerInput::StartOpen)
    );
    assert_eq!(
        resolve_picker_ipc_input(PickerCommand::Close, true, true),
        Some(PickerInput::StartClose)
    );

    // End-to-end Toggle -> mid-close -> Toggle interrupt -> Close -> animated Exit
    let mut ui = PickerUi::new(PickerConfig::default(), 1920.0, 1080.0);
    ui.handle_input(PickerInput::SetCatalog {
        images: vec![CatalogEntry {
            file_name: "a.png".into(),
            file_url: "file:///a.png".into(),
            thumbnail_url: "file:///a.png".into(),
            original_path: "/a.png".into(),
            thumbnail_error: None,
        }],
        videos: vec![],
    });
    let mut acts = Vec::new();
    let input = resolve_picker_ipc_input(PickerCommand::Open, ui.is_visible(), ui.is_closing())
        .expect("open when closed");
    assert_eq!(ui.handle_input(input), vec![PickerActionOut::RequestCatalog]);
    for _ in 0..16 {
        ui.tick(0.016, &mut acts);
    }
    assert!(acts.is_empty());
    assert!((ui.panel_progress() - 1.0).abs() < 1e-4);

    // Mod+N Toggle starts animated close (no immediate Exit)
    let input = resolve_picker_ipc_input(PickerCommand::Toggle, ui.is_visible(), ui.is_closing())
        .expect("toggle to close");
    assert!(ui.handle_input(input).is_empty());
    assert!(ui.is_closing());
    for _ in 0..3 {
        ui.tick(0.016, &mut acts);
    }
    assert!(acts.is_empty());
    let mid = ui.panel_progress();
    assert!(mid > 0.1 && mid < 0.9);

    // Mod+N Toggle again mid-close interrupts and reopens smoothly!
    let input = resolve_picker_ipc_input(PickerCommand::Toggle, ui.is_visible(), ui.is_closing())
        .expect("toggle to reopen");
    assert_eq!(input, PickerInput::StartOpen);
    let _ = ui.handle_input(input);
    assert!(!ui.is_closing());
    assert!((ui.panel_progress() - mid).abs() < 1e-4);
    for _ in 0..16 {
        ui.tick(0.016, &mut acts);
    }
    assert!(acts.is_empty());
    assert!((ui.panel_progress() - 1.0).abs() < 1e-4);

    // IPC Close animates down and emits Exit only upon completion
    let input = resolve_picker_ipc_input(PickerCommand::Close, ui.is_visible(), ui.is_closing())
        .expect("close");
    assert!(ui.handle_input(input).is_empty());
    assert!(ui.is_closing());
    for _ in 0..15 {
        ui.tick(0.016, &mut acts);
    }
    assert_eq!(acts, vec![PickerActionOut::Exit]);
    assert!(!ui.is_visible());
}
