use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use bgpicker_core::niri::{NiriClientConfig, NiriWatcher};
use bgpicker_core::paths::BgPickerPaths;
use bgpicker_core::protocol::Request;

use crate::actor::{DaemonActor, DaemonActorHandle};
use crate::inotify::SettingsWatcher;
use crate::ipc::IpcServer;
use crate::renderer::{GfxRendererBackend, RendererBackend};
use crate::video::{RealVideoBackend, VideoBackend};

/// Options for starting [`DaemonInstance`].
#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub paths: BgPickerPaths,
    pub project_dir: PathBuf,
    pub restore_on_start: bool,
    pub enable_niri: bool,
    pub enable_settings_watch: bool,
}

impl DaemonOptions {
    pub fn from_env(socket_override: Option<PathBuf>, project_dir: PathBuf, restore: bool) -> Self {
        let mut paths = BgPickerPaths::from_env();
        if let Some(sock) = socket_override {
            paths.socket_override = Some(sock);
        }
        Self {
            paths,
            project_dir,
            restore_on_start: restore,
            enable_niri: true,
            enable_settings_watch: true,
        }
    }
}

/// Complete running daemon assembly (`actor` + `ipc` + `niri` + `settings watch` + `renderer` + `video`).
pub struct DaemonInstance {
    renderer: Arc<dyn RendererBackend>,
    video: Arc<dyn VideoBackend>,
    actor: Option<DaemonActor>,
    ipc: Option<IpcServer>,
    niri: Option<NiriWatcher>,
    settings_watch: Option<SettingsWatcher>,
    stop_signal: Arc<(Mutex<bool>, Condvar)>,
    stopped: AtomicBool,
}

impl DaemonInstance {
    /// Starts the live daemon backed by Wayland+EGL (`GfxRendererBackend`) and FFmpeg (`RealVideoBackend`).
    pub fn start_live(opts: DaemonOptions) -> Result<Self, String> {
        let renderer: Arc<dyn RendererBackend> = Arc::new(GfxRendererBackend::connect()?);
        let video: Arc<dyn VideoBackend> = Arc::new(RealVideoBackend::new(Arc::clone(&renderer)));
        Self::start_with_backends(opts, renderer, video)
    }

    /// Starts the daemon using caller-supplied renderer and video backends (for tests or live).
    pub fn start_with_backends(
        opts: DaemonOptions,
        renderer: Arc<dyn RendererBackend>,
        video: Arc<dyn VideoBackend>,
    ) -> Result<Self, String> {
        let actor = DaemonActor::spawn(
            opts.paths.clone(),
            opts.project_dir.clone(),
            Arc::clone(&renderer),
            Arc::clone(&video),
        );
        let actor_handle = actor.handle();

        let niri = if opts.enable_niri {
            NiriClientConfig::from_env().ok().map(|cfg| {
                let niri_actor = actor_handle.clone();
                NiriWatcher::spawn(cfg, move |connected, state| {
                    niri_actor.send_niri_update(connected, state.clone());
                })
            })
        } else {
            None
        };

        let settings_watch = if opts.enable_settings_watch {
            let watch_actor = actor_handle.clone();
            match SettingsWatcher::spawn(&opts.paths.settings_file(), move || {
                watch_actor.notify_settings_changed();
            }) {
                Ok(w) => Some(w),
                Err(err) => {
                    log::warn!("BgPicker: settings watcher disabled: {err}");
                    None
                }
            }
        } else {
            None
        };

        let stop_signal = Arc::new((Mutex::new(false), Condvar::new()));
        let ipc_stop_signal = Arc::clone(&stop_signal);
        let socket_path = opts.paths.daemon_socket_path();
        let ipc = IpcServer::bind(
            socket_path,
            actor_handle.clone(),
            Arc::new(move || {
                let (lock, cv) = &*ipc_stop_signal;
                let mut stopped = lock.lock().expect("stop signal mutex poisoned");
                *stopped = true;
                cv.notify_all();
            }),
        )
        .map_err(|e| format!("failed to bind daemon IPC socket: {e}"))?;

        if opts.restore_on_start {
            let resp = actor_handle.dispatch(Request::Startup);
            if !resp.ok {
                log::warn!(
                    "BgPicker: startup restore reported error: {}",
                    resp.error.as_deref().unwrap_or("unknown")
                );
            }
        }

        Ok(Self {
            renderer,
            video,
            actor: Some(actor),
            ipc: Some(ipc),
            niri,
            settings_watch,
            stop_signal,
            stopped: AtomicBool::new(false),
        })
    }

    pub fn actor_handle(&self) -> DaemonActorHandle {
        self.actor.as_ref().expect("actor active").handle()
    }

    pub fn socket_path(&self) -> &Path {
        self.ipc.as_ref().expect("ipc active").socket_path()
    }

    /// Returns a clone of the stop notification pair for signal handlers or wait loops.
    pub fn stop_notifier(&self) -> Arc<(Mutex<bool>, Condvar)> {
        Arc::clone(&self.stop_signal)
    }

    /// Blocks until IPC `Stop` or [`Self::signal_stop`] triggers shutdown.
    pub fn wait_for_stop(&self) {
        let (lock, cv) = &*self.stop_signal;
        let mut stopped = lock.lock().expect("stop signal mutex poisoned");
        while !*stopped {
            stopped = cv.wait(stopped).expect("stop signal mutex poisoned");
        }
    }

    pub fn signal_stop(&self) {
        let (lock, cv) = &*self.stop_signal;
        let mut stopped = lock.lock().expect("stop signal mutex poisoned");
        *stopped = true;
        cv.notify_all();
    }

    /// Deterministically shuts down IPC, watchers, actor, video decoder, and Wayland renderer.
    pub fn shutdown(&mut self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(ipc) = self.ipc.take() {
            ipc.shutdown();
        }
        if let Some(watch) = self.settings_watch.take() {
            watch.stop();
        }
        if let Some(niri) = self.niri.take() {
            niri.stop();
        }
        if let Some(actor) = self.actor.take() {
            actor.join();
        }
        self.video.shutdown();
        self.renderer.shutdown();
    }
}

impl Drop for DaemonInstance {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Resolves the bgpicker repository/project directory from CLI override, `BGPICKER_DIR`,
/// current directory, or ancestors of the running executable.
pub fn resolve_project_dir(explicit: Option<PathBuf>) -> PathBuf {
    bgpicker_core::resolve_project_dir(explicit)
}
