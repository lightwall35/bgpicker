//! XDG config/cache/state/runtime paths and atomic state persistence helpers.

use serde::{Deserialize, Serialize};
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Errors returned by path resolution or atomic state persistence.
#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid state value for {field}: {value}")]
    InvalidValue { field: &'static str, value: String },
}

/// Active static wallpaper presentation mode (`~/.local/state/bgpicker/mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WallpaperMode {
    #[default]
    Awww,
    Parallax,
}

impl WallpaperMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Awww => "awww",
            Self::Parallax => "parallax",
        }
    }

    pub const fn toggled(self) -> Self {
        match self {
            Self::Awww => Self::Parallax,
            Self::Parallax => Self::Awww,
        }
    }
}

impl FromStr for WallpaperMode {
    type Err = PathsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "awww" => Ok(Self::Awww),
            "parallax" => Ok(Self::Parallax),
            other => Err(PathsError::InvalidValue {
                field: "mode",
                value: other.to_string(),
            }),
        }
    }
}

/// Active video wallpaper mode (`~/.local/state/bgpicker/video-mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VideoMode {
    #[default]
    Normal,
    Parallax,
}

impl VideoMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Parallax => "parallax",
        }
    }

    pub const fn toggled(self) -> Self {
        match self {
            Self::Normal => Self::Parallax,
            Self::Parallax => Self::Normal,
        }
    }
}

impl FromStr for VideoMode {
    type Err = PathsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "normal" => Ok(Self::Normal),
            "parallax" => Ok(Self::Parallax),
            other => Err(PathsError::InvalidValue {
                field: "video-mode",
                value: other.to_string(),
            }),
        }
    }
}

/// Active wallpaper engine (`~/.local/state/bgpicker/engine`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    #[default]
    Full,
    Simple,
    Video,
}

impl EngineKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Simple => "simple",
            Self::Video => "video",
        }
    }
}

impl FromStr for EngineKind {
    type Err = PathsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "full" => Ok(Self::Full),
            "simple" => Ok(Self::Simple),
            "video" => Ok(Self::Video),
            other => Err(PathsError::InvalidValue {
                field: "engine",
                value: other.to_string(),
            }),
        }
    }
}

/// Overview backdrop parallax toggle (`~/.local/state/bgpicker/backdrop_parallax`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackdropParallax {
    On,
    #[default]
    Off,
}

impl BackdropParallax {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }

    pub const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }

    pub const fn toggled(self) -> Self {
        match self {
            Self::On => Self::Off,
            Self::Off => Self::On,
        }
    }
}

impl FromStr for BackdropParallax {
    type Err = PathsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            other => Err(PathsError::InvalidValue {
                field: "backdrop_parallax",
                value: other.to_string(),
            }),
        }
    }
}

/// All XDG directories and well-known files used by bgpicker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BgPickerPaths {
    pub home_dir: PathBuf,
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub wayland_display: String,
    pub socket_override: Option<PathBuf>,
    pub picker_socket_override: Option<PathBuf>,
}

impl BgPickerPaths {
    /// Resolves paths using standard XDG environment variables and `HOME`.
    pub fn from_env() -> Self {
        let home_dir = env_path("HOME").unwrap_or_else(|| PathBuf::from("/tmp"));
        let config_home = env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home_dir.join(".config"));
        let cache_home = env_path("XDG_CACHE_HOME").unwrap_or_else(|| home_dir.join(".cache"));
        let state_home =
            env_path("XDG_STATE_HOME").unwrap_or_else(|| home_dir.join(".local/state"));
        let runtime_dir = env_path("XDG_RUNTIME_DIR").unwrap_or_else(|| PathBuf::from("/tmp"));
        let wayland_display = env::var("WAYLAND_DISPLAY")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "wayland-0".to_string());
        let socket_override = env_path("BGPICKER_SOCKET");
        let picker_socket_override = env_path("BGPICKER_PICKER_SOCKET");

        Self {
            home_dir,
            config_dir: config_home.join("bgpicker"),
            cache_dir: cache_home.join("bgpicker"),
            state_dir: state_home.join("bgpicker"),
            runtime_dir,
            wayland_display,
            socket_override,
            picker_socket_override,
        }
    }

    /// Constructs isolated paths for tests without mutating process env vars.
    pub fn with_roots(
        home_dir: impl Into<PathBuf>,
        config_home: impl Into<PathBuf>,
        cache_home: impl Into<PathBuf>,
        state_home: impl Into<PathBuf>,
        runtime_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            home_dir: home_dir.into(),
            config_dir: config_home.into().join("bgpicker"),
            cache_dir: cache_home.into().join("bgpicker"),
            state_dir: state_home.into().join("bgpicker"),
            runtime_dir: runtime_dir.into(),
            wayland_display: "wayland-0".to_string(),
            socket_override: None,
            picker_socket_override: None,
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }

    pub fn switch_lock_file(&self) -> PathBuf {
        self.cache_dir.join("switch.lock")
    }

    pub fn engine_lock_file(&self) -> PathBuf {
        self.cache_dir.join("engine.lock")
    }

    pub fn video_thumbnails_dir(&self) -> PathBuf {
        self.cache_dir.join("video-thumbnails")
    }

    pub fn image_thumbnails_dir(&self) -> PathBuf {
        self.cache_dir.join("image-thumbnails")
    }

    pub fn blur_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("blur")
    }

    pub fn video_blur_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("video-blur")
    }

    pub fn current_wallpaper_file(&self) -> PathBuf {
        self.state_dir.join("current_wallpaper")
    }

    pub fn mode_file(&self) -> PathBuf {
        self.state_dir.join("mode")
    }

    pub fn engine_file(&self) -> PathBuf {
        self.state_dir.join("engine")
    }

    pub fn backdrop_parallax_file(&self) -> PathBuf {
        self.state_dir.join("backdrop_parallax")
    }

    pub fn video_wallpaper_file(&self) -> PathBuf {
        self.state_dir.join("video-wallpaper")
    }

    pub fn video_mode_file(&self) -> PathBuf {
        self.state_dir.join("video-mode")
    }

    pub fn startup_placeholder_front_pid_file(&self) -> PathBuf {
        self.state_dir.join("startup-placeholder-front.pid")
    }

    pub fn startup_placeholder_back_pid_file(&self) -> PathBuf {
        self.state_dir.join("startup-placeholder-back.pid")
    }

    /// Advisory `flock` lock file preventing duplicate concurrent `bgpicker-splash` instances.
    pub fn startup_placeholder_lock_file(&self) -> PathBuf {
        self.state_dir.join("startup-placeholder.lock")
    }

    pub fn startup_placeholder_file(&self) -> PathBuf {
        self.state_dir.join("startup_placeholder")
    }

    pub fn placeholder_cache_file(&self) -> PathBuf {
        self.state_dir.join("placeholder_cache")
    }

    pub fn startup_placeholder_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("startup-placeholder")
    }

    /// Pre-scaled zero-decode `BGPX` bitmap for the currently active foreground placeholder.
    pub fn startup_splash_fg_bin_file(&self) -> PathBuf {
        self.cache_dir.join("startup-splash-fg.bin")
    }

    /// Legacy compatibility blur pointer (`~/.cache/current_swaybg_img`).
    pub fn legacy_swaybg_cache_file(&self) -> PathBuf {
        self.home_dir.join(".cache/current_swaybg_img")
    }

    fn sanitized_wayland_display(&self) -> String {
        self.wayland_display
            .chars()
            .map(|c| if c == '/' || c == '\0' { '_' } else { c })
            .collect()
    }

    /// Unix socket path for `bgpicker-daemon` (`BGPICKER_SOCKET` or
    /// `${XDG_RUNTIME_DIR:-/tmp}/bgpicker-${WAYLAND_DISPLAY:-wayland-0}.sock`).
    pub fn daemon_socket_path(&self) -> PathBuf {
        if let Some(ref custom) = self.socket_override {
            return custom.clone();
        }
        let sanitized_display = self.sanitized_wayland_display();
        self.runtime_dir
            .join(format!("bgpicker-{sanitized_display}.sock"))
    }

    /// Unix socket path for `bgpicker-picker` (`BGPICKER_PICKER_SOCKET` or
    /// `${XDG_RUNTIME_DIR:-/tmp}/bgpicker-picker-${WAYLAND_DISPLAY:-wayland-0}.sock`).
    pub fn picker_socket_path(&self) -> PathBuf {
        if let Some(ref custom) = self.picker_socket_override {
            return custom.clone();
        }
        let sanitized_display = self.sanitized_wayland_display();
        self.runtime_dir
            .join(format!("bgpicker-picker-{sanitized_display}.sock"))
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    let val: OsString = env::var_os(key)?;
    if val.is_empty() {
        None
    } else {
        Some(PathBuf::from(val))
    }
}

/// Resolves the bgpicker repository/project directory from CLI override, `BGPICKER_DIR`,
/// current directory, or ancestors of the running executable.
pub fn resolve_project_dir(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir;
    }
    if let Some(env_dir) = env::var_os("BGPICKER_DIR").filter(|s| !s.is_empty()) {
        return PathBuf::from(env_dir);
    }
    if let Ok(cwd) = env::current_dir() {
        if looks_like_project_dir(&cwd) {
            return cwd;
        }
    }
    if let Ok(exe) = env::current_exe() {
        let mut cur = exe.parent();
        while let Some(dir) = cur {
            if looks_like_project_dir(dir) {
                return dir.to_path_buf();
            }
            cur = dir.parent();
        }
    }
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn looks_like_project_dir(dir: &Path) -> bool {
    dir.join("shell.qml").is_file()
        || (dir.join("wallpapers").is_dir() && dir.join("dynamic").is_dir())
}

fn default_true() -> bool {
    true
}

/// Snapshot of persisted `~/.local/state/bgpicker/*` files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedState {
    pub engine: EngineKind,
    pub mode: WallpaperMode,
    pub backdrop_parallax: BackdropParallax,
    pub current_wallpaper: Option<PathBuf>,
    pub video_wallpaper: Option<PathBuf>,
    pub video_mode: VideoMode,
    #[serde(default = "default_true")]
    pub startup_placeholder: bool,
    #[serde(default = "default_true")]
    pub placeholder_cache: bool,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            engine: EngineKind::default(),
            mode: WallpaperMode::default(),
            backdrop_parallax: BackdropParallax::default(),
            current_wallpaper: None,
            video_wallpaper: None,
            video_mode: VideoMode::default(),
            startup_placeholder: true,
            placeholder_cache: true,
        }
    }
}

impl PersistedState {
    /// Loads persisted state from `paths`, falling back to canonical defaults
    /// for missing or unrecognized enum files.
    pub fn load(paths: &BgPickerPaths) -> Self {
        let engine = read_trimmed(&paths.engine_file())
            .and_then(|s| EngineKind::from_str(&s).ok())
            .unwrap_or_default();
        let mode = read_trimmed(&paths.mode_file())
            .and_then(|s| WallpaperMode::from_str(&s).ok())
            .unwrap_or_default();
        let backdrop_parallax = read_trimmed(&paths.backdrop_parallax_file())
            .and_then(|s| BackdropParallax::from_str(&s).ok())
            .unwrap_or_default();
        let current_wallpaper = read_trimmed(&paths.current_wallpaper_file())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let video_wallpaper = read_trimmed(&paths.video_wallpaper_file())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let video_mode = read_trimmed(&paths.video_mode_file())
            .and_then(|s| VideoMode::from_str(&s).ok())
            .unwrap_or_default();
        let startup_placeholder = read_on_off_default_on(&paths.startup_placeholder_file());
        let placeholder_cache = read_on_off_default_on(&paths.placeholder_cache_file());

        Self {
            engine,
            mode,
            backdrop_parallax,
            current_wallpaper,
            video_wallpaper,
            video_mode,
            startup_placeholder,
            placeholder_cache,
        }
    }
}

fn read_on_off_default_on(path: &Path) -> bool {
    !matches!(read_trimmed(path).as_deref(), Some("off"))
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Atomically writes `bytes` to `target` via sibling temp file + `sync_all` + `rename`.
pub fn atomic_write(target: &Path, bytes: &[u8]) -> Result<(), PathsError> {
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".to_string());
    let tmp_path = parent.join(format!(
        ".{file_name}.tmp.{}.{:?}",
        std::process::id(),
        std::thread::current().id()
    ));

    let res = (|| -> Result<(), PathsError> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp_path, target)?;
        Ok(())
    })();

    if res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    res
}

/// Atomically writes a text line (appending trailing `\n` if omitted).
pub fn atomic_write_text(target: &Path, text: &str) -> Result<(), PathsError> {
    if text.ends_with('\n') {
        atomic_write(target, text.as_bytes())
    } else {
        let mut buf = String::with_capacity(text.len() + 1);
        buf.push_str(text);
        buf.push('\n');
        atomic_write(target, buf.as_bytes())
    }
}
