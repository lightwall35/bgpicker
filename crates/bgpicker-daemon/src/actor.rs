use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

use bgpicker_core::catalog::{
    cached_startup_placeholder_path, prepare_startup_placeholder,
};
use bgpicker_core::config::{
    read_settings_json, resolve_transition, set_transition_type, BgPickerConfig,
};
use bgpicker_core::niri::NiriState;
use bgpicker_core::paths::{
    atomic_write_text, BackdropParallax, BgPickerPaths, EngineKind, PersistedState, VideoMode,
    WallpaperMode,
};
use bgpicker_core::protocol::{
    BackdropAction, BackdropOverride, ClearCacheReport, DaemonStatus, EngineAction, ModeAction,
    OutputStatus, Request, Response, ResponseData, VideoModeAction,
};
use bgpicker_core::{
    read_splash_header, retire_startup_placeholders, write_splash_bin_from_packed,
};
pub use bgpicker_core::list_videos_sorted;
use bgpicker_gfx::{FrameInput, FrameModeTargets, KawaseSettings, LayerTargets, PixelFormat, TransitionSpec};

use crate::renderer::{to_gfx_parallax, to_gfx_transition, RendererBackend};
use crate::video::VideoBackend;

/// Snapshot shared lock-free/read-mostly with IPC status handlers so `status`
/// never blocks behind slow media decoding.
#[derive(Debug, Clone)]
pub struct SharedStatusSnapshot {
    pub engine: EngineKind,
    pub mode: WallpaperMode,
    pub current_wallpaper: Option<PathBuf>,
    pub video_mode: VideoMode,
    pub current_video: Option<PathBuf>,
    pub backdrop_parallax: bool,
    pub startup_placeholder: bool,
    pub placeholder_cache: bool,
    pub config: BgPickerConfig,
    pub busy: bool,
    pub niri_connected: bool,
    pub overview_open: bool,
    pub focused_output: Option<String>,
    pub last_error: Option<String>,
}

pub enum ActorMsg {
    Command {
        req: Request,
        reply: Sender<Response>,
    },
    NiriUpdate {
        connected: bool,
        state: NiriState,
    },
    SettingsChanged,
    Shutdown,
}

#[derive(Clone)]
pub struct DaemonActorHandle {
    tx: Sender<ActorMsg>,
    snapshot: Arc<RwLock<SharedStatusSnapshot>>,
    paths: BgPickerPaths,
    project_dir: PathBuf,
    renderer: Arc<dyn RendererBackend>,
}

impl DaemonActorHandle {
    pub fn paths(&self) -> &BgPickerPaths {
        &self.paths
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    pub fn send_niri_update(&self, connected: bool, state: NiriState) {
        let _ = self.tx.send(ActorMsg::NiriUpdate { connected, state });
    }

    pub fn notify_settings_changed(&self) {
        let _ = self.tx.send(ActorMsg::SettingsChanged);
    }

    pub fn shutdown_actor(&self) {
        let _ = self.tx.send(ActorMsg::Shutdown);
    }

    /// Dispatches a client IPC [`Request`], answering read-only queries immediately
    /// and serializing state-mutating operations through the actor queue.
    pub fn dispatch(&self, req: Request) -> Response {
        match req {
            Request::Ping => Response::ok(ResponseData::Pong),
            Request::Status => Response::ok(ResponseData::Status(Box::new(self.status_snapshot()))),
            Request::ResolveTransition { instant } => {
                match read_settings_json(&self.paths.settings_file()) {
                    Ok(raw) => {
                        let resolved = resolve_transition(&raw, instant);
                        Response::ok(ResponseData::ResolvedTransition(resolved))
                    }
                    Err(err) => Response::err(err.to_string()),
                }
            }
            Request::Catalog {
                image_dir,
                video_dir,
            } => {
                let img = image_dir.unwrap_or_else(|| self.project_dir.join("wallpapers"));
                let vid = video_dir.unwrap_or_else(|| self.project_dir.join("dynamic"));
                match bgpicker_core::catalog::build_bundle(&img, &vid, &self.paths.cache_dir) {
                    Ok(bundle) => Response::ok(ResponseData::Catalog(bundle)),
                    Err(err) => Response::err(err.to_string()),
                }
            }
            mutating => {
                let (reply_tx, reply_rx) = mpsc::channel();
                if self
                    .tx
                    .send(ActorMsg::Command {
                        req: mutating,
                        reply: reply_tx,
                    })
                    .is_err()
                {
                    return Response::err("daemon actor stopped");
                }
                reply_rx
                    .recv()
                    .unwrap_or_else(|_| Response::err("daemon actor dropped reply"))
            }
        }
    }

    pub fn status_snapshot(&self) -> DaemonStatus {
        let snap = self
            .snapshot
            .read()
            .expect("status snapshot lock poisoned")
            .clone();
        let outputs = self
            .renderer
            .snapshots()
            .into_iter()
            .map(|o| OutputStatus {
                name: o.name,
                width: o.wallpaper.logical_size.0,
                height: o.wallpaper.logical_size.1,
                scale: f64::from(o.wallpaper.scale_120) / 120.0,
                progress: f64::from(o.wallpaper.animated_progress),
                mode_blend: f64::from(o.wallpaper.mode_blend),
                transition_active: o.wallpaper.transition_active,
            })
            .collect();

        DaemonStatus {
            engine: snap.engine,
            mode: snap.mode,
            current_wallpaper: snap.current_wallpaper,
            video_mode: snap.video_mode,
            current_video: snap.current_video,
            backdrop_parallax: snap.backdrop_parallax,
            startup_placeholder: snap.startup_placeholder,
            placeholder_cache: snap.placeholder_cache,
            transition: snap.config.transition,
            parallax: snap.config.parallax,
            busy: snap.busy,
            niri_connected: snap.niri_connected,
            overview_open: snap.overview_open,
            focused_output: snap.focused_output,
            outputs,
            error: snap.last_error,
        }
    }
}

pub struct DaemonActor {
    handle: DaemonActorHandle,
    thread: Option<JoinHandle<()>>,
}

impl DaemonActor {
    pub fn spawn(
        paths: BgPickerPaths,
        project_dir: PathBuf,
        renderer: Arc<dyn RendererBackend>,
        video: Arc<dyn VideoBackend>,
    ) -> Self {
        let persisted = PersistedState::load(&paths);
        let raw_settings = read_settings_json(&paths.settings_file())
            .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
        let config = BgPickerConfig::from_value(&raw_settings);

        let init_snapshot = SharedStatusSnapshot {
            engine: persisted.engine,
            mode: persisted.mode,
            current_wallpaper: persisted.current_wallpaper.clone(),
            video_mode: persisted.video_mode,
            current_video: persisted.video_wallpaper.clone(),
            backdrop_parallax: persisted.backdrop_parallax.is_on(),
            startup_placeholder: persisted.startup_placeholder,
            placeholder_cache: persisted.placeholder_cache,
            config,
            busy: false,
            niri_connected: false,
            overview_open: false,
            focused_output: None,
            last_error: None,
        };

        let snapshot = Arc::new(RwLock::new(init_snapshot));
        let (tx, rx) = mpsc::channel();

        let handle = DaemonActorHandle {
            tx,
            snapshot: Arc::clone(&snapshot),
            paths: paths.clone(),
            project_dir: project_dir.clone(),
            renderer: Arc::clone(&renderer),
        };

        let mut runner = ActorRunner {
            paths,
            project_dir,
            renderer,
            video,
            state: persisted,
            raw_settings,
            config,
            niri_connected: false,
            niri_state: NiriState::initial(),
            last_output_progress: std::collections::HashMap::new(),
            last_error: None,
            restored_once: false,
            startup_seed: None,
            snapshot,
        };

        // Sync initial settings & surface modes before any frame arrives.
        runner.sync_renderer_initial_modes();

        let thread = thread::Builder::new()
            .name("bgpicker-actor".into())
            .spawn(move || runner.run(rx))
            .expect("failed to spawn bgpicker-actor thread");

        Self {
            handle,
            thread: Some(thread),
        }
    }

    pub fn handle(&self) -> DaemonActorHandle {
        self.handle.clone()
    }

    pub fn join(mut self) {
        self.handle.shutdown_actor();
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DaemonActor {
    fn drop(&mut self) {
        self.handle.shutdown_actor();
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

struct ActorRunner {
    paths: BgPickerPaths,
    project_dir: PathBuf,
    renderer: Arc<dyn RendererBackend>,
    video: Arc<dyn VideoBackend>,
    state: PersistedState,
    raw_settings: Value,
    config: BgPickerConfig,
    niri_connected: bool,
    niri_state: NiriState,
    last_output_progress: std::collections::HashMap<String, f32>,
    last_error: Option<String>,
    restored_once: bool,
    startup_seed: Option<StartupSeedKind>,
    snapshot: Arc<RwLock<SharedStatusSnapshot>>,
}

enum StartupSeedKind {
    Full { wallpaper: PathBuf },
    Video { seeded: bool },
}

impl ActorRunner {
    fn sync_renderer_initial_modes(&self) {
        let _ = self
            .renderer
            .update_settings(to_gfx_parallax(self.config.parallax), KawaseSettings::default());
        let (fg_parallax, bg_parallax) = match self.state.engine {
            EngineKind::Simple => (false, false),
            EngineKind::Full => (
                self.state.mode == WallpaperMode::Parallax,
                self.state.backdrop_parallax.is_on(),
            ),
            EngineKind::Video => (
                self.state.video_mode == VideoMode::Parallax,
                self.state.backdrop_parallax.is_on(),
            ),
        };
        let _ = self
            .renderer
            .set_mode(fg_parallax, Some(0), LayerTargets::WallpaperOnly);
        let _ = self
            .renderer
            .set_mode(bg_parallax, Some(0), LayerTargets::BackdropOnly);
    }

    fn publish_snapshot(&self, busy: bool) {
        if let Ok(mut guard) = self.snapshot.write() {
            *guard = SharedStatusSnapshot {
                engine: self.state.engine,
                mode: self.state.mode,
                current_wallpaper: self.state.current_wallpaper.clone(),
                video_mode: self.state.video_mode,
                current_video: self.state.video_wallpaper.clone(),
                backdrop_parallax: self.state.backdrop_parallax.is_on(),
                startup_placeholder: self.state.startup_placeholder,
                placeholder_cache: self.state.placeholder_cache,
                config: self.config,
                busy,
                niri_connected: self.niri_connected,
                overview_open: self.niri_state.overview_open,
                focused_output: self.niri_state.focused_output.clone(),
                last_error: self.last_error.clone(),
            };
        }
    }

    fn run(&mut self, rx: Receiver<ActorMsg>) {
        while let Ok(msg) = rx.recv() {
            match msg {
                ActorMsg::Shutdown => break,
                ActorMsg::SettingsChanged => {
                    let _ = self.reload_settings_inner();
                    self.publish_snapshot(false);
                }
                ActorMsg::NiriUpdate { connected, state } => {
                    self.handle_niri_update(connected, state);
                    self.publish_snapshot(false);
                }
                ActorMsg::Command { req, reply } => {
                    self.publish_snapshot(true);
                    let is_stop = matches!(req, Request::Stop);
                    let resp = self.handle_command(req);
                    self.last_error = resp.error.clone();
                    self.publish_snapshot(false);
                    let _ = reply.send(resp);
                    if is_stop {
                        break;
                    }
                }
            }
        }
    }

    fn handle_niri_update(&mut self, connected: bool, next: NiriState) {
        let prev_overview = self.niri_state.overview_open;
        let prev_connected = self.niri_connected;
        self.niri_connected = connected;
        self.niri_state = next;

        if self.niri_state.overview_open != prev_overview {
            self.video.set_overview_open(self.niri_state.overview_open);
        }

        if !connected {
            return;
        }

        let mut output_names: Vec<String> = self
            .renderer
            .snapshots()
            .into_iter()
            .map(|o| o.name)
            .collect();
        for ws in &self.niri_state.workspaces {
            if let Some(ref name) = ws.output {
                if !output_names.iter().any(|n| n == name) {
                    output_names.push(name.clone());
                }
            }
        }

        let any_parallax = match self.state.engine {
            EngineKind::Simple => false,
            EngineKind::Full => {
                self.state.mode == WallpaperMode::Parallax || self.state.backdrop_parallax.is_on()
            }
            EngineKind::Video => {
                self.state.video_mode == VideoMode::Parallax
                    || self.state.backdrop_parallax.is_on()
            }
        };

        let col_span = self.config.parallax.column_span as usize;
        for name in output_names {
            let progress = self.niri_state.progress(&name, col_span) as f32;
            let changed = self
                .last_output_progress
                .get(&name)
                .is_none_or(|&prev| (prev - progress).abs() > 1e-5);
            if changed || !prev_connected {
                self.last_output_progress.insert(name.clone(), progress);
                let dur = if !prev_connected || !any_parallax {
                    0
                } else {
                    self.config.parallax.duration_ms
                };
                let _ = self.renderer.set_progress(Some(name), progress, Some(dur));
            }
        }
    }

    fn handle_command(&mut self, req: Request) -> Response {
        match req {
            Request::Startup => match self.cmd_startup() {
                Ok(()) => Response::ok_empty(),
                Err(e) => Response::err(e),
            },
            Request::StartupSeed { placeholder } => match self.cmd_startup_seed(placeholder) {
                Ok(status) => Response::ok(ResponseData::Message(status.into())),
                Err(e) => Response::err(e),
            },
            Request::StartupSettle => match self.cmd_startup_settle() {
                Ok(()) => Response::ok_empty(),
                Err(e) => Response::err(e),
            },
            Request::Restore => match self.cmd_restore() {
                Ok(()) => Response::ok_empty(),
                Err(e) => Response::err(e),
            },
            Request::Stop => {
                self.video.stop();
                Response::ok_empty()
            }
            Request::ReloadSettings => match self.reload_settings_inner() {
                Ok(()) => Response::ok(ResponseData::TransitionSettings(self.config.transition)),
                Err(e) => Response::err(e),
            },
            Request::ApplyStatic {
                path,
                mode,
                instant,
                backdrop,
            } => match self.cmd_apply_static(&path, mode, instant, EngineKind::Full, backdrop) {
                Ok(()) => Response::ok_empty(),
                Err(e) => Response::err(e),
            },
            Request::SetMode { mode, backdrop } => match self.cmd_set_mode(mode, backdrop) {
                Ok(()) => Response::ok(ResponseData::Message(self.state.mode.as_str().into())),
                Err(e) => Response::err(e),
            },
            Request::BackdropParallax { action } => match self.cmd_backdrop_parallax(action) {
                Ok(()) => Response::ok(ResponseData::Message(
                    self.state.backdrop_parallax.as_str().into(),
                )),
                Err(e) => Response::err(e),
            },
            Request::StartupPlaceholder { action } => match self.cmd_startup_placeholder(action) {
                Ok(enabled) => Response::ok(ResponseData::Message(
                    if enabled { "on" } else { "off" }.into(),
                )),
                Err(e) => Response::err(e),
            },
            Request::PlaceholderCache { action } => match self.cmd_placeholder_cache(action) {
                Ok(enabled) => Response::ok(ResponseData::Message(
                    if enabled { "on" } else { "off" }.into(),
                )),
                Err(e) => Response::err(e),
            },
            Request::ApplyVideo {
                mode,
                path,
                backdrop,
            } => match self.cmd_apply_video(mode, path, false, backdrop) {
                Ok(()) => Response::ok_empty(),
                Err(e) => Response::err(e),
            },
            Request::SetEngine { engine } => match self.cmd_set_engine(engine) {
                Ok(()) => Response::ok(ResponseData::Message(self.state.engine.as_str().into())),
                Err(e) => Response::err(e),
            },
            Request::SetTransition { transition_type } => {
                match set_transition_type(
                    &self.paths.settings_file(),
                    &self.paths.switch_lock_file(),
                    transition_type,
                ) {
                    Ok(()) => {
                        if let Err(e) = self.reload_settings_inner() {
                            return Response::err(e);
                        }
                        Response::ok(ResponseData::TransitionSettings(self.config.transition))
                    }
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Request::ClearCache => match self.cmd_clear_cache() {
                Ok(report) => Response::ok(ResponseData::CacheCleared(report)),
                Err(e) => Response::err(e),
            },
            // Handled synchronously before actor queue, kept exhaustive for safety.
            Request::Ping => Response::ok(ResponseData::Pong),
            Request::Status
            | Request::ResolveTransition { .. }
            | Request::Catalog { .. } => Response::err("unexpected read request in actor"),
        }
    }

    fn reload_settings_inner(&mut self) -> Result<(), String> {
        let raw = read_settings_json(&self.paths.settings_file()).map_err(|e| e.to_string())?;
        let next = BgPickerConfig::from_value(&raw);
        self.raw_settings = raw;
        self.config = next;
        self.renderer
            .update_settings(to_gfx_parallax(next.parallax), KawaseSettings::default())?;
        Ok(())
    }

    fn resolve_backdrop_target(
        &self,
        override_mode: BackdropOverride,
        sync_when_auto: bool,
        synced_parallax: bool,
    ) -> BackdropParallax {
        match override_mode {
            BackdropOverride::Auto => {
                if sync_when_auto {
                    backdrop_from_bool(synced_parallax)
                } else {
                    self.state.backdrop_parallax
                }
            }
            BackdropOverride::On => BackdropParallax::On,
            BackdropOverride::Off => BackdropParallax::Off,
            BackdropOverride::Keep => self.state.backdrop_parallax,
        }
    }

    fn apply_layer_modes(
        &self,
        fg_parallax: bool,
        bg_parallax: bool,
        fg_dur: u32,
        bg_dur: u32,
        update_fg: bool,
        update_bg: bool,
    ) -> Result<(), String> {
        if update_fg && update_bg && fg_parallax == bg_parallax && fg_dur == bg_dur {
            return self
                .renderer
                .set_mode(fg_parallax, Some(fg_dur), LayerTargets::Both);
        }
        if update_fg {
            self.renderer
                .set_mode(fg_parallax, Some(fg_dur), LayerTargets::WallpaperOnly)?;
        }
        if update_bg {
            self.renderer
                .set_mode(bg_parallax, Some(bg_dur), LayerTargets::BackdropOnly)?;
        }
        Ok(())
    }

    fn wait_and_retire_startup_placeholder(&self) {
        self.renderer
            .wait_presented(Duration::from_millis(1200));
        retire_startup_placeholders(&self.paths, &[]);
    }

    fn cmd_startup(&mut self) -> Result<(), String> {
        if self.restored_once {
            return Ok(());
        }
        self.startup_seed = None;
        self.restored_once = true;
        let res = match self.state.engine {
            EngineKind::Video => {
                if let Err(err) = self.cmd_apply_video(None, None, true, BackdropOverride::Keep) {
                    log::warn!("BgPicker: video restore failed ({err}); falling back to static");
                    if let Some(wp) = self.state.current_wallpaper.clone() {
                        if wp.is_file() {
                            return self.cmd_apply_static(
                                &wp,
                                Some(WallpaperMode::Awww),
                                true,
                                EngineKind::Simple,
                                BackdropOverride::Keep,
                            );
                        }
                    }
                }
                Ok(())
            }
            EngineKind::Full => {
                if let Some(wp) = self.state.current_wallpaper.clone() {
                    if wp.is_file() {
                        self.cmd_apply_static(
                            &wp,
                            Some(self.state.mode),
                            true,
                            EngineKind::Full,
                            BackdropOverride::Keep,
                        )?;
                    }
                }
                Ok(())
            }
            EngineKind::Simple => {
                if let Some(wp) = self.state.current_wallpaper.clone() {
                    if wp.is_file() {
                        return self.cmd_apply_static(
                            &wp,
                            Some(WallpaperMode::Awww),
                            true,
                            EngineKind::Simple,
                            BackdropOverride::Keep,
                        );
                    }
                }
                Ok(())
            }
        };
        if res.is_ok() {
            self.wait_and_retire_startup_placeholder();
        }
        res
    }

    fn cmd_startup_seed(&mut self, placeholder: Option<PathBuf>) -> Result<&'static str, String> {
        if self.restored_once {
            return Ok("already_restored");
        }
        match self.state.engine {
            EngineKind::Full => {
                let Some(wp) = self.state.current_wallpaper.clone().filter(|p| p.is_file()) else {
                    self.startup_seed = None;
                    return Ok("unseeded");
                };
                let canonical = fs::canonicalize(&wp).map_err(|e| {
                    format!("BgPicker: failed to resolve {}: {e}", wp.display())
                })?;
                let frame = self
                    .renderer
                    .decode_image(&canonical, self.config.parallax.max_texture_size)?;
                // Present initial frame centered (`mode_blend = 0.0`) so it visually matches
                // the temporary static placeholder before smoothly gliding into parallax.
                self.renderer.apply_frame(
                    frame,
                    TransitionSpec::instant(),
                    LayerTargets::Both,
                    FrameModeTargets {
                        wallpaper_parallax: Some(false),
                        backdrop_parallax: Some(false),
                    },
                )?;
                self.startup_seed = Some(StartupSeedKind::Full {
                    wallpaper: canonical,
                });
                self.wait_and_retire_startup_placeholder();
                Ok("seeded")
            }
            EngineKind::Video => {
                let candidate = placeholder
                    .filter(|p| p.is_file())
                    .or_else(|| self.state.current_wallpaper.clone().filter(|p| p.is_file()));
                if let Some(ph) = candidate {
                    let canonical = fs::canonicalize(&ph).map_err(|e| {
                        format!("BgPicker: failed to resolve {}: {e}", ph.display())
                    })?;
                    match self
                        .renderer
                        .decode_image(&canonical, self.config.parallax.max_texture_size)
                    {
                        Ok(frame) => {
                            self.renderer.apply_frame(
                                frame,
                                TransitionSpec::instant(),
                                LayerTargets::Both,
                                FrameModeTargets {
                                    wallpaper_parallax: Some(false),
                                    backdrop_parallax: Some(false),
                                },
                            )?;
                            self.startup_seed = Some(StartupSeedKind::Video { seeded: true });
                            self.wait_and_retire_startup_placeholder();
                            return Ok("seeded");
                        }
                        Err(err) => {
                            log::warn!("BgPicker: placeholder decode failed ({err})");
                        }
                    }
                }
                self.startup_seed = Some(StartupSeedKind::Video { seeded: false });
                Ok("unseeded")
            }
            EngineKind::Simple => {
                self.startup_seed = None;
                Ok("unseeded")
            }
        }
    }

    fn cmd_startup_settle(&mut self) -> Result<(), String> {
        let Some(seed) = self.startup_seed.take() else {
            return self.cmd_startup();
        };
        if self.restored_once {
            return Ok(());
        }
        match seed {
            StartupSeedKind::Full { wallpaper } => {
                self.restored_once = true;
                let fg_parallax = self.state.mode == WallpaperMode::Parallax;
                let bg_parallax = self.state.backdrop_parallax.is_on();
                let dur = self.config.parallax.mode_transition_ms;
                self.apply_layer_modes(
                    fg_parallax,
                    bg_parallax,
                    dur,
                    dur,
                    fg_parallax,
                    bg_parallax,
                )?;
                spawn_compat_blur_update(&self.paths, &wallpaper);
                if self.state.placeholder_cache {
                    let dst = self.target_splash_pixel_size();
                    let bin = self.paths.startup_splash_fg_bin_file();
                    let splash_target = (!splash_matches_size(&bin, dst.0, dst.1)).then_some(
                        SplashBinTarget {
                            bin_path: bin,
                            dst_size: dst,
                            frame: None,
                        },
                    );
                    spawn_placeholder_cache_update(&self.paths, &wallpaper, false, splash_target);
                }
                retire_startup_placeholders(&self.paths, &[]);
                Ok(())
            }
            StartupSeedKind::Video { seeded } => {
                self.restored_once = true;
                if let Err(err) =
                    self.cmd_apply_video(None, None, !seeded, BackdropOverride::Keep)
                {
                    log::warn!("BgPicker: video restore failed ({err}); falling back to static");
                    if let Some(wp) = self.state.current_wallpaper.clone() {
                        if wp.is_file() {
                            return self.cmd_apply_static(
                                &wp,
                                Some(WallpaperMode::Awww),
                                !seeded,
                                EngineKind::Simple,
                                BackdropOverride::Keep,
                            );
                        }
                    }
                }
                if seeded {
                    retire_startup_placeholders(&self.paths, &[]);
                } else {
                    self.wait_and_retire_startup_placeholder();
                }
                Ok(())
            }
        }
    }

    fn cmd_restore(&mut self) -> Result<(), String> {
        self.restored_once = true;
        let res = match self.state.engine {
            EngineKind::Video => {
                if self.state.current_video_available(&self.project_dir).is_some() {
                    self.cmd_apply_video(None, None, true, BackdropOverride::Keep)
                } else {
                    Ok(())
                }
            }
            EngineKind::Full => {
                if let Some(wp) = self.state.current_wallpaper.clone() {
                    if wp.is_file() {
                        self.cmd_apply_static(
                            &wp,
                            Some(self.state.mode),
                            true,
                            EngineKind::Full,
                            BackdropOverride::Keep,
                        )?;
                    }
                }
                Ok(())
            }
            EngineKind::Simple => {
                if let Some(wp) = self.state.current_wallpaper.clone() {
                    if wp.is_file() {
                        return self.cmd_apply_static(
                            &wp,
                            Some(WallpaperMode::Awww),
                            true,
                            EngineKind::Simple,
                            BackdropOverride::Keep,
                        );
                    }
                }
                Ok(())
            }
        };
        if res.is_ok() {
            self.wait_and_retire_startup_placeholder();
        }
        res
    }

    fn cmd_apply_static(
        &mut self,
        path: &Path,
        requested_mode: Option<WallpaperMode>,
        instant: bool,
        target_engine: EngineKind,
        backdrop: BackdropOverride,
    ) -> Result<(), String> {
        if !path.is_file() {
            return Err(format!(
                "BgPicker: select an existing wallpaper first: {}",
                path.display()
            ));
        }
        let canonical = fs::canonicalize(path).map_err(|e| {
            format!(
                "BgPicker: failed to resolve {}: {e}",
                path.display()
            )
        })?;
        let frame = self
            .renderer
            .decode_image(&canonical, self.config.parallax.max_texture_size)?;
        let splash_dst = self.target_splash_pixel_size();
        let splash_bin = self.paths.startup_splash_fg_bin_file();
        let splash_target = (self.state.placeholder_cache
            && (self.state.engine != target_engine
                || self.state.current_wallpaper.as_deref() != Some(canonical.as_path())
                || !splash_matches_size(&splash_bin, splash_dst.0, splash_dst.1)))
        .then(|| SplashBinTarget {
            bin_path: splash_bin,
            dst_size: splash_dst,
            frame: Some(frame.clone()),
        });

        let target_mode = match target_engine {
            EngineKind::Simple => WallpaperMode::Awww,
            EngineKind::Full => requested_mode.unwrap_or(self.state.mode),
            EngineKind::Video => return Err("internal error: video engine in apply_static".into()),
        };

        let target_backdrop = match target_engine {
            EngineKind::Simple => self.state.backdrop_parallax,
            EngineKind::Full => self.resolve_backdrop_target(
                backdrop,
                requested_mode.is_some() || self.state.engine == EngineKind::Video,
                target_mode == WallpaperMode::Parallax,
            ),
            EngineKind::Video => return Err("internal error: video engine in apply_static".into()),
        };

        let resolved = resolve_transition(
            &self.raw_settings,
            instant || target_engine == EngineKind::Simple,
        );
        let spec = to_gfx_transition(resolved);

        // Halt any live video frame pushes BEFORE committing the static frame.
        self.video.stop();

        let fg_parallax =
            target_engine == EngineKind::Full && target_mode == WallpaperMode::Parallax;
        let bg_parallax = target_engine == EngineKind::Full && target_backdrop.is_on();
        let modes = FrameModeTargets {
            wallpaper_parallax: Some(fg_parallax),
            backdrop_parallax: Some(bg_parallax),
        };
        self.renderer
            .apply_frame(frame, spec, LayerTargets::Both, modes)?;

        atomic_write_text(&self.paths.engine_file(), target_engine.as_str())
            .map_err(|e| e.to_string())?;
        atomic_write_text(
            &self.paths.current_wallpaper_file(),
            &canonical.to_string_lossy(),
        )
        .map_err(|e| e.to_string())?;
        if target_engine == EngineKind::Full {
            atomic_write_text(&self.paths.mode_file(), target_mode.as_str())
                .map_err(|e| e.to_string())?;
            self.state.mode = target_mode;
            if target_backdrop != self.state.backdrop_parallax {
                atomic_write_text(&self.paths.backdrop_parallax_file(), target_backdrop.as_str())
                    .map_err(|e| e.to_string())?;
                self.state.backdrop_parallax = target_backdrop;
            }
        }
        self.state.engine = target_engine;
        self.state.current_wallpaper = Some(canonical.clone());
        self.restored_once = true;

        spawn_compat_blur_update(&self.paths, &canonical);
        if self.state.placeholder_cache {
            spawn_placeholder_cache_update(&self.paths, &canonical, false, splash_target);
        }
        Ok(())
    }

    fn cmd_set_mode(&mut self, action: ModeAction, backdrop: BackdropOverride) -> Result<(), String> {
        if self.state.engine == EngineKind::Simple {
            return Err("BgPicker: mode changes require the full engine".into());
        }
        let target_mode = match action {
            ModeAction::Awww => WallpaperMode::Awww,
            ModeAction::Parallax => WallpaperMode::Parallax,
            ModeAction::Toggle => self.state.mode.toggled(),
        };

        let wp = self
            .state
            .current_wallpaper
            .clone()
            .filter(|p| p.is_file())
            .ok_or_else(|| "BgPicker: select an existing wallpaper first".to_string())?;

        if self.state.engine == EngineKind::Video {
            return self.cmd_apply_static(&wp, Some(target_mode), false, EngineKind::Full, backdrop);
        }

        let target_backdrop = self.resolve_backdrop_target(
            backdrop,
            true,
            target_mode == WallpaperMode::Parallax,
        );
        let mode_changed = target_mode != self.state.mode;
        let backdrop_changed = target_backdrop != self.state.backdrop_parallax;
        let dur = self.config.parallax.mode_transition_ms;

        self.apply_layer_modes(
            target_mode == WallpaperMode::Parallax,
            target_backdrop.is_on(),
            dur,
            dur,
            mode_changed,
            backdrop_changed,
        )?;

        if mode_changed {
            atomic_write_text(&self.paths.mode_file(), target_mode.as_str())
                .map_err(|e| e.to_string())?;
            self.state.mode = target_mode;
        }
        if backdrop_changed {
            atomic_write_text(&self.paths.backdrop_parallax_file(), target_backdrop.as_str())
                .map_err(|e| e.to_string())?;
            self.state.backdrop_parallax = target_backdrop;
        }
        Ok(())
    }

    fn cmd_backdrop_parallax(&mut self, action: BackdropAction) -> Result<(), String> {
        let target = match action {
            BackdropAction::On => BackdropParallax::On,
            BackdropAction::Off => BackdropParallax::Off,
            BackdropAction::Toggle => self.state.backdrop_parallax.toggled(),
        };
        if target == self.state.backdrop_parallax {
            return Ok(());
        }
        if self.state.engine != EngineKind::Simple {
            self.renderer.set_mode(
                target.is_on(),
                Some(self.config.parallax.mode_transition_ms),
                LayerTargets::BackdropOnly,
            )?;
        }
        atomic_write_text(&self.paths.backdrop_parallax_file(), target.as_str())
            .map_err(|e| e.to_string())?;
        self.state.backdrop_parallax = target;
        Ok(())
    }

    fn cmd_startup_placeholder(&mut self, action: BackdropAction) -> Result<bool, String> {
        let target = match action {
            BackdropAction::On => true,
            BackdropAction::Off => false,
            BackdropAction::Toggle => !self.state.startup_placeholder,
        };
        if target != self.state.startup_placeholder {
            atomic_write_text(
                &self.paths.startup_placeholder_file(),
                if target { "on" } else { "off" },
            )
            .map_err(|e| e.to_string())?;
            self.state.startup_placeholder = target;
        }
        Ok(target)
    }

    fn cmd_placeholder_cache(&mut self, action: BackdropAction) -> Result<bool, String> {
        let target = match action {
            BackdropAction::On => true,
            BackdropAction::Off => false,
            BackdropAction::Toggle => !self.state.placeholder_cache,
        };
        if target != self.state.placeholder_cache {
            atomic_write_text(
                &self.paths.placeholder_cache_file(),
                if target { "on" } else { "off" },
            )
            .map_err(|e| e.to_string())?;
            self.state.placeholder_cache = target;
        }
        if target {
            let active_is_video = self.state.engine == EngineKind::Video;
            let dst = self.target_splash_pixel_size();
            let bin = self.paths.startup_splash_fg_bin_file();
            if let Some(ref wp) = self.state.current_wallpaper {
                if wp.is_file() {
                    let splash = (!active_is_video).then(|| SplashBinTarget {
                        bin_path: bin.clone(),
                        dst_size: dst,
                        frame: None,
                    });
                    spawn_placeholder_cache_update(&self.paths, wp, false, splash);
                }
            }
            if let Some(vid) = self.state.current_video_available(&self.project_dir) {
                let splash = active_is_video.then_some(SplashBinTarget {
                    bin_path: bin,
                    dst_size: dst,
                    frame: None,
                });
                spawn_placeholder_cache_update(&self.paths, &vid, true, splash);
            }
        }
        Ok(target)
    }

    fn cmd_apply_video(
        &mut self,
        mode: Option<VideoModeAction>,
        path: Option<PathBuf>,
        instant: bool,
        backdrop: BackdropOverride,
    ) -> Result<(), String> {
        let target_mode = match mode {
            None => self.state.video_mode,
            Some(VideoModeAction::Normal) => VideoMode::Normal,
            Some(VideoModeAction::Parallax) => VideoMode::Parallax,
            Some(VideoModeAction::Toggle) => self.state.video_mode.toggled(),
        };
        let target_backdrop = self.resolve_backdrop_target(
            backdrop,
            mode.is_some() || self.state.engine != EngineKind::Video,
            target_mode == VideoMode::Parallax,
        );

        let raw_path = match path {
            Some(p) => p,
            None => self
                .state
                .current_video_available(&self.project_dir)
                .ok_or_else(|| "BgPicker: no video found in dynamic/".to_string())?,
        };
        if !raw_path.is_file() {
            return Err(format!(
                "BgPicker: video file not found: {}",
                raw_path.display()
            ));
        }
        let canonical = fs::canonicalize(&raw_path).map_err(|e| {
            format!(
                "BgPicker: failed to resolve video {}: {e}",
                raw_path.display()
            )
        })?;

        // Fast path: same video already playing -> smooth in-surface mode blend only.
        if self.state.engine == EngineKind::Video && self.video.is_playing_path(&canonical) {
            let fg_changed = target_mode != self.state.video_mode;
            let bg_changed = target_backdrop != self.state.backdrop_parallax;
            let dur = self.config.parallax.mode_transition_ms;
            self.apply_layer_modes(
                target_mode == VideoMode::Parallax,
                target_backdrop.is_on(),
                dur,
                dur,
                fg_changed,
                bg_changed,
            )?;
            if fg_changed {
                atomic_write_text(&self.paths.video_mode_file(), target_mode.as_str())
                    .map_err(|e| e.to_string())?;
                self.state.video_mode = target_mode;
            }
            if bg_changed {
                atomic_write_text(&self.paths.backdrop_parallax_file(), target_backdrop.as_str())
                    .map_err(|e| e.to_string())?;
                self.state.backdrop_parallax = target_backdrop;
            }
            return Ok(());
        }

        let ready = self.video.open_and_wait(
            &canonical,
            self.niri_state.overview_open,
            Duration::from_secs(15),
        )?;

        let resolved = resolve_transition(&self.raw_settings, instant);
        let spec = to_gfx_transition(resolved);

        let fg_parallax = target_mode == VideoMode::Parallax;
        let bg_parallax = target_backdrop.is_on();
        if let Some(bg_frame) = ready.bg_frame {
            self.renderer.apply_frame(
                ready.fg_frame,
                spec,
                LayerTargets::WallpaperOnly,
                FrameModeTargets {
                    wallpaper_parallax: Some(fg_parallax),
                    backdrop_parallax: None,
                },
            )?;
            self.renderer.apply_frame(
                bg_frame,
                spec,
                LayerTargets::BackdropOnly,
                FrameModeTargets {
                    wallpaper_parallax: None,
                    backdrop_parallax: Some(bg_parallax),
                },
            )?;
        } else {
            self.renderer.apply_frame(
                ready.fg_frame,
                spec,
                LayerTargets::Both,
                FrameModeTargets {
                    wallpaper_parallax: Some(fg_parallax),
                    backdrop_parallax: Some(bg_parallax),
                },
            )?;
        }

        self.video.activate_stream(ready.stream_id);

        atomic_write_text(&self.paths.engine_file(), EngineKind::Video.as_str())
            .map_err(|e| e.to_string())?;
        atomic_write_text(
            &self.paths.video_wallpaper_file(),
            &canonical.to_string_lossy(),
        )
        .map_err(|e| e.to_string())?;
        atomic_write_text(&self.paths.video_mode_file(), target_mode.as_str())
            .map_err(|e| e.to_string())?;
        if target_backdrop != self.state.backdrop_parallax {
            atomic_write_text(&self.paths.backdrop_parallax_file(), target_backdrop.as_str())
                .map_err(|e| e.to_string())?;
            self.state.backdrop_parallax = target_backdrop;
        }

        let splash_dst = self.target_splash_pixel_size();
        let splash_bin = self.paths.startup_splash_fg_bin_file();
        let splash_target = (self.state.placeholder_cache
            && (self.state.engine != EngineKind::Video
                || self.state.video_wallpaper.as_deref() != Some(canonical.as_path())
                || !splash_matches_size(&splash_bin, splash_dst.0, splash_dst.1)))
        .then_some(SplashBinTarget {
            bin_path: splash_bin,
            dst_size: splash_dst,
            frame: None,
        });
        self.state.engine = EngineKind::Video;
        self.state.video_wallpaper = Some(canonical.clone());
        self.state.video_mode = target_mode;
        self.restored_once = true;
        if self.state.placeholder_cache {
            spawn_placeholder_cache_update(&self.paths, &canonical, true, splash_target);
        }
        Ok(())
    }

    fn target_splash_pixel_size(&self) -> (u32, u32) {
        self.renderer
            .snapshots()
            .into_iter()
            .map(|o| o.wallpaper.pixel_size)
            .find(|&(w, h)| w > 0 && h > 0)
            .unwrap_or((2560, 1600))
    }

    fn cmd_set_engine(&mut self, action: EngineAction) -> Result<(), String> {
        let target = match action {
            EngineAction::Full => EngineKind::Full,
            EngineAction::Simple => EngineKind::Simple,
            EngineAction::Video => EngineKind::Video,
            EngineAction::Toggle => {
                if self.state.engine == EngineKind::Full {
                    EngineKind::Simple
                } else {
                    EngineKind::Full
                }
            }
        };

        match target {
            EngineKind::Video => self.cmd_apply_video(None, None, false, BackdropOverride::Auto),
            EngineKind::Simple => {
                let wp = self
                    .state
                    .current_wallpaper
                    .clone()
                    .filter(|p| p.is_file())
                    .ok_or_else(|| "BgPicker: select a wallpaper first".to_string())?;
                self.cmd_apply_static(
                    &wp,
                    Some(WallpaperMode::Awww),
                    true,
                    EngineKind::Simple,
                    BackdropOverride::Keep,
                )
            }
            EngineKind::Full => {
                let wp = self
                    .state
                    .current_wallpaper
                    .clone()
                    .filter(|p| p.is_file())
                    .ok_or_else(|| "BgPicker: select a wallpaper first".to_string())?;
                let instant = self.state.engine != EngineKind::Video;
                let backdrop = if self.state.engine == EngineKind::Video {
                    BackdropOverride::Auto
                } else {
                    BackdropOverride::Keep
                };
                self.cmd_apply_static(&wp, Some(self.state.mode), instant, EngineKind::Full, backdrop)
            }
        }
    }

    fn cmd_clear_cache(&mut self) -> Result<ClearCacheReport, String> {
        let (rss_before_kib, pss_before_kib) = read_self_smaps_kib();
        let video_backend_stopped = self.state.engine != EngineKind::Video;
        if video_backend_stopped {
            self.video.shutdown();
        }
        self.renderer.trim_idle()?;
        trim_process_heap();
        let (rss_after_kib, pss_after_kib) = read_self_smaps_kib();
        Ok(ClearCacheReport {
            rss_before_kib,
            rss_after_kib,
            pss_before_kib,
            pss_after_kib,
            video_backend_stopped,
        })
    }
}

fn read_self_smaps_kib() -> (u64, u64) {
    let Ok(text) = fs::read_to_string("/proc/self/smaps_rollup") else {
        return (0, 0);
    };
    let mut rss = 0u64;
    let mut pss = 0u64;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some("Rss:"), Some(v)) => rss = v.parse().unwrap_or(0),
            (Some("Pss:"), Some(v)) => pss = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    (rss, pss)
}

#[cfg(target_env = "gnu")]
fn trim_process_heap() {
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(target_env = "gnu"))]
fn trim_process_heap() {}

fn backdrop_from_bool(on: bool) -> BackdropParallax {
    if on {
        BackdropParallax::On
    } else {
        BackdropParallax::Off
    }
}

trait PersistedStateExt {
    fn current_video_available(&self, project_dir: &Path) -> Option<PathBuf>;
}

impl PersistedStateExt for PersistedState {
    fn current_video_available(&self, project_dir: &Path) -> Option<PathBuf> {
        if let Some(ref saved) = self.video_wallpaper {
            if saved.is_file() {
                return Some(saved.clone());
            }
        }
        find_first_video(&project_dir.join("dynamic"))
    }
}

/// Scans `dir` for the first supported video file sorted case-insensitively by filename.
pub fn find_first_video(dir: &Path) -> Option<PathBuf> {
    list_videos_sorted(dir).into_iter().next()
}

/// Best-effort async update of `~/.cache/bgpicker/blur/*` and `~/.cache/current_swaybg_img`
/// so external compositor tools (e.g. lockscreen) keep finding the blurred variant.
fn spawn_compat_blur_update(paths: &BgPickerPaths, original: &Path) {
    let blur_dir = paths.blur_cache_dir();
    let legacy_file = paths.legacy_swaybg_cache_file();
    let original = original.to_path_buf();
    let _ = thread::Builder::new()
        .name("bgpicker-compat-blur".into())
        .spawn(move || {
            let _ = update_compat_blur(&blur_dir, &legacy_file, &original);
        });
}

struct SplashBinTarget {
    bin_path: PathBuf,
    dst_size: (u32, u32),
    frame: Option<FrameInput>,
}

fn splash_matches_size(path: &Path, dst_w: u32, dst_h: u32) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    matches!(read_splash_header(&mut file), Ok((w, h)) if w == dst_w && h == dst_h)
}

fn write_splash_from_frame(bin_path: &Path, frame: &FrameInput, dst_size: (u32, u32)) {
    let src_is_bgra = match frame.format {
        PixelFormat::Rgba8 => false,
        PixelFormat::Bgra8 => true,
        PixelFormat::Nv12 { .. } => return,
    };
    if let Err(err) = write_splash_bin_from_packed(
        bin_path,
        frame.width,
        frame.height,
        frame.stride_bytes,
        &frame.data,
        src_is_bgra,
        dst_size.0,
        dst_size.1,
    ) {
        log::warn!("BgPicker: failed writing {}: {err}", bin_path.display());
    }
}

/// Best-effort async generation of high-res startup placeholder JPEG under
/// `~/.cache/bgpicker/startup-placeholder/*` plus pre-rendered `BGPX` bitmap
/// (`startup-splash-fg.bin`) for zero-decode `bgpicker-splash` boot.
fn spawn_placeholder_cache_update(
    paths: &BgPickerPaths,
    media: &Path,
    is_video: bool,
    splash_target: Option<SplashBinTarget>,
) {
    if cached_startup_placeholder_path(media, &paths.cache_dir, is_video).is_some()
        && splash_target.is_none()
    {
        return;
    }
    let cache_dir = paths.cache_dir.clone();
    let media = media.to_path_buf();
    let _ = thread::Builder::new()
        .name("bgpicker-ph-cache".into())
        .spawn(move || {
            let prepared_jpg = prepare_startup_placeholder(&media, &cache_dir, is_video).ok();
            let Some(splash) = splash_target else {
                return;
            };
            if let Some(ref frame) = splash.frame {
                write_splash_from_frame(&splash.bin_path, frame, splash.dst_size);
                return;
            }
            let src_img = if is_video {
                prepared_jpg.or_else(|| cached_startup_placeholder_path(&media, &cache_dir, true))
            } else {
                Some(media)
            };
            if let Some(path) = src_img {
                if let Ok(decoded) = FrameInput::from_image_path(&path, 8192) {
                    write_splash_from_frame(&splash.bin_path, &decoded, splash.dst_size);
                }
            }
        });
}

fn update_compat_blur(
    blur_dir: &Path,
    legacy_file: &Path,
    original: &Path,
) -> std::io::Result<()> {
    let blur_size = std::env::var("BGPICKER_BLUR_SIZE").unwrap_or_else(|_| "1920x1080".into());
    let blur_sigma = std::env::var("BGPICKER_BLUR_SIGMA").unwrap_or_else(|_| "20".into());
    fs::create_dir_all(blur_dir)?;

    let meta = fs::metadata(original)?;
    let meta_info = format!(
        "{}|{}|{}|{}|{}",
        original.display(),
        meta.ino(),
        meta.size(),
        meta.mtime(),
        meta.mtime_nsec()
    );
    let meta_key = sha256_hex(&format!("blur-meta-v1\0{meta_info}\0{blur_size}\0{blur_sigma}"));
    let meta_ref = blur_dir.join(format!("{meta_key}.ref"));
    if let Ok(existing) = fs::read_to_string(&meta_ref) {
        let p = existing.trim();
        if !p.is_empty() && fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false) {
            let _ = atomic_write_text(legacy_file, p);
            return Ok(());
        }
    }

    let bytes = fs::read(original)?;
    let source_hash = sha256_bytes_hex(&bytes);
    let cache_key = sha256_hex(&format!("blur-v1\0{source_hash}\0{blur_size}\0{blur_sigma}"));
    let blurred = blur_dir.join(format!("{cache_key}.png"));

    if !fs::metadata(&blurred)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
    {
        let tmp = blur_dir.join(format!(".blur-{}.png", std::process::id()));
        let mut source_frame = original.as_os_str().to_os_string();
        source_frame.push("[0]");
        let out = Command::new("magick")
            .args(["-limit", "thread", "2"])
            .arg(&source_frame)
            .args([
                "-auto-orient",
                "-thumbnail",
                &format!("{blur_size}>"),
                "-blur",
                &format!("0x{blur_sigma}"),
                "-strip",
            ])
            .arg(&tmp)
            .output();
        match out {
            Ok(res)
                if res.status.success()
                    && fs::metadata(&tmp).map(|m| m.len() > 0).unwrap_or(false) =>
            {
                fs::rename(&tmp, &blurred)?;
            }
            _ => {
                let _ = fs::remove_file(&tmp);
                return Ok(());
            }
        }
    }

    let blurred_str = blurred.to_string_lossy().into_owned();
    let tmp_ref = blur_dir.join(format!(".meta-{}.ref", std::process::id()));
    if let Ok(mut f) = fs::File::create(&tmp_ref) {
        let _ = writeln!(f, "{blurred_str}");
        let _ = f.sync_all();
        let _ = fs::rename(&tmp_ref, &meta_ref);
    }
    let _ = atomic_write_text(legacy_file, &blurred_str);
    Ok(())
}

fn sha256_hex(input: &str) -> String {
    sha256_bytes_hex(input.as_bytes())
}

fn sha256_bytes_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
