//! CLI argument parsing, single-instance Unix socket IPC server (`0600`),
//! and `calloop` event-source integration for `bgpicker-picker`.

use std::fs::{self, Permissions};
use std::io::{BufReader, BufWriter};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use bgpicker_core::protocol::write_response;
use bgpicker_core::{
    BgPickerPaths, PickerCommand, PickerIpcClient, ProtocolError, Response, ResponseData,
    read_picker_command,
};
use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use thiserror::Error;

const PING_TIMEOUT: Duration = Duration::from_millis(250);
const FORWARD_TIMEOUT: Duration = Duration::from_millis(500);
const CONN_IO_TIMEOUT: Duration = Duration::from_millis(300);

/// Initial action passed to `bgpicker-picker --action <...>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PickerStartAction {
    #[default]
    Open,
    Toggle,
    CycleNext,
    CyclePrevious,
    Close,
}

impl PickerStartAction {
    /// Parses a CLI `--action` value.
    pub fn from_cli_str(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "open" => Ok(Self::Open),
            "toggle" => Ok(Self::Toggle),
            "cycle-next" => Ok(Self::CycleNext),
            "cycle-prev" => Ok(Self::CyclePrevious),
            "close" => Ok(Self::Close),
            other => Err(format!(
                "invalid --action value '{other}' (expected open|toggle|cycle-next|cycle-prev|close)"
            )),
        }
    }

    /// Canonical CLI string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Toggle => "toggle",
            Self::CycleNext => "cycle-next",
            Self::CyclePrevious => "cycle-prev",
            Self::Close => "close",
        }
    }

    /// Converts the CLI startup action into the corresponding IPC [`PickerCommand`].
    pub const fn to_command(self) -> PickerCommand {
        match self {
            Self::Open => PickerCommand::Open,
            Self::Toggle => PickerCommand::Toggle,
            Self::CycleNext => PickerCommand::CycleNext,
            Self::CyclePrevious => PickerCommand::CyclePrevious,
            Self::Close => PickerCommand::Close,
        }
    }
}

/// Parsed CLI options for `bgpicker-picker`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerCliOptions {
    pub project_dir: PathBuf,
    pub daemon_socket: PathBuf,
    pub picker_socket: PathBuf,
    pub action: PickerStartAction,
}

/// Errors returned while parsing `bgpicker-picker` CLI arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliParseError {
    Help(String),
    Invalid(String),
}

/// Usage string for `bgpicker-picker`.
pub fn usage_text() -> String {
    "\
Usage: bgpicker-picker [OPTIONS]

Options:
  --project-dir <DIR>       bgpicker repository/media root directory
  --daemon-socket <PATH>    bgpicker-daemon Unix socket path
  --picker-socket <PATH>    bgpicker-picker single-instance Unix socket path
  --action <ACTION>         open | toggle | cycle-next | cycle-prev | close (default: open)
  -h, --help                Show this help text"
        .to_string()
}

/// Parses `bgpicker-picker` CLI flags (excluding argv[0]), applying defaults from `default_paths`.
pub fn parse_cli_args<I, S>(
    args: I,
    default_paths: &BgPickerPaths,
) -> Result<PickerCliOptions, CliParseError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let raw: Vec<String> = args.into_iter().map(Into::into).collect();
    let mut project_dir: Option<PathBuf> = None;
    let mut daemon_socket: Option<PathBuf> = None;
    let mut picker_socket: Option<PathBuf> = None;
    let mut action = PickerStartAction::Open;

    let mut i = 0usize;
    while i < raw.len() {
        let arg = raw[i].as_str();
        if let Some(val) = arg.strip_prefix("--project-dir=") {
            if val.is_empty() {
                return Err(CliParseError::Invalid("--project-dir requires a path".into()));
            }
            project_dir = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--daemon-socket=") {
            if val.is_empty() {
                return Err(CliParseError::Invalid("--daemon-socket requires a path".into()));
            }
            daemon_socket = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--picker-socket=") {
            if val.is_empty() {
                return Err(CliParseError::Invalid("--picker-socket requires a path".into()));
            }
            picker_socket = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--action=") {
            action = PickerStartAction::from_cli_str(val).map_err(CliParseError::Invalid)?;
        } else {
            match arg {
                "--project-dir" => {
                    i += 1;
                    let val = raw
                        .get(i)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| CliParseError::Invalid("--project-dir requires a path".into()))?;
                    project_dir = Some(PathBuf::from(val));
                }
                "--daemon-socket" => {
                    i += 1;
                    let val = raw
                        .get(i)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| CliParseError::Invalid("--daemon-socket requires a path".into()))?;
                    daemon_socket = Some(PathBuf::from(val));
                }
                "--picker-socket" => {
                    i += 1;
                    let val = raw
                        .get(i)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| CliParseError::Invalid("--picker-socket requires a path".into()))?;
                    picker_socket = Some(PathBuf::from(val));
                }
                "--action" => {
                    i += 1;
                    let val = raw.get(i).ok_or_else(|| {
                        CliParseError::Invalid(
                            "--action requires open|toggle|cycle-next|cycle-prev|close".into(),
                        )
                    })?;
                    action = PickerStartAction::from_cli_str(val).map_err(CliParseError::Invalid)?;
                }
                "-h" | "--help" => {
                    return Err(CliParseError::Help(usage_text()));
                }
                other => {
                    return Err(CliParseError::Invalid(format!(
                        "unknown argument '{other}'\n\n{}",
                        usage_text()
                    )));
                }
            }
        }
        i += 1;
    }

    Ok(PickerCliOptions {
        project_dir: resolve_project_dir(project_dir),
        daemon_socket: daemon_socket.unwrap_or_else(|| default_paths.daemon_socket_path()),
        picker_socket: picker_socket.unwrap_or_else(|| default_paths.picker_socket_path()),
        action,
    })
}

/// Resolves the bgpicker project directory from explicit override, `BGPICKER_DIR`,
/// current directory, or ancestors of the running executable.
pub fn resolve_project_dir(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir;
    }
    if let Some(env_dir) = std::env::var_os("BGPICKER_DIR").filter(|s| !s.is_empty()) {
        return PathBuf::from(env_dir);
    }
    if let Ok(cwd) = std::env::current_dir()
        && looks_like_project_dir(&cwd)
    {
        return cwd;
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent();
        while let Some(dir) = cur {
            if looks_like_project_dir(dir) {
                return dir.to_path_buf();
            }
            cur = dir.parent();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn looks_like_project_dir(dir: &Path) -> bool {
    dir.join("shell.qml").is_file()
        || (dir.join("wallpapers").is_dir() && dir.join("dynamic").is_dir())
}

/// Errors returned while acquiring, binding, or running the single-instance picker IPC socket.
#[derive(Debug, Error)]
pub enum PickerIpcError {
    #[error("IPC I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("picker IPC protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("picker instance already active at {0}")]
    AlreadyRunning(PathBuf),
    #[error("failed to register picker IPC source in calloop: {0}")]
    Calloop(String),
}

/// RAII guard that removes the bound Unix domain socket file on [`Drop`].
#[derive(Debug)]
pub struct SocketGuard {
    path: PathBuf,
    armed: bool,
}

impl SocketGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Explicitly unlinks the socket now and disarms future removal on drop.
    pub fn cleanup_now(&mut self) {
        if self.armed {
            self.armed = false;
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        self.cleanup_now();
    }
}

/// Result of attempting to acquire the single-instance picker socket.
pub enum SingleInstanceOutcome {
    /// Another `bgpicker-picker` responded to `Ping` and received our forwarded command.
    Forwarded(Response),
    /// `--action close` was requested when no picker instance was running.
    AlreadyClosed,
    /// We won the single-instance lock and bound the picker IPC socket.
    Acquired(PickerIpcListener),
}

/// Bound, non-blocking Unix listener paired with its RAII [`SocketGuard`].
pub struct PickerIpcListener {
    listener: UnixListener,
    guard: SocketGuard,
}

impl PickerIpcListener {
    /// Performs the single-instance check against `socket_path`:
    ///
    /// - If a live instance responds to [`PickerCommand::Ping`], forwards `action_cmd`
    ///   and returns [`SingleInstanceOutcome::Forwarded`].
    /// - Otherwise, if `action_cmd` is [`PickerCommand::Close`], returns
    ///   [`SingleInstanceOutcome::AlreadyClosed`].
    /// - Otherwise, removes any stale socket file, binds a fresh `0600` Unix listener,
    ///   and returns [`SingleInstanceOutcome::Acquired`].
    pub fn acquire_or_forward(
        socket_path: PathBuf,
        action_cmd: PickerCommand,
    ) -> Result<SingleInstanceOutcome, PickerIpcError> {
        if is_picker_live(&socket_path) {
            let client = PickerIpcClient::new(&socket_path).with_timeout(FORWARD_TIMEOUT);
            let resp = client.call(&action_cmd)?;
            return Ok(SingleInstanceOutcome::Forwarded(resp));
        }

        if action_cmd == PickerCommand::Close {
            cleanup_stale_file_if_present(&socket_path);
            return Ok(SingleInstanceOutcome::AlreadyClosed);
        }

        match Self::bind_fresh(socket_path.clone()) {
            Ok(listener) => Ok(SingleInstanceOutcome::Acquired(listener)),
            Err(PickerIpcError::AlreadyRunning(_)) => {
                // Another instance won the race between Ping and bind; forward to it.
                let client = PickerIpcClient::new(&socket_path).with_timeout(FORWARD_TIMEOUT);
                let resp = client.call(&action_cmd)?;
                Ok(SingleInstanceOutcome::Forwarded(resp))
            }
            Err(err) => Err(err),
        }
    }

    /// Binds a fresh non-blocking `0600` Unix socket at `socket_path`, removing any stale
    /// socket file if no live instance answers [`PickerCommand::Ping`].
    pub fn bind_fresh(socket_path: PathBuf) -> Result<Self, PickerIpcError> {
        if let Some(parent) = socket_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        if socket_path.symlink_metadata().is_ok() {
            if is_picker_live(&socket_path) {
                return Err(PickerIpcError::AlreadyRunning(socket_path));
            }
            let _ = fs::remove_file(&socket_path);
        }

        let listener = match UnixListener::bind(&socket_path) {
            Ok(l) => l,
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
                if is_picker_live(&socket_path) {
                    return Err(PickerIpcError::AlreadyRunning(socket_path));
                }
                let _ = fs::remove_file(&socket_path);
                UnixListener::bind(&socket_path)?
            }
            Err(err) => return Err(PickerIpcError::Io(err)),
        };

        let guard = SocketGuard::new(socket_path.clone());
        fs::set_permissions(&socket_path, Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;

        Ok(Self { listener, guard })
    }

    pub fn socket_path(&self) -> &Path {
        self.guard.path()
    }

    /// Registers the Unix listener directly into a `calloop` event loop with zero extra threads.
    ///
    /// Returns the `calloop` [`RegistrationToken`] and the [`SocketGuard`] whose `Drop` unlinks
    /// the socket file cleanly on exit.
    pub fn register_in_calloop<D, F>(
        self,
        handle: &LoopHandle<'_, D>,
        mut on_command: F,
    ) -> Result<(RegistrationToken, SocketGuard), PickerIpcError>
    where
        F: FnMut(&mut D, PickerCommand) -> Response + 'static,
    {
        let Self { listener, guard } = self;
        let source = Generic::new(listener, Interest::READ, Mode::Level);
        let token = handle
            .insert_source(source, move |_, wrapped, state| {
                accept_pending_connections(wrapped, |cmd| on_command(state, cmd));
                Ok(PostAction::Continue)
            })
            .map_err(|e| PickerIpcError::Calloop(e.to_string()))?;

        Ok((token, guard))
    }

    /// Drains and handles any currently pending connections (useful for synchronous unit tests).
    pub fn accept_pending<F>(&self, on_command: F)
    where
        F: FnMut(PickerCommand) -> Response,
    {
        accept_pending_connections(&self.listener, on_command);
    }
}

/// Default protocol response for [`PickerCommand`]: `Pong` for `Ping`, empty `ok` for all others.
pub fn default_picker_response(cmd: PickerCommand) -> Response {
    match cmd {
        PickerCommand::Ping => Response::ok(ResponseData::Pong),
        PickerCommand::Toggle
        | PickerCommand::Open
        | PickerCommand::Close
        | PickerCommand::CycleNext
        | PickerCommand::CyclePrevious => Response::ok_empty(),
    }
}

/// Probes `socket_path` with [`PickerCommand::Ping`] using a short timeout.
pub fn is_picker_live(socket_path: &Path) -> bool {
    let client = PickerIpcClient::new(socket_path).with_timeout(PING_TIMEOUT);
    matches!(
        client.call(&PickerCommand::Ping),
        Ok(resp) if resp.ok && matches!(resp.data, Some(ResponseData::Pong))
    )
}

fn cleanup_stale_file_if_present(socket_path: &Path) {
    if socket_path.symlink_metadata().is_ok() && !is_picker_live(socket_path) {
        let _ = fs::remove_file(socket_path);
    }
}

fn accept_pending_connections<F>(listener: &UnixListener, mut on_command: F)
where
    F: FnMut(PickerCommand) -> Response,
{
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                handle_client_stream(stream, &mut on_command);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                log::warn!("bgpicker-picker: IPC accept error: {err}");
                break;
            }
        }
    }
}

fn handle_client_stream<F>(stream: UnixStream, on_command: &mut F)
where
    F: FnMut(PickerCommand) -> Response,
{
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(CONN_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONN_IO_TIMEOUT));

    let Ok(write_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut writer = BufWriter::new(write_stream);

    match read_picker_command(&mut reader) {
        Ok(Some(cmd)) => {
            let resp = on_command(cmd);
            let _ = write_response(&mut writer, &resp);
        }
        Ok(None) => {}
        Err(err) => {
            let _ = write_response(&mut writer, &Response::err(err.to_string()));
        }
    }
}
