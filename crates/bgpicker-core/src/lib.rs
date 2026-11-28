//! Core logic, state reduction, geometry math, configuration, media catalog,
//! XDG paths, and daemon IPC protocol for bgpicker.

pub mod catalog;
pub mod config;
pub mod math;
pub mod niri;
pub mod paths;
pub mod protocol;
pub mod splash;

pub use catalog::{
    build_bundle, build_image_catalog, build_video_catalog, cached_startup_placeholder_path,
    cached_video_thumbnail_path, image_thumbnail_cache_key, list_videos_sorted,
    prepare_image_thumbnail, prepare_startup_placeholder, prepare_video_thumbnail,
    startup_placeholder_cache_key, video_thumbnail_cache_key, CatalogBundle, CatalogEntry,
    CatalogError, IMAGE_EXTENSIONS, VIDEO_EXTENSIONS,
};
pub use config::{
    normalize_parallax, normalize_transition, read_settings_json, resolve_transition,
    resolve_transition_with_rng, set_transition_type, BgPickerConfig, ConfigError, ModeSwitch,
    ParallaxSettings, ResolvedTransition, TransitionChoice, TransitionSettings, TransitionType,
    TRANSITION_CHOICES,
};
pub use math::{clamp, file_url, geometry, mode_geometry, Geometry, ModeGeometry};
pub use niri::{
    niri_socket_path, NiriClientConfig, NiriError, NiriEvent, NiriState, NiriWatcher, Window,
    WindowLayout, Workspace,
};
pub use paths::{
    atomic_write, atomic_write_text, resolve_project_dir, BackdropParallax, BgPickerPaths,
    EngineKind, PathsError, PersistedState, VideoMode, WallpaperMode,
};
pub use protocol::{
    read_picker_command, send_picker_command, write_picker_command, BackdropAction,
    ClearCacheReport, DaemonStatus, EngineAction, IpcClient, ModeAction, OutputStatus,
    PickerCommand, PickerIpcClient, ProtocolError, Request, Response, ResponseData,
    VideoModeAction,
};
pub use splash::{
    compute_scaled_size_120, cover_fill_to_xrgb8888, is_placeholder_pid_alive,
    is_splash_lock_held, process_matches_placeholder, read_proc_start_time, read_splash_header,
    record_placeholder_pid, resolve_startup_placeholders, retire_startup_placeholders,
    stop_placeholder_pid_file, try_acquire_splash_lock, try_load_bgpx_exact_into_memfd,
    write_pixels_to_memfd, write_splash_bin_from_packed, MemfdBuffer, SplashInstanceLock,
    SPLASH_BIN_MAGIC, SPLASH_HEADER_BYTES,
};
