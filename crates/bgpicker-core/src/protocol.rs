//! Newline-delimited JSON IPC protocol between `bgpickerctl` / UI and `bgpicker-daemon`.

use crate::catalog::CatalogBundle;
use crate::config::{ParallaxSettings, ResolvedTransition, TransitionSettings, TransitionType};
use crate::paths::{EngineKind, VideoMode, WallpaperMode};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Errors returned by daemon IPC serialization or Unix socket transport.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("IPC I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("IPC JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("daemon closed IPC connection without a response")]
    UnexpectedEof,
}

/// Requested static mode change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModeAction {
    Awww,
    Parallax,
    Toggle,
}

/// Requested video mode change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoModeAction {
    Normal,
    Parallax,
    Toggle,
}

/// Requested engine switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineAction {
    Full,
    Simple,
    Video,
    Toggle,
}

/// Requested overview backdrop parallax action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackdropAction {
    On,
    Off,
    Toggle,
}

/// Fine-grained override for how a foreground command affects overview backdrop parallax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackdropOverride {
    /// Follow the default foreground-sync rule for this command.
    #[default]
    Auto,
    /// Explicitly enable overview backdrop parallax.
    On,
    /// Explicitly disable overview backdrop parallax.
    Off,
    /// Preserve the current backdrop parallax setting unchanged.
    Keep,
}

impl BackdropOverride {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
            Self::Keep => "keep",
        }
    }

    pub const fn is_auto(&self) -> bool {
        matches!(self, Self::Auto)
    }
}

impl std::str::FromStr for BackdropOverride {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "auto" => Ok(Self::Auto),
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            "keep" => Ok(Self::Keep),
            other => Err(format!(
                "invalid backdrop override '{other}' (expected on|off|keep|auto)"
            )),
        }
    }
}

/// Client-to-daemon JSON-line command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Status,
    Startup,
    StartupSeed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<PathBuf>,
    },
    StartupSettle,
    Restore,
    Stop,
    ReloadSettings,
    ApplyStatic {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<WallpaperMode>,
        #[serde(default)]
        instant: bool,
        #[serde(default, skip_serializing_if = "BackdropOverride::is_auto")]
        backdrop: BackdropOverride,
    },
    SetMode {
        mode: ModeAction,
        #[serde(default, skip_serializing_if = "BackdropOverride::is_auto")]
        backdrop: BackdropOverride,
    },
    BackdropParallax {
        action: BackdropAction,
    },
    StartupPlaceholder {
        action: BackdropAction,
    },
    PlaceholderCache {
        action: BackdropAction,
    },
    ApplyVideo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<VideoModeAction>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "BackdropOverride::is_auto")]
        backdrop: BackdropOverride,
    },
    SetEngine {
        engine: EngineAction,
    },
    SetTransition {
        #[serde(rename = "type")]
        transition_type: TransitionType,
    },
    ResolveTransition {
        #[serde(default)]
        instant: bool,
    },
    Catalog {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_dir: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        video_dir: Option<PathBuf>,
    },
    ClearCache,
}

/// Runtime memory report returned by [`Request::ClearCache`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearCacheReport {
    pub rss_before_kib: u64,
    pub rss_after_kib: u64,
    pub pss_before_kib: u64,
    pub pss_after_kib: u64,
    pub video_backend_stopped: bool,
}

/// Per-output live state reported in [`DaemonStatus`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputStatus {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub progress: f64,
    pub mode_blend: f64,
    pub transition_active: bool,
}

fn default_true() -> bool {
    true
}

/// Complete daemon status snapshot matching the IPC contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonStatus {
    pub engine: EngineKind,
    pub mode: WallpaperMode,
    pub current_wallpaper: Option<PathBuf>,
    pub video_mode: VideoMode,
    pub current_video: Option<PathBuf>,
    pub backdrop_parallax: bool,
    #[serde(default = "default_true")]
    pub startup_placeholder: bool,
    #[serde(default = "default_true")]
    pub placeholder_cache: bool,
    pub transition: TransitionSettings,
    pub parallax: ParallaxSettings,
    pub busy: bool,
    pub niri_connected: bool,
    pub overview_open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_output: Option<String>,
    pub outputs: Vec<OutputStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Client-to-picker JSON-line control command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum PickerCommand {
    Ping,
    Toggle,
    Open,
    Close,
    CycleNext,
    CyclePrevious,
}

/// Typed payload inside [`Response::data`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ResponseData {
    Pong,
    Status(Box<DaemonStatus>),
    TransitionSettings(TransitionSettings),
    ResolvedTransition(ResolvedTransition),
    Catalog(CatalogBundle),
    CacheCleared(ClearCacheReport),
    Message(String),
}

/// Daemon-to-client JSON-line response (`{"ok":bool,"data":...,"error":...}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ResponseData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    /// Constructs a success response without payload data.
    pub fn ok_empty() -> Self {
        Self {
            ok: true,
            data: None,
            error: None,
        }
    }

    /// Constructs a success response carrying typed payload data.
    pub fn ok(data: ResponseData) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    /// Constructs an error response.
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

/// Writes one newline-delimited JSON request and flushes the stream.
pub fn write_request<W: Write>(writer: &mut W, req: &Request) -> Result<(), ProtocolError> {
    serde_json::to_writer(&mut *writer, req)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Reads one newline-delimited JSON request (`Ok(None)` on clean EOF).
pub fn read_request<R: BufRead>(reader: &mut R) -> Result<Option<Request>, ProtocolError> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(line.trim())?))
}

/// Writes one newline-delimited JSON response and flushes the stream.
pub fn write_response<W: Write>(writer: &mut W, resp: &Response) -> Result<(), ProtocolError> {
    serde_json::to_writer(&mut *writer, resp)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Reads one newline-delimited JSON response.
pub fn read_response<R: BufRead>(reader: &mut R) -> Result<Response, ProtocolError> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Err(ProtocolError::UnexpectedEof);
    }
    Ok(serde_json::from_str(line.trim())?)
}

/// Writes one newline-delimited JSON picker command and flushes the stream.
pub fn write_picker_command<W: Write>(
    writer: &mut W,
    cmd: &PickerCommand,
) -> Result<(), ProtocolError> {
    serde_json::to_writer(&mut *writer, cmd)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Reads one newline-delimited JSON picker command (`Ok(None)` on clean EOF).
pub fn read_picker_command<R: BufRead>(
    reader: &mut R,
) -> Result<Option<PickerCommand>, ProtocolError> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(line.trim())?))
}

/// Sends one [`PickerCommand`] to `socket_path` and awaits one [`Response`].
pub fn send_picker_command(
    socket_path: &Path,
    cmd: &PickerCommand,
    timeout: Duration,
) -> Result<Response, ProtocolError> {
    let mut stream = UnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write_picker_command(&mut stream, cmd)?;
    let mut reader = BufReader::new(stream);
    read_response(&mut reader)
}

/// Synchronous Unix-socket IPC client for `bgpicker-picker`.
#[derive(Debug, Clone)]
pub struct PickerIpcClient {
    socket_path: PathBuf,
    timeout: Duration,
}

impl PickerIpcClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout: Duration::from_millis(500),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Sends one command and awaits one response from `bgpicker-picker`.
    pub fn call(&self, cmd: &PickerCommand) -> Result<Response, ProtocolError> {
        send_picker_command(&self.socket_path, cmd, self.timeout)
    }
}

/// Synchronous Unix-socket IPC client for `bgpickerctl` and callers.
#[derive(Debug, Clone)]
pub struct IpcClient {
    socket_path: PathBuf,
    timeout: Duration,
}

impl IpcClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout: Duration::from_secs(10),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Sends one request and awaits one response from `bgpicker-daemon`.
    pub fn call(&self, request: &Request) -> Result<Response, ProtocolError> {
        let mut stream = UnixStream::connect(&self.socket_path)?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        write_request(&mut stream, request)?;
        let mut reader = BufReader::new(stream);
        read_response(&mut reader)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_backdrop_override_defaults_to_auto_and_omits_default_json() {
        let legacy_mode: Request =
            serde_json::from_str(r#"{"cmd":"set_mode","mode":"parallax"}"#).unwrap();
        assert_eq!(
            legacy_mode,
            Request::SetMode {
                mode: ModeAction::Parallax,
                backdrop: BackdropOverride::Auto,
            }
        );
        let serialized = serde_json::to_string(&legacy_mode).unwrap();
        assert!(!serialized.contains("backdrop"), "unexpected field: {serialized}");

        let legacy_static: Request =
            serde_json::from_str(r#"{"cmd":"apply_static","path":"/tmp/a.png"}"#).unwrap();
        assert_eq!(
            legacy_static,
            Request::ApplyStatic {
                path: PathBuf::from("/tmp/a.png"),
                mode: None,
                instant: false,
                backdrop: BackdropOverride::Auto,
            }
        );
        assert!(!serde_json::to_string(&legacy_static).unwrap().contains("backdrop"));

        let legacy_video: Request = serde_json::from_str(r#"{"cmd":"apply_video"}"#).unwrap();
        assert_eq!(
            legacy_video,
            Request::ApplyVideo {
                mode: None,
                path: None,
                backdrop: BackdropOverride::Auto,
            }
        );
        assert!(!serde_json::to_string(&legacy_video).unwrap().contains("backdrop"));
    }

    #[test]
    fn request_backdrop_override_roundtrips_explicit_variants() {
        for ov in [
            BackdropOverride::On,
            BackdropOverride::Off,
            BackdropOverride::Keep,
        ] {
            let req = Request::SetMode {
                mode: ModeAction::Toggle,
                backdrop: ov,
            };
            let json = serde_json::to_string(&req).unwrap();
            assert!(json.contains(ov.as_str()), "missing override in {json}");
            let parsed: Request = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, req);
        }
    }
}
