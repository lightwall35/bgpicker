//! Niri IPC `EventStream` client, automatic reconnection, and state reducer.
//!
//! Matches `NiriState.qml` and `WallpaperMath.js` event reduction and workspace
//! column progress calculation.

use crate::math::clamp;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Errors returned by the niri IPC socket client.
#[derive(Debug, thiserror::Error)]
pub enum NiriError {
    #[error("NIRI_SOCKET environment variable is not set or empty")]
    MissingSocketEnv,
    #[error("niri IPC I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON from niri IPC: {0}")]
    Json(#[from] serde_json::Error),
    #[error("niri rejected EventStream request: {0}")]
    HandshakeRejected(String),
}

/// Returns `$NIRI_SOCKET` if set and non-empty.
pub fn niri_socket_path() -> Option<PathBuf> {
    std::env::var_os("NIRI_SOCKET")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Niri workspace snapshot entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: u64,
    #[serde(default)]
    pub idx: u8,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub is_urgent: bool,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_focused: bool,
    #[serde(default)]
    pub active_window_id: Option<u64>,
}

/// Window layout subset needed for scrolling column lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WindowLayout {
    #[serde(default)]
    pub pos_in_scrolling_layout: Option<Vec<i64>>,
}

/// Niri window snapshot entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub id: u64,
    #[serde(default)]
    pub workspace_id: Option<u64>,
    #[serde(default)]
    pub is_focused: bool,
    #[serde(default)]
    pub is_floating: bool,
    #[serde(default)]
    pub layout: Option<WindowLayout>,
}

/// Events emitted by niri's `EventStream`.
///
/// Any unknown or future event type deserializes cleanly into [`NiriEvent::Unknown`]
/// so state reduction remains forward-compatible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NiriEvent {
    OverviewOpenedOrClosed { is_open: bool },
    WorkspacesChanged { workspaces: Vec<Workspace> },
    WorkspaceActivated { id: u64, focused: bool },
    WorkspaceActiveWindowChanged {
        workspace_id: u64,
        active_window_id: Option<u64>,
    },
    WindowsChanged { windows: Vec<Window> },
    WindowOpenedOrChanged { window: Window },
    WindowClosed { id: u64 },
    WindowFocusChanged { id: Option<u64> },
    WindowLayoutsChanged { changes: Vec<(u64, WindowLayout)> },
    Unknown,
}

impl<'de> Deserialize<'de> for NiriEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let Some(obj) = value.as_object() else {
            return Err(D::Error::custom("niri event must be a JSON object"));
        };
        if obj.len() != 1 {
            return Ok(Self::Unknown);
        }
        let (key, payload) = obj.iter().next().expect("checked len == 1");
        match key.as_str() {
            "OverviewOpenedOrClosed" => {
                #[derive(Deserialize)]
                struct Payload {
                    is_open: bool,
                }
                let p: Payload = serde_json::from_value(payload.clone()).map_err(D::Error::custom)?;
                Ok(Self::OverviewOpenedOrClosed { is_open: p.is_open })
            }
            "WorkspacesChanged" => {
                #[derive(Deserialize)]
                struct Payload {
                    workspaces: Vec<Workspace>,
                }
                let p: Payload = serde_json::from_value(payload.clone()).map_err(D::Error::custom)?;
                Ok(Self::WorkspacesChanged {
                    workspaces: p.workspaces,
                })
            }
            "WorkspaceActivated" => {
                #[derive(Deserialize)]
                struct Payload {
                    id: u64,
                    #[serde(default)]
                    focused: bool,
                }
                let p: Payload = serde_json::from_value(payload.clone()).map_err(D::Error::custom)?;
                Ok(Self::WorkspaceActivated {
                    id: p.id,
                    focused: p.focused,
                })
            }
            "WorkspaceActiveWindowChanged" => {
                #[derive(Deserialize)]
                struct Payload {
                    workspace_id: u64,
                    #[serde(default)]
                    active_window_id: Option<u64>,
                }
                let p: Payload = serde_json::from_value(payload.clone()).map_err(D::Error::custom)?;
                Ok(Self::WorkspaceActiveWindowChanged {
                    workspace_id: p.workspace_id,
                    active_window_id: p.active_window_id,
                })
            }
            "WindowsChanged" => {
                #[derive(Deserialize)]
                struct Payload {
                    windows: Vec<Window>,
                }
                let p: Payload = serde_json::from_value(payload.clone()).map_err(D::Error::custom)?;
                Ok(Self::WindowsChanged { windows: p.windows })
            }
            "WindowOpenedOrChanged" => {
                #[derive(Deserialize)]
                struct Payload {
                    window: Window,
                }
                let p: Payload = serde_json::from_value(payload.clone()).map_err(D::Error::custom)?;
                Ok(Self::WindowOpenedOrChanged { window: p.window })
            }
            "WindowClosed" => {
                #[derive(Deserialize)]
                struct Payload {
                    id: u64,
                }
                let p: Payload = serde_json::from_value(payload.clone()).map_err(D::Error::custom)?;
                Ok(Self::WindowClosed { id: p.id })
            }
            "WindowFocusChanged" => {
                #[derive(Deserialize)]
                struct Payload {
                    #[serde(default)]
                    id: Option<u64>,
                }
                let p: Payload = serde_json::from_value(payload.clone()).map_err(D::Error::custom)?;
                Ok(Self::WindowFocusChanged { id: p.id })
            }
            "WindowLayoutsChanged" => {
                #[derive(Deserialize)]
                struct Payload {
                    changes: Vec<(u64, WindowLayout)>,
                }
                let p: Payload = serde_json::from_value(payload.clone()).map_err(D::Error::custom)?;
                Ok(Self::WindowLayoutsChanged { changes: p.changes })
            }
            _ => Ok(Self::Unknown),
        }
    }
}

/// Reduced niri compositor state used for parallax and overview backdrop logic.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NiriState {
    pub windows: Vec<Window>,
    pub workspaces: Vec<Workspace>,
    pub last_columns: HashMap<u64, i64>,
    pub overview_open: bool,
}

impl NiriState {
    /// Returns a fresh empty state matching `WallpaperMath.initialState()`.
    pub fn initial() -> Self {
        Self::default()
    }

    /// Returns the scrolling column index of a tiled window, or `None` if floating/unplaced.
    pub fn window_column(window: &Window) -> Option<i64> {
        if window.is_floating {
            return None;
        }
        window
            .layout
            .as_ref()
            .and_then(|l| l.pos_in_scrolling_layout.as_ref())
            .and_then(|pos| pos.first().copied())
    }

    /// Applies one niri IPC event, returning a new reduced state.
    pub fn reduce(&self, event: NiriEvent) -> Self {
        let mut windows = self.windows.clone();
        let mut workspaces = self.workspaces.clone();
        let overview_open = self.overview_open;

        match event {
            NiriEvent::OverviewOpenedOrClosed { is_open } => {
                return Self {
                    windows,
                    workspaces,
                    last_columns: self.last_columns.clone(),
                    overview_open: is_open,
                };
            }
            NiriEvent::WorkspacesChanged {
                workspaces: next_workspaces,
            } => {
                workspaces = next_workspaces;
            }
            NiriEvent::WorkspaceActivated { id, focused } => {
                let selected_output = workspaces
                    .iter()
                    .find(|w| w.id == id)
                    .map(|w| w.output.clone());
                for w in &mut workspaces {
                    if let Some(ref sel_out) = selected_output {
                        if &w.output == sel_out {
                            w.is_active = w.id == id;
                        }
                    }
                    if focused {
                        w.is_focused = w.id == id;
                    }
                }
            }
            NiriEvent::WorkspaceActiveWindowChanged {
                workspace_id,
                active_window_id,
            } => {
                for w in &mut workspaces {
                    if w.id == workspace_id {
                        w.active_window_id = active_window_id;
                    }
                }
            }
            NiriEvent::WindowsChanged {
                windows: next_windows,
            } => {
                windows = next_windows;
            }
            NiriEvent::WindowOpenedOrChanged { window: changed } => {
                windows.retain(|w| w.id != changed.id);
                if changed.is_focused {
                    for w in &mut windows {
                        w.is_focused = false;
                    }
                }
                windows.push(changed);
            }
            NiriEvent::WindowClosed { id } => {
                windows.retain(|w| w.id != id);
            }
            NiriEvent::WindowFocusChanged { id } => {
                for w in &mut windows {
                    w.is_focused = Some(w.id) == id;
                }
            }
            NiriEvent::WindowLayoutsChanged { changes } => {
                for (win_id, layout) in changes {
                    if let Some(w) = windows.iter_mut().find(|w| w.id == win_id) {
                        w.layout = Some(layout);
                    }
                }
            }
            NiriEvent::Unknown => {
                return self.clone();
            }
        }

        let mut memory = HashMap::new();
        for ws in &workspaces {
            if let Some(&col) = self.last_columns.get(&ws.id) {
                memory.insert(ws.id, col);
            }
        }
        for w in &windows {
            if w.is_focused {
                if let (Some(col), Some(ws_id)) = (Self::window_column(w), w.workspace_id) {
                    if workspaces.iter().any(|ws| ws.id == ws_id) {
                        memory.insert(ws_id, col);
                    }
                }
            }
        }

        Self {
            windows,
            workspaces,
            last_columns: memory,
            overview_open,
        }
    }

    /// Parses one JSON line and applies [`NiriState::reduce`].
    pub fn reduce_json_line(&self, line: &str) -> Result<Self, serde_json::Error> {
        let event: NiriEvent = serde_json::from_str(line)?;
        Ok(self.reduce(event))
    }

    /// Computes normalized horizontal parallax progress `[0.0, 1.0]` for `output`.
    ///
    /// Matches `WallpaperMath.progress(state, output, columnSpan)`:
    /// - Deduplicates and sorts tiled column indices on the output's active workspace.
    /// - Uses the active window's column, falling back to `last_columns` when a
    ///   floating window is focused, and picking the nearest remaining column.
    /// - Ranks the chosen column over `span = column_span > 1 ? column_span : columns.len()`.
    pub fn progress(&self, output: &str, column_span: usize) -> f64 {
        let Some(workspace) = self
            .workspaces
            .iter()
            .find(|w| w.output.as_deref() == Some(output) && w.is_active)
        else {
            return 0.5;
        };

        let ws_windows: Vec<&Window> = self
            .windows
            .iter()
            .filter(|w| w.workspace_id == Some(workspace.id))
            .collect();

        let mut columns: Vec<i64> = Vec::new();
        for w in &ws_windows {
            if let Some(col) = Self::window_column(w) {
                if !columns.contains(&col) {
                    columns.push(col);
                }
            }
        }
        columns.sort_unstable();
        if columns.is_empty() {
            return 0.5;
        }

        let active = ws_windows
            .iter()
            .copied()
            .find(|w| Some(w.id) == workspace.active_window_id);
        let preferred = active
            .and_then(Self::window_column)
            .or_else(|| self.last_columns.get(&workspace.id).copied());

        let nearest = match preferred {
            Some(pref) => {
                let mut best = columns[0];
                let mut best_dist = (best - pref).abs();
                for &c in &columns[1..] {
                    let dist = (c - pref).abs();
                    if dist < best_dist {
                        best = c;
                        best_dist = dist;
                    }
                }
                best
            }
            None => columns[0],
        };

        let span = if column_span > 1 {
            column_span
        } else {
            columns.len()
        };
        if span <= 1 {
            return 0.5;
        }
        let rank = columns.iter().position(|&c| c == nearest).unwrap_or(0);
        clamp(rank as f64 / (span - 1) as f64, 0.0, 1.0)
    }
}

/// Configuration for [`NiriWatcher`].
#[derive(Debug, Clone)]
pub struct NiriClientConfig {
    pub socket_path: PathBuf,
    pub reconnect_interval: Duration,
    pub read_timeout: Option<Duration>,
}

impl NiriClientConfig {
    /// Builds a config from `$NIRI_SOCKET` with the default 1500ms reconnect interval.
    pub fn from_env() -> Result<Self, NiriError> {
        let socket_path = niri_socket_path().ok_or(NiriError::MissingSocketEnv)?;
        Ok(Self::new(socket_path))
    }

    /// Builds a config for an explicit socket path.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            reconnect_interval: Duration::from_millis(1500),
            read_timeout: Some(Duration::from_millis(500)),
        }
    }
}

/// Connects to `socket_path`, sends `"EventStream"`, verifies any initial
/// `Err` reply, and returns a buffered line reader on the stream.
pub fn open_event_stream(
    socket_path: &Path,
    read_timeout: Option<Duration>,
) -> Result<BufReader<UnixStream>, NiriError> {
    let mut stream = UnixStream::connect(socket_path)?;
    stream.set_read_timeout(read_timeout)?;
    stream.write_all(b"\"EventStream\"\n")?;
    stream.flush()?;
    Ok(BufReader::new(stream))
}

#[derive(Debug, Default)]
struct SharedWatcherState {
    connected: AtomicBool,
    stop: AtomicBool,
    snapshot: RwLock<NiriState>,
}

/// Background niri `EventStream` watcher with automatic reconnection and
/// clean state reset on every connection.
pub struct NiriWatcher {
    shared: Arc<SharedWatcherState>,
    thread: Option<JoinHandle<()>>,
}

impl NiriWatcher {
    /// Spawns a background thread tracking niri state from `config.socket_path`.
    ///
    /// `on_update` is invoked with `(connected, &NiriState)` whenever connection
    /// status or state changes.
    pub fn spawn<F>(config: NiriClientConfig, mut on_update: F) -> Self
    where
        F: FnMut(bool, &NiriState) + Send + 'static,
    {
        let shared = Arc::new(SharedWatcherState::default());
        let worker_shared = Arc::clone(&shared);

        let thread = thread::Builder::new()
            .name("bgpicker-niri".into())
            .spawn(move || {
                while !worker_shared.stop.load(Ordering::Acquire) {
                    match open_event_stream(&config.socket_path, config.read_timeout) {
                        Ok(mut reader) => {
                            let initial = NiriState::initial();
                            {
                                let mut guard = worker_shared
                                    .snapshot
                                    .write()
                                    .expect("niri state lock poisoned");
                                *guard = initial.clone();
                            }
                            worker_shared.connected.store(true, Ordering::Release);
                            on_update(true, &initial);

                            let mut line = String::new();
                            let mut checked_handshake = false;
                            while !worker_shared.stop.load(Ordering::Acquire) {
                                line.clear();
                                match reader.read_line(&mut line) {
                                    Ok(0) => break, // EOF -> reconnect
                                    Ok(_) => {
                                        let trimmed = line.trim();
                                        if trimmed.is_empty() {
                                            continue;
                                        }
                                        if !checked_handshake {
                                            checked_handshake = true;
                                            if let Ok(serde_json::Value::Object(map)) =
                                                serde_json::from_str::<serde_json::Value>(trimmed)
                                            {
                                                if map.contains_key("Err") {
                                                    break;
                                                }
                                            }
                                        }
                                        if let Ok(event) = serde_json::from_str::<NiriEvent>(trimmed)
                                        {
                                            if matches!(event, NiriEvent::Unknown) {
                                                continue;
                                            }
                                            let next = {
                                                let mut guard = worker_shared
                                                    .snapshot
                                                    .write()
                                                    .expect("niri state lock poisoned");
                                                let reduced = guard.reduce(event);
                                                *guard = reduced.clone();
                                                reduced
                                            };
                                            on_update(true, &next);
                                        }
                                    }
                                    Err(err)
                                        if err.kind() == std::io::ErrorKind::WouldBlock
                                            || err.kind() == std::io::ErrorKind::TimedOut =>
                                    {
                                        continue;
                                    }
                                    Err(_) => break,
                                }
                            }

                            worker_shared.connected.store(false, Ordering::Release);
                            let current = worker_shared
                                .snapshot
                                .read()
                                .expect("niri state lock poisoned")
                                .clone();
                            on_update(false, &current);
                        }
                        Err(_) => {
                            worker_shared.connected.store(false, Ordering::Release);
                        }
                    }

                    if worker_shared.stop.load(Ordering::Acquire) {
                        break;
                    }
                    sleep_interruptible(&worker_shared.stop, config.reconnect_interval);
                }
            })
            .expect("failed to spawn niri IPC thread");

        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// Returns whether the watcher is currently connected to niri.
    pub fn is_connected(&self) -> bool {
        self.shared.connected.load(Ordering::Acquire)
    }

    /// Returns a cloned snapshot of the current reduced niri state.
    pub fn snapshot(&self) -> NiriState {
        self.shared
            .snapshot
            .read()
            .expect("niri state lock poisoned")
            .clone()
    }

    /// Signals the background thread to stop and waits for it to exit.
    pub fn stop(mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for NiriWatcher {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn sleep_interruptible(stop: &AtomicBool, total: Duration) {
    let step = Duration::from_millis(50);
    let mut elapsed = Duration::ZERO;
    while elapsed < total && !stop.load(Ordering::Acquire) {
        let chunk = step.min(total - elapsed);
        thread::sleep(chunk);
        elapsed += chunk;
    }
}
