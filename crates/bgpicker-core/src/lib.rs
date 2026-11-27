//! Core logic, state reduction, geometry math, configuration, media catalog,
//! XDG paths, and daemon IPC protocol for bgpicker.

pub mod catalog;
pub mod config;
pub mod math;
pub mod niri;
pub mod paths;
pub mod protocol;

pub use catalog::{
    build_bundle, build_image_catalog, build_video_catalog, image_thumbnail_cache_key,
    prepare_image_thumbnail, prepare_video_thumbnail, video_thumbnail_cache_key, CatalogBundle,
    CatalogEntry, CatalogError, IMAGE_EXTENSIONS, VIDEO_EXTENSIONS,
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
    atomic_write, atomic_write_text, BackdropParallax, BgPickerPaths, EngineKind, PathsError,
    PersistedState, VideoMode, WallpaperMode,
};
pub use protocol::{
    read_picker_command, send_picker_command, write_picker_command, BackdropAction,
    ClearCacheReport, DaemonStatus, EngineAction, IpcClient, ModeAction, OutputStatus,
    PickerCommand, PickerIpcClient, ProtocolError, Request, Response, ResponseData,
    VideoModeAction,
};
