//! Wayland `zwlr_layer_shell_v1` overlay runtime, EGL/GLES rendering, HiDPI fractional scaling,
//! keyboard/pointer input handling, signal wakeup, and off-thread `bgpicker-daemon` IPC bridge.

use std::num::NonZeroU32;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bgpicker_core::protocol::BackdropOverride;
use bgpicker_core::{
    BgPickerConfig, BgPickerPaths, CatalogBundle, DaemonStatus, EngineKind,
    IpcClient, ModeAction, PersistedState, PickerCommand, Request, Response, ResponseData,
    TransitionType, VideoMode, VideoModeAction, WallpaperMode,
};
use bgpicker_gfx::{EglContext, compute_scaled_size};
use calloop::channel::{self, Sender as CalloopSender};
use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, RegistrationToken};
use khronos_egl as egl;
use log::{debug, info, warn};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, FrameCallbackData};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers as SctkModifiers, RawModifiers,
};
use smithay_client_toolkit::seat::pointer::{
    AxisScroll, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, PointerEvent, PointerEventKind, PointerHandler,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};
use thiserror::Error;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_keyboard::WlKeyboard;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_egl::WlEglSurface;
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{self, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::ipc::{
    PickerCliOptions, PickerIpcError, PickerIpcListener, PickerStartAction, SocketGuard,
    default_picker_response,
};
use crate::render::{PickerError, PickerRenderer};
use crate::state::{
    DaemonStateSnapshot, KeyCode, KeyInput, Modifiers, MouseButton, Page, PickerActionOut,
    PickerConfig, PickerInput, PickerUi,
};
use crate::text::TextAtlas;
use crate::thumbs::{ThumbnailCache, ThumbnailCacheConfig};

const OVERLAY_NAMESPACE: &str = "bgpicker:overlay";
const FRAME_CALLBACK_TIMEOUT: Duration = Duration::from_millis(100);
const THUMB_POLL_INTERVAL: Duration = Duration::from_millis(16);
const STARTUP_STATUS_TIMEOUT: Duration = Duration::from_millis(800);
const DAEMON_MUTATION_TIMEOUT: Duration = Duration::from_secs(30);
const DAEMON_CATALOG_TIMEOUT: Duration = Duration::from_secs(120);

/// Errors returned by the Wayland overlay runtime.
#[derive(Debug, Error)]
pub enum WaylandOverlayError {
    #[error("failed to connect to Wayland compositor: {0}")]
    WaylandConnect(String),
    #[error("missing required Wayland global: {0}")]
    WaylandGlobal(String),
    #[error("Wayland/calloop dispatch error: {0}")]
    Dispatch(String),
    #[error("EGL/GL error: {0}")]
    Gfx(#[from] bgpicker_gfx::GfxError),
    #[error("picker renderer error: {0}")]
    Renderer(#[from] PickerError),
    #[error("picker IPC error: {0}")]
    Ipc(#[from] PickerIpcError),
}

// ── Signal self-pipe for instant SIGINT/SIGTERM wakeup with zero extra threads ─

static SIGNAL_PIPE_WR: AtomicI32 = AtomicI32::new(-1);

extern "C" fn handle_termination_signal(_sig: libc::c_int) {
    let fd = SIGNAL_PIPE_WR.load(Ordering::Acquire);
    if fd >= 0 {
        let byte: u8 = 1;
        unsafe {
            let _ = libc::write(fd, (&raw const byte).cast(), 1);
        }
    }
}

struct SignalWakeup {
    read_fd: Option<OwnedFd>,
    write_fd: Option<OwnedFd>,
}

impl SignalWakeup {
    fn install() -> std::io::Result<Self> {
        let mut fds = [-1_i32; 2];
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        SIGNAL_PIPE_WR.store(fds[1], Ordering::Release);

        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handle_termination_signal as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = libc::SA_RESTART;
            libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        }

        Ok(Self {
            read_fd: Some(read_fd),
            write_fd: Some(write_fd),
        })
    }

    fn take_read_fd(&mut self) -> Option<OwnedFd> {
        self.read_fd.take()
    }
}

impl Drop for SignalWakeup {
    fn drop(&mut self) {
        SIGNAL_PIPE_WR.store(-1, Ordering::Release);
        self.write_fd = None;
        self.read_fd = None;
    }
}

// ── Wayland axis & key mapping helpers (pub for shell_tests) ───────────────────

/// Maps a Wayland Linux evdev button code into [`MouseButton`].
pub fn map_pointer_button(button: u32) -> Option<MouseButton> {
    match button {
        BTN_LEFT => Some(MouseButton::Left),
        BTN_RIGHT => Some(MouseButton::Right),
        BTN_MIDDLE => Some(MouseButton::Middle),
        _ => None,
    }
}

/// Converts a Wayland `wl_pointer` [`AxisScroll`] into Qt's `angleDelta` sign convention
/// expected by [`PickerInput::PointerWheel`].
///
/// In Wayland, positive vertical scroll means scrolling down; in Qt/`Carousel.qml`,
/// negative `dy` scrolls down toward [`Page::Video`] and positive `dy` scrolls up toward
/// [`Page::Static`].
pub fn wayland_axis_to_qt_delta(axis: &AxisScroll) -> f32 {
    if axis.value120 != 0 {
        -(axis.value120 as f32)
    } else if axis.discrete != 0 {
        -(axis.discrete as f32 * 120.0)
    } else {
        -(axis.absolute as f32)
    }
}

/// Translates an SCTK `xkbcommon` [`KeyEvent`] + [`Modifiers`] into a [`PickerInput`].
///
/// Handles `Ctrl+F` search activation, printable UTF-8 text entry when `search_visible`
/// is true (avoiding double entry), navigation keys, `Home`/`End`, `Backspace`, `Tab`,
/// and `PageUp`/`PageDown` page switching.
pub fn map_key_event_to_input(
    event: &KeyEvent,
    modifiers: Modifiers,
    search_visible: bool,
) -> Option<PickerInput> {
    let sym = event.keysym;

    // Ctrl+F always opens/focuses search even when Ctrl turns utf8 into "\x06".
    if modifiers.ctrl && (sym == Keysym::f || sym == Keysym::F) {
        return Some(PickerInput::KeyInput(KeyInput {
            key: KeyCode::Char('f'),
            modifiers,
        }));
    }

    // Special / control keys first (same in both normal and search modes).
    let special = match sym {
        Keysym::Escape => Some(KeyCode::Escape),
        Keysym::Return | Keysym::KP_Enter | Keysym::ISO_Enter => Some(KeyCode::Enter),
        Keysym::Left | Keysym::KP_Left => Some(KeyCode::Left),
        Keysym::Right | Keysym::KP_Right => Some(KeyCode::Right),
        Keysym::Up | Keysym::KP_Up => Some(KeyCode::Up),
        Keysym::Down | Keysym::KP_Down => Some(KeyCode::Down),
        Keysym::Home | Keysym::KP_Home => Some(KeyCode::Home),
        Keysym::End | Keysym::KP_End => Some(KeyCode::End),
        Keysym::BackSpace => Some(KeyCode::Backspace),
        Keysym::Tab | Keysym::ISO_Left_Tab => Some(KeyCode::Tab),
        _ => None,
    };
    if let Some(code) = special {
        return Some(PickerInput::KeyInput(KeyInput {
            key: code,
            modifiers,
        }));
    }

    if !search_visible {
        match sym {
            Keysym::Page_Up | Keysym::KP_Page_Up => {
                return Some(PickerInput::ShowPage(Page::Static));
            }
            Keysym::Page_Down | Keysym::KP_Page_Down => {
                return Some(PickerInput::ShowPage(Page::Video));
            }
            _ => {}
        }
    }

    if search_visible && !modifiers.ctrl && !modifiers.alt {
        if let Some(ref utf8) = event.utf8 {
            let printable: String = utf8.chars().filter(|c| !c.is_control()).collect();
            if !printable.is_empty() {
                return Some(PickerInput::TextInput(printable));
            }
        }
        if let Some(ch) = sym.key_char()
            && !ch.is_control()
        {
            return Some(PickerInput::TextInput(ch.to_string()));
        }
        return None;
    }

    // Non-search character bindings (e.g. Vim h/H/l/L navigation).
    if let Some(ch) = sym.key_char()
        && !ch.is_control()
    {
        return Some(PickerInput::KeyInput(KeyInput {
            key: KeyCode::Char(ch),
            modifiers,
        }));
    }

    None
}

// ── Off-thread daemon IPC bridge ───────────────────────────────────────────────

/// Daemon IPC mutation job queued onto the off-thread worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonMutationJob {
    ApplyStatic {
        path: PathBuf,
        mode: Option<WallpaperMode>,
    },
    ApplyVideo {
        mode: VideoModeAction,
        path: PathBuf,
    },
    ToggleStaticMode,
    SelectTransition(TransitionType),
}

/// Resolves which [`PickerInput`] to dispatch for an incoming [`PickerCommand`]
/// given the current visibility and closing state of [`PickerUi`].
pub fn resolve_picker_ipc_input(
    cmd: PickerCommand,
    is_visible: bool,
    is_closing: bool,
) -> Option<PickerInput> {
    match cmd {
        PickerCommand::Ping => None,
        PickerCommand::Open => (!is_visible || is_closing).then_some(PickerInput::StartOpen),
        PickerCommand::Toggle => {
            if is_closing {
                Some(PickerInput::StartOpen)
            } else if is_visible {
                Some(PickerInput::StartClose)
            } else {
                Some(PickerInput::StartOpen)
            }
        }
        PickerCommand::Close => Some(PickerInput::StartClose),
        PickerCommand::CycleNext => Some(PickerInput::CycleNext),
        PickerCommand::CyclePrevious => Some(PickerInput::CyclePrevious),
    }
}

/// Resolves which daemon mutation to dispatch when the user toggles the
/// `普通` / `视差` mode selector on `page` given the current daemon snapshot.
pub fn resolve_mode_toggle_job(
    page: Page,
    snap: &DaemonStateSnapshot,
) -> Option<DaemonMutationJob> {
    match page {
        Page::Video if !snap.current_video.is_empty() => {
            let next_mode = match snap.video_mode {
                VideoMode::Normal => VideoModeAction::Parallax,
                VideoMode::Parallax => VideoModeAction::Normal,
            };
            Some(DaemonMutationJob::ApplyVideo {
                mode: next_mode,
                path: PathBuf::from(&snap.current_video),
            })
        }
        Page::Static if !snap.current_wallpaper.is_empty() => {
            if snap.engine != EngineKind::Full {
                Some(DaemonMutationJob::ApplyStatic {
                    path: PathBuf::from(&snap.current_wallpaper),
                    mode: Some(snap.wallpaper_mode.toggled()),
                })
            } else {
                Some(DaemonMutationJob::ToggleStaticMode)
            }
        }
        _ => None,
    }
}

#[derive(Debug)]
enum DaemonEvent {
    CatalogLoaded(Result<CatalogBundle, String>),
    MutationFinished {
        status: Option<DaemonStatus>,
        error: Option<String>,
    },
}

struct DaemonBridge {
    client: IpcClient,
    project_dir: PathBuf,
    event_tx: CalloopSender<DaemonEvent>,
    mutation_tx: Option<std_mpsc::Sender<DaemonMutationJob>>,
    mutation_worker: Option<JoinHandle<()>>,
    catalog_in_flight: bool,
    pending_mutations: usize,
}

impl DaemonBridge {
    fn new(
        daemon_socket: PathBuf,
        project_dir: PathBuf,
        event_tx: CalloopSender<DaemonEvent>,
    ) -> Self {
        let client = IpcClient::new(daemon_socket).with_timeout(DAEMON_MUTATION_TIMEOUT);
        let (mutation_tx, mutation_rx) = std_mpsc::channel::<DaemonMutationJob>();

        let worker_client = client.clone();
        let worker_tx = event_tx.clone();
        let mutation_worker = thread::Builder::new()
            .name("bgpicker-picker-ipc".into())
            .spawn(move || {
                while let Ok(job) = mutation_rx.recv() {
                    let req = match job {
                        DaemonMutationJob::ApplyStatic { path, mode } => Request::ApplyStatic {
                            path,
                            mode,
                            instant: false,
                            backdrop: BackdropOverride::Auto,
                        },
                        DaemonMutationJob::ApplyVideo { mode, path } => Request::ApplyVideo {
                            mode: Some(mode),
                            path: Some(path),
                            backdrop: BackdropOverride::Auto,
                        },
                        DaemonMutationJob::ToggleStaticMode => Request::SetMode {
                            mode: ModeAction::Toggle,
                            backdrop: BackdropOverride::Auto,
                        },
                        DaemonMutationJob::SelectTransition(transition_type) => {
                            Request::SetTransition { transition_type }
                        }
                    };

                    let error = match worker_client.call(&req) {
                        Ok(resp) if resp.ok => None,
                        Ok(resp) => Some(
                            resp.error
                                .unwrap_or_else(|| "壁纸切换失败，请查看后台服务日志。".into()),
                        ),
                        Err(err) => Some(format!("后台服务通信失败: {err}")),
                    };

                    let status = if error.is_none() {
                        fetch_daemon_status_quick(&worker_client)
                    } else {
                        None
                    };

                    if worker_tx
                        .send(DaemonEvent::MutationFinished { status, error })
                        .is_err()
                    {
                        // Event loop already closed (e.g. Apply + Exit); keep draining remaining
                        // queued mutations before exiting the worker thread.
                    }
                }
            })
            .ok();

        Self {
            client,
            project_dir,
            event_tx,
            mutation_tx: Some(mutation_tx),
            mutation_worker,
            catalog_in_flight: false,
            pending_mutations: 0,
        }
    }

    fn request_catalog(&mut self) {
        if self.catalog_in_flight {
            return;
        }
        self.catalog_in_flight = true;
        let client = self.client.clone().with_timeout(DAEMON_CATALOG_TIMEOUT);
        let image_dir = self.project_dir.join("wallpapers");
        let video_dir = self.project_dir.join("dynamic");
        let tx = self.event_tx.clone();

        let _ = thread::Builder::new()
            .name("bgpicker-picker-catalog".into())
            .spawn(move || {
                let res = match client.call(&Request::Catalog {
                    image_dir: Some(image_dir),
                    video_dir: Some(video_dir),
                }) {
                    Ok(resp) if resp.ok => match resp.data {
                        Some(ResponseData::Catalog(bundle)) => Ok(bundle),
                        _ => Err("媒体目录返回格式异常。".into()),
                    },
                    Ok(resp) => Err(resp.error.unwrap_or_else(|| "媒体目录读取失败。".into())),
                    Err(err) => Err(format!("媒体目录读取失败: {err}")),
                };
                let _ = tx.send(DaemonEvent::CatalogLoaded(res));
            });
    }

    fn enqueue_mutation(&mut self, job: DaemonMutationJob) {
        if let Some(ref tx) = self.mutation_tx
            && tx.send(job).is_ok()
        {
            self.pending_mutations += 1;
        }
    }

    /// Drops the mutation sender and joins the mutation worker thread so any in-flight
    /// `ApplyStatic` / `ApplyVideo` IPC request finishes cleanly before process exit.
    fn finish_pending_mutations(&mut self) {
        self.mutation_tx.take();
        if let Some(handle) = self.mutation_worker.take() {
            let _ = handle.join();
        }
    }
}

fn fetch_daemon_status_quick(client: &IpcClient) -> Option<DaemonStatus> {
    let quick = client.clone().with_timeout(STARTUP_STATUS_TIMEOUT);
    match quick.call(&Request::Status) {
        Ok(resp) if resp.ok => match resp.data {
            Some(ResponseData::Status(st)) => Some(*st),
            _ => None,
        },
        _ => None,
    }
}

fn status_to_snapshot(st: &DaemonStatus, local_busy: bool) -> DaemonStateSnapshot {
    DaemonStateSnapshot {
        engine: st.engine,
        wallpaper_mode: st.mode,
        video_mode: st.video_mode,
        transition_type: st.transition.transition_type,
        current_wallpaper: st
            .current_wallpaper
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        current_video: st
            .current_video
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        busy: st.busy || local_busy,
        error: st.error.clone().unwrap_or_default(),
    }
}

fn fallback_snapshot_from_disk(paths: &BgPickerPaths) -> DaemonStateSnapshot {
    let persisted = PersistedState::load(paths);
    let cfg = BgPickerConfig::load(&paths.settings_file()).unwrap_or_default();
    DaemonStateSnapshot {
        engine: persisted.engine,
        wallpaper_mode: persisted.mode,
        video_mode: persisted.video_mode,
        transition_type: cfg.transition.transition_type,
        current_wallpaper: persisted
            .current_wallpaper
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        current_video: persisted
            .video_wallpaper
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        busy: false,
        error: String::new(),
    }
}

// ── Wayland Overlay Slot ───────────────────────────────────────────────────────

struct OverlaySlot {
    // Drop order: EGLSurface -> WlEglSurface -> add-on protocols -> LayerSurface (WlSurface).
    egl_surface: Option<egl::Surface>,
    wl_egl: Option<WlEglSurface>,
    fractional_scale: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    layer: LayerSurface,
    fallback_logical_size: (u32, u32),
    logical_width: u32,
    logical_height: u32,
    pixel_width: u32,
    pixel_height: u32,
    scale_120: u32,
    configured: bool,
    waiting_frame_since: Option<Instant>,
}

impl OverlaySlot {
    #[allow(clippy::too_many_arguments)]
    fn new(
        compositor: &CompositorState,
        layer_shell: &LayerShell,
        qh: &QueueHandle<PickerWaylandApp>,
        output: Option<&WlOutput>,
        fallback_logical_size: (u32, u32),
        initial_integer_scale: i32,
        viewporter: Option<&WpViewporter>,
        scale_mgr: Option<&WpFractionalScaleManagerV1>,
    ) -> Self {
        let wl_surface = compositor.create_surface(qh);
        let viewport = viewporter.map(|vp| vp.get_viewport(&wl_surface, qh, ()));
        let fractional_scale =
            scale_mgr.map(|mgr| mgr.get_fractional_scale(&wl_surface, qh, wl_surface.clone()));

        if viewport.is_some() {
            wl_surface.set_buffer_scale(1);
        } else {
            wl_surface.set_buffer_scale(initial_integer_scale.max(1));
        }

        let layer = layer_shell.create_layer_surface(
            qh,
            wl_surface,
            Layer::Overlay,
            Some(OVERLAY_NAMESPACE),
            output,
        );
        layer.set_anchor(Anchor::all());
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.set_size(0, 0);
        layer.commit();

        let scale_120 = (initial_integer_scale.max(1) as u32) * 120;

        Self {
            egl_surface: None,
            wl_egl: None,
            fractional_scale,
            viewport,
            layer,
            fallback_logical_size,
            logical_width: 0,
            logical_height: 0,
            pixel_width: 0,
            pixel_height: 0,
            scale_120,
            configured: false,
            waiting_frame_since: None,
        }
    }

    #[inline]
    fn dpr(&self) -> f32 {
        (self.scale_120.max(30) as f32) / 120.0
    }

    fn is_waiting_for_frame(&mut self, now: Instant) -> bool {
        let Some(since) = self.waiting_frame_since else {
            return false;
        };
        if now.saturating_duration_since(since) >= FRAME_CALLBACK_TIMEOUT {
            self.waiting_frame_since = None;
            return false;
        }
        true
    }

    fn apply_geometry(&mut self, egl: &EglContext) -> Result<bool, bgpicker_gfx::GfxError> {
        if self.logical_width == 0 || self.logical_height == 0 {
            return Ok(false);
        }
        let (pw, ph) = compute_scaled_size(self.logical_width, self.logical_height, self.scale_120);

        if let Some(ref vp) = self.viewport {
            self.layer.wl_surface().set_buffer_scale(1);
            vp.set_destination(self.logical_width as i32, self.logical_height as i32);
        } else {
            let int_scale = (self.scale_120 / 120).max(1) as i32;
            self.layer.wl_surface().set_buffer_scale(int_scale);
        }

        let mut resized = false;
        if let Some(ref wl_egl) = self.wl_egl {
            if self.pixel_width != pw || self.pixel_height != ph {
                wl_egl.resize(pw as i32, ph as i32, 0, 0);
                self.pixel_width = pw;
                self.pixel_height = ph;
                resized = true;
            }
        } else {
            let wl_egl =
                WlEglSurface::new(self.layer.wl_surface().id(), pw as i32, ph as i32).map_err(
                    |e| bgpicker_gfx::GfxError::EglSurface(format!("WlEglSurface::new failed: {e}")),
                )?;
            let egl_surf = unsafe { egl.create_window_surface(wl_egl.ptr() as *mut _)? };
            self.wl_egl = Some(wl_egl);
            self.egl_surface = Some(egl_surf);
            self.pixel_width = pw;
            self.pixel_height = ph;
            resized = true;
        }

        Ok(resized)
    }

    fn destroy(&mut self, egl: &EglContext) {
        if let Some(surf) = self.egl_surface.take() {
            egl.destroy_surface(surf);
        }
        self.wl_egl = None;
        if let Some(fs) = self.fractional_scale.take() {
            fs.destroy();
        }
        if let Some(vp) = self.viewport.take() {
            vp.destroy();
        }
    }
}

// ── Wayland App State ──────────────────────────────────────────────────────────

struct PickerWaylandApp {
    registry_state: RegistryState,
    compositor_state: CompositorState,
    layer_shell: LayerShell,
    output_state: OutputState,
    seat_state: SeatState,
    viewporter: Option<WpViewporter>,
    fractional_scale_mgr: Option<WpFractionalScaleManagerV1>,
    cursor_shape_mgr: Option<WpCursorShapeManagerV1>,

    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    cursor_shape_device: Option<WpCursorShapeDeviceV1>,
    pointer_enter_serial: Option<u32>,
    last_cursor_shape: Option<wp_cursor_shape_device_v1::Shape>,
    modifiers: Modifiers,

    overlay: Option<OverlaySlot>,
    ui: PickerUi,
    thumbs: ThumbnailCache,
    text: TextAtlas,
    renderer: PickerRenderer,
    // Keep EGL before Connection so wl_display outlives EGLContext.
    egl: EglContext,
    conn: Connection,

    daemon: DaemonBridge,
    daemon_snapshot: DaemonStateSnapshot,
    socket_guard: Option<SocketGuard>,

    qh: QueueHandle<Self>,
    loop_handle: LoopHandle<'static, Self>,
    loop_signal: LoopSignal,
    pacing_timer: Option<RegistrationToken>,
    last_tick: Instant,
    force_redraw: bool,
    exiting: bool,
}

impl PickerWaylandApp {
    fn teardown_gpu_and_surface(&mut self) {
        if let Some(tok) = self.pacing_timer.take() {
            self.loop_handle.remove(tok);
        }
        let _ = self.egl.make_fallback_current();
        self.thumbs.clear_all_gpu(&self.egl.gl);
        self.text.destroy_gpu(&self.egl.gl);
        self.renderer.destroy();

        if let Some(mut slot) = self.overlay.take() {
            slot.destroy(&self.egl);
        }
        if let Some(dev) = self.cursor_shape_device.take() {
            dev.destroy();
        }
        if let Some(ptr) = self.pointer.take()
            && ptr.version() >= 3
        {
            ptr.release();
        }
        if let Some(kbd) = self.keyboard.take()
            && kbd.version() >= 3
        {
            kbd.release();
        }
        let _ = self.conn.flush();
        if let Some(mut guard) = self.socket_guard.take() {
            guard.cleanup_now();
        }
    }

    fn begin_exit(&mut self) {
        if self.exiting {
            return;
        }
        self.exiting = true;
        self.teardown_gpu_and_surface();
        self.loop_signal.stop();
    }

    fn sync_ui_viewport(&mut self) {
        if let Some(ref slot) = self.overlay
            && slot.logical_width > 0
            && slot.logical_height > 0
        {
            let _ = self.ui.handle_input(PickerInput::SetViewport {
                width: slot.logical_width as f32,
                height: slot.logical_height as f32,
                scale: slot.dpr(),
            });
            self.force_redraw = true;
        }
    }

    fn dispatch_ui_actions(&mut self, actions: Vec<PickerActionOut>) {
        for action in actions {
            match action {
                PickerActionOut::RequestCatalog => {
                    self.daemon.request_catalog();
                }
                PickerActionOut::ApplyStatic(path) => {
                    info!("bgpicker-picker: applying static wallpaper {path}");
                    self.daemon.enqueue_mutation(DaemonMutationJob::ApplyStatic {
                        path: PathBuf::from(path),
                        mode: None,
                    });
                }
                PickerActionOut::ApplyVideo(mode, path) => {
                    info!("bgpicker-picker: applying video wallpaper ({}) {path}", mode.as_str());
                    let mode_action = match mode {
                        VideoMode::Normal => VideoModeAction::Normal,
                        VideoMode::Parallax => VideoModeAction::Parallax,
                    };
                    self.daemon.enqueue_mutation(DaemonMutationJob::ApplyVideo {
                        mode: mode_action,
                        path: PathBuf::from(path),
                    });
                }
                PickerActionOut::ToggleMode => {
                    let page = self.ui.page();
                    if let Some(job) = resolve_mode_toggle_job(page, &self.daemon_snapshot) {
                        self.daemon.enqueue_mutation(job);
                        self.set_local_busy();
                    }
                }
                PickerActionOut::SelectTransition(kind) => {
                    self.daemon_snapshot.transition_type = kind;
                    self.daemon
                        .enqueue_mutation(DaemonMutationJob::SelectTransition(kind));
                    self.set_local_busy();
                }
                PickerActionOut::Exit => {
                    self.begin_exit();
                }
            }
        }
    }

    fn set_local_busy(&mut self) {
        self.daemon_snapshot.busy = true;
        self.daemon_snapshot.error.clear();
        let _ = self
            .ui
            .handle_input(PickerInput::SetDaemonState(self.daemon_snapshot.clone()));
        self.force_redraw = true;
    }

    fn handle_picker_ipc(&mut self, cmd: PickerCommand) -> Response {
        let resp = default_picker_response(cmd);
        if self.exiting {
            return resp;
        }
        if cmd == PickerCommand::Ping {
            return resp;
        }
        self.sync_ui_clock_before_input(Instant::now());
        if self.exiting {
            return resp;
        }
        if let Some(input) =
            resolve_picker_ipc_input(cmd, self.ui.is_visible(), self.ui.is_closing())
        {
            let actions = self.ui.handle_input(input);
            self.dispatch_ui_actions(actions);
        }
        self.render_and_schedule();
        resp
    }

    fn handle_daemon_event(&mut self, event: DaemonEvent) {
        if self.exiting {
            return;
        }
        match event {
            DaemonEvent::CatalogLoaded(Ok(bundle)) => {
                self.daemon.catalog_in_flight = false;
                if !self.daemon_snapshot.error.is_empty() {
                    self.daemon_snapshot.error.clear();
                    let _ = self
                        .ui
                        .handle_input(PickerInput::SetDaemonState(self.daemon_snapshot.clone()));
                }
                let actions = self.ui.handle_input(PickerInput::SetCatalog {
                    images: bundle.images,
                    videos: bundle.videos,
                });
                self.dispatch_ui_actions(actions);
                self.force_redraw = true;
            }
            DaemonEvent::CatalogLoaded(Err(err)) => {
                self.daemon.catalog_in_flight = false;
                warn!("bgpicker-picker: catalog load error: {err}");
                self.daemon_snapshot.error = err;
                let _ = self
                    .ui
                    .handle_input(PickerInput::SetDaemonState(self.daemon_snapshot.clone()));
                self.force_redraw = true;
            }
            DaemonEvent::MutationFinished { status, error } => {
                self.daemon.pending_mutations = self.daemon.pending_mutations.saturating_sub(1);
                let local_busy = self.daemon.pending_mutations > 0;
                if let Some(st) = status {
                    self.daemon_snapshot = status_to_snapshot(&st, local_busy);
                } else {
                    self.daemon_snapshot.busy = local_busy;
                }
                if let Some(err) = error {
                    warn!("bgpicker-picker: daemon action error: {err}");
                    self.daemon_snapshot.error = err;
                }
                let _ = self
                    .ui
                    .handle_input(PickerInput::SetDaemonState(self.daemon_snapshot.clone()));
                self.force_redraw = true;
            }
        }
        self.render_and_schedule();
    }

    fn sync_ui_clock_before_input(&mut self, now: Instant) {
        if self.advance_ui_time(now) {
            self.force_redraw = true;
        }
    }

    fn handle_key_press_or_repeat(&mut self, event: KeyEvent) {
        if self.exiting {
            return;
        }
        self.sync_ui_clock_before_input(Instant::now());
        if self.exiting {
            return;
        }
        let Some(input) = map_key_event_to_input(&event, self.modifiers, self.ui.search_visible())
        else {
            return;
        };
        let actions = self.ui.handle_input(input);
        self.dispatch_ui_actions(actions);
        self.render_and_schedule();
    }

    fn update_cursor_shape(&mut self, x: f32, y: f32) {
        let Some(ref device) = self.cursor_shape_device else {
            return;
        };
        let Some(serial) = self.pointer_enter_serial else {
            return;
        };
        let shape = if self.ui.is_dragging() {
            wp_cursor_shape_device_v1::Shape::Grabbing
        } else if self.ui.hit_test_search_box(x, y) {
            wp_cursor_shape_device_v1::Shape::Text
        } else if self.ui.hit_test_mode_button(x, y).is_some()
            || self.ui.hit_test_transition_button(x, y).is_some()
            || self.ui.hit_test_card(x, y).is_some()
        {
            wp_cursor_shape_device_v1::Shape::Pointer
        } else {
            wp_cursor_shape_device_v1::Shape::Default
        };
        if self.last_cursor_shape != Some(shape) {
            device.set_shape(serial, shape);
            self.last_cursor_shape = Some(shape);
        }
    }

    /// Advances `PickerUi` across `dt_s` in `<= 0.25s` chunks (matching `PickerUi::tick`'s clamp)
    /// while stopping immediately once an animation wakes up so Frame 0 starts smoothly.
    fn advance_ui_time(&mut self, now: Instant) -> bool {
        let mut remaining = now
            .saturating_duration_since(self.last_tick)
            .as_secs_f32()
            .clamp(0.0, 5.0);
        self.last_tick = now;

        let mut actions = Vec::new();
        let mut changed = false;
        if remaining <= 1e-6 {
            changed = self.ui.tick(0.0, &mut actions);
        } else {
            while remaining > 1e-6 {
                let step = remaining.min(0.25);
                remaining -= step;
                if self.ui.tick(step, &mut actions) {
                    changed = true;
                }
                if actions.iter().any(|a| matches!(a, PickerActionOut::Exit)) {
                    break;
                }
                if self.ui.is_animating() {
                    break;
                }
            }
        }
        if !actions.is_empty() {
            self.dispatch_ui_actions(actions);
        }
        changed
    }

    fn on_pacing_timer(&mut self) {
        if self.exiting {
            return;
        }
        let now = Instant::now();
        if let Some(ref mut slot) = self.overlay
            && slot.waiting_frame_since.is_some()
            && !slot.is_waiting_for_frame(now)
        {
            // Frame callback timed out (e.g. DPMS/occluded) — allow rendering to proceed.
            self.force_redraw = true;
        }

        // Poll ready thumbnail / hi-res uploads without redrawing if nothing decoded yet.
        if self.thumbs.has_pending_work()
            && self.egl.make_fallback_current().is_ok()
            && self.thumbs.upload_ready(&self.egl.gl, 4)
        {
            self.ui.set_hires_ready(self.thumbs.hires_loaded_path());
            self.force_redraw = true;
        }

        let configured = self.overlay.as_ref().is_some_and(|s| s.configured);
        if (configured || self.ui.is_closing()) && self.advance_ui_time(now) {
            self.force_redraw = true;
        }
        self.render_and_schedule();
    }

    fn render_and_schedule(&mut self) {
        if self.exiting {
            return;
        }
        self.render_if_needed();
        if !self.exiting {
            self.rearm_pacing_timer();
        }
    }

    fn render_if_needed(&mut self) {
        if self.exiting {
            return;
        }
        let now = Instant::now();
        let Some(ref mut slot) = self.overlay else {
            return;
        };
        if !slot.configured {
            return;
        }
        if slot.is_waiting_for_frame(now) {
            return;
        }

        // Advance animations up to `now` right before rendering.
        if self.ui.is_animating() {
            let _ = self.advance_ui_time(now);
            if self.exiting {
                return;
            }
        } else {
            // Keep clock fresh when idle transitions to a redraw.
            if self.ui.next_timer_delay_s().is_none() {
                self.last_tick = now;
            }
        }

        if !self.force_redraw && !self.ui.needs_redraw() {
            return;
        }

        let Some(ref mut slot) = self.overlay else {
            return;
        };
        let Some(egl_surface) = slot.egl_surface else {
            return;
        };
        if self.egl.make_current(Some(egl_surface)).is_err() {
            return;
        }

        let pw = slot.pixel_width;
        let ph = slot.pixel_height;
        let dpr = slot.dpr();

        self.renderer
            .render(&mut self.ui, &mut self.thumbs, &mut self.text, (pw, ph), dpr);
        self.ui.clear_dirty();
        self.force_redraw = false;

        if self.ui.is_animating() {
            let wl_surface = slot.layer.wl_surface();
            wl_surface.frame(&self.qh, FrameCallbackData(wl_surface.clone()));
            slot.waiting_frame_since = Some(Instant::now());
        }

        slot.layer
            .wl_surface()
            .damage_buffer(0, 0, pw as i32, ph as i32);
        if let Err(err) = self.egl.swap_buffers(egl_surface) {
            warn!("bgpicker-picker: eglSwapBuffers error: {err}");
        }
    }

    fn rearm_pacing_timer(&mut self) {
        if let Some(tok) = self.pacing_timer.take() {
            self.loop_handle.remove(tok);
        }
        if self.exiting {
            return;
        }

        let mut next_delay: Option<Duration> = None;
        let mut consider = |d: Duration| {
            next_delay = Some(match next_delay {
                Some(prev) => prev.min(d),
                None => d,
            });
        };

        let waiting_frame = self
            .overlay
            .as_ref()
            .is_some_and(|s| s.waiting_frame_since.is_some());

        if self.ui.is_animating() {
            if waiting_frame {
                // Paced by wl_surface.frame; safety fallback only if compositor drops frame callback.
                consider(FRAME_CALLBACK_TIMEOUT);
            } else {
                consider(Duration::from_millis(4));
            }
        } else if let Some(delay_s) = self.ui.next_timer_delay_s() {
            let dur = Duration::from_secs_f32(delay_s.max(0.004));
            consider(dur);
        }

        if self.thumbs.has_pending_work() {
            consider(THUMB_POLL_INTERVAL);
        }

        if let Some(delay) = next_delay
            && let Ok(tok) = self
                .loop_handle
                .insert_source(Timer::from_duration(delay), |_, _, app| {
                    app.pacing_timer = None;
                    app.on_pacing_timer();
                    TimeoutAction::Drop
                })
        {
            self.pacing_timer = Some(tok);
        }
    }
}

impl Drop for PickerWaylandApp {
    fn drop(&mut self) {
        self.teardown_gpu_and_surface();
    }
}

// ── SCTK Trait Implementations ─────────────────────────────────────────────────

impl CompositorHandler for PickerWaylandApp {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &WlSurface,
        new_factor: i32,
    ) {
        if self.fractional_scale_mgr.is_some() {
            return;
        }
        let scale_120 = (new_factor.max(1) as u32) * 120;
        let mut changed = false;
        if let Some(ref mut slot) = self.overlay
            && slot.layer.wl_surface() == surface
            && slot.scale_120 != scale_120
        {
            slot.scale_120 = scale_120;
            if let Err(err) = slot.apply_geometry(&self.egl) {
                warn!("bgpicker-picker: integer scale resize failed: {err}");
            }
            changed = true;
        }
        if changed {
            self.sync_ui_viewport();
            self.render_and_schedule();
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &WlSurface,
        _time: u32,
    ) {
        if let Some(ref mut slot) = self.overlay
            && slot.layer.wl_surface() == surface
        {
            slot.waiting_frame_since = None;
        }
        self.render_and_schedule();
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }
}

impl OutputHandler for PickerWaylandApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: WlOutput) {}

    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: WlOutput) {}

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }
}

impl LayerShellHandler for PickerWaylandApp {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        debug!("bgpicker-picker: layer surface closed by compositor");
        self.begin_exit();
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let mut configured_now = false;
        let mut first_configure = false;
        if let Some(ref mut slot) = self.overlay
            && slot.layer == *layer
        {
            let was_configured = slot.configured;
            let lw = NonZeroU32::new(configure.new_size.0)
                .map_or(slot.fallback_logical_size.0, NonZeroU32::get);
            let lh = NonZeroU32::new(configure.new_size.1)
                .map_or(slot.fallback_logical_size.1, NonZeroU32::get);
            slot.logical_width = lw;
            slot.logical_height = lh;
            slot.configured = true;
            if let Err(err) = slot.apply_geometry(&self.egl) {
                warn!("bgpicker-picker: failed to configure Wayland EGL surface: {err}");
            }
            configured_now = true;
            first_configure = !was_configured;
        }
        if configured_now {
            if first_configure {
                self.last_tick = Instant::now();
            }
            self.sync_ui_viewport();
            self.render_and_schedule();
        }
    }
}

impl SeatHandler for PickerWaylandApp {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            match self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|app, _kbd, event| {
                    app.handle_key_press_or_repeat(event);
                }),
            ) {
                Ok(kbd) => self.keyboard = Some(kbd),
                Err(err) => warn!("bgpicker-picker: failed to acquire wl_keyboard: {err}"),
            }
        }

        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(ptr) => {
                    if let Some(ref mgr) = self.cursor_shape_mgr {
                        self.cursor_shape_device = Some(mgr.get_pointer(&ptr, qh, ()));
                    }
                    self.pointer = Some(ptr);
                }
                Err(err) => warn!("bgpicker-picker: failed to acquire wl_pointer: {err}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(kbd) = self.keyboard.take()
            && kbd.version() >= 3
        {
            kbd.release();
        }
        if capability == Capability::Pointer {
            if let Some(dev) = self.cursor_shape_device.take() {
                dev.destroy();
            }
            if let Some(ptr) = self.pointer.take()
                && ptr.version() >= 3
            {
                ptr.release();
            }
            self.pointer_enter_serial = None;
            self.last_cursor_shape = None;
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}
}

impl KeyboardHandler for PickerWaylandApp {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _surface: &WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _surface: &WlSurface,
        _serial: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.handle_key_press_or_repeat(event);
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
        // Key repeat is driven by SCTK's `get_keyboard_with_repeat` timer to avoid double-repeats.
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _serial: u32,
        modifiers: SctkModifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        self.modifiers = Modifiers {
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            alt: modifiers.alt,
        };
    }
}

impl PointerHandler for PickerWaylandApp {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        if self.exiting {
            return;
        }
        let Some(overlay_surface) = self.overlay.as_ref().map(|s| s.layer.wl_surface().clone())
        else {
            return;
        };

        let mut handled_any = false;
        let mut synced_clock = false;
        for event in events {
            if event.surface != overlay_surface {
                continue;
            }
            if !synced_clock {
                self.sync_ui_clock_before_input(Instant::now());
                synced_clock = true;
                if self.exiting {
                    return;
                }
            }
            let x = event.position.0 as f32;
            let y = event.position.1 as f32;

            match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.pointer_enter_serial = Some(serial);
                    self.last_cursor_shape = None;
                    let actions = self.ui.handle_input(PickerInput::PointerMove { x, y });
                    self.dispatch_ui_actions(actions);
                    self.update_cursor_shape(x, y);
                    handled_any = true;
                }
                PointerEventKind::Leave { .. } => {
                    self.pointer_enter_serial = None;
                    self.last_cursor_shape = None;
                    let actions = self.ui.handle_input(PickerInput::PointerLeave);
                    self.dispatch_ui_actions(actions);
                    handled_any = true;
                }
                PointerEventKind::Motion { .. } => {
                    let actions = self.ui.handle_input(PickerInput::PointerMove { x, y });
                    self.dispatch_ui_actions(actions);
                    self.update_cursor_shape(x, y);
                    handled_any = true;
                }
                PointerEventKind::Press { button, .. } => {
                    if let Some(btn) = map_pointer_button(button) {
                        let actions = self.ui.handle_input(PickerInput::PointerPress {
                            x,
                            y,
                            button: btn,
                        });
                        self.dispatch_ui_actions(actions);
                        self.update_cursor_shape(x, y);
                        handled_any = true;
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if let Some(btn) = map_pointer_button(button) {
                        let actions = self.ui.handle_input(PickerInput::PointerRelease {
                            x,
                            y,
                            button: btn,
                        });
                        self.dispatch_ui_actions(actions);
                        self.update_cursor_shape(x, y);
                        handled_any = true;
                    }
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    let dx = wayland_axis_to_qt_delta(&horizontal);
                    let dy = wayland_axis_to_qt_delta(&vertical);
                    if dx.abs() > 0.0 || dy.abs() > 0.0 {
                        let actions = self
                            .ui
                            .handle_input(PickerInput::PointerWheel { dx, dy });
                        self.dispatch_ui_actions(actions);
                        handled_any = true;
                    }
                }
            }
            if self.exiting {
                return;
            }
        }

        if handled_any {
            self.render_and_schedule();
        }
    }
}

impl ProvidesRegistryState for PickerWaylandApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpViewporter, ()> for PickerWaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &WpViewporter,
        _event: <WpViewporter as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewport, ()> for PickerWaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &WpViewport,
        _event: <WpViewport as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleManagerV1, ()> for PickerWaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &WpFractionalScaleManagerV1,
        _event: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleV1, WlSurface> for PickerWaylandApp {
    fn event(
        state: &mut Self,
        _proxy: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        surface: &WlSurface,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let mut changed = false;
            if let Some(ref mut slot) = state.overlay
                && slot.layer.wl_surface() == surface
                && slot.scale_120 != scale
            {
                slot.scale_120 = scale;
                if let Err(err) = slot.apply_geometry(&state.egl) {
                    warn!("bgpicker-picker: fractional scale resize failed: {err}");
                }
                changed = true;
            }
            if changed {
                state.sync_ui_viewport();
                state.render_and_schedule();
            }
        }
    }
}

impl Dispatch<WpCursorShapeManagerV1, ()> for PickerWaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &WpCursorShapeManagerV1,
        _event: <WpCursorShapeManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpCursorShapeDeviceV1, ()> for PickerWaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &WpCursorShapeDeviceV1,
        _event: <WpCursorShapeDeviceV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

delegate_registry!(PickerWaylandApp);
delegate_dispatch2!(PickerWaylandApp);

// ── Public Entrypoint ──────────────────────────────────────────────────────────

/// Runs the native Wayland layer-shell picker overlay until the user confirms a pick,
/// closes the overlay, receives IPC `Close`/`Toggle`, or receives `SIGINT`/`SIGTERM`.
pub fn run_overlay(
    opts: PickerCliOptions,
    ipc_listener: PickerIpcListener,
) -> Result<(), WaylandOverlayError> {
    let mut paths = BgPickerPaths::from_env();
    paths.socket_override = Some(opts.daemon_socket.clone());
    paths.picker_socket_override = Some(opts.picker_socket.clone());

    // 1. Synchronous fast Status query (<5ms) for `focused_output` & initial DaemonStateSnapshot.
    let status_client = IpcClient::new(&opts.daemon_socket).with_timeout(STARTUP_STATUS_TIMEOUT);
    let initial_status = fetch_daemon_status_quick(&status_client);
    let focused_output = initial_status
        .as_ref()
        .and_then(|st| st.focused_output.clone());
    let initial_snapshot = initial_status
        .as_ref()
        .map(|st| status_to_snapshot(st, false))
        .unwrap_or_else(|| fallback_snapshot_from_disk(&paths));

    // 2. Connect to Wayland and bind globals.
    let conn = Connection::connect_to_env()
        .map_err(|e| WaylandOverlayError::WaylandConnect(e.to_string()))?;
    let (globals, mut event_queue) = registry_queue_init::<PickerWaylandApp>(&conn)
        .map_err(|e| WaylandOverlayError::WaylandGlobal(e.to_string()))?;
    let qh = event_queue.handle();

    let compositor_state = CompositorState::bind(&globals, &qh)
        .map_err(|e| WaylandOverlayError::WaylandGlobal(format!("wl_compositor: {e}")))?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|e| WaylandOverlayError::WaylandGlobal(format!("zwlr_layer_shell_v1: {e}")))?;
    let output_state = OutputState::new(&globals, &qh);
    let seat_state = SeatState::new(&globals, &qh);
    let viewporter = globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok();
    let fractional_scale_mgr = globals
        .bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ())
        .ok();
    let cursor_shape_mgr = globals
        .bind::<WpCursorShapeManagerV1, _, _>(&qh, 1..=2, ())
        .ok();

    // 3. Initialize EGL + GL renderer + text atlas + bounded async thumbnail cache.
    let wl_display_ptr = conn.backend().display_ptr() as *mut std::ffi::c_void;
    let egl = unsafe { EglContext::new_wayland(wl_display_ptr)? };
    let renderer = PickerRenderer::new(Arc::clone(&egl.gl), egl.is_gles)?;
    let text = TextAtlas::new_system();
    let thumbs = ThumbnailCache::new(ThumbnailCacheConfig::default());

    // 4. Configure PickerUi with project directories and initial daemon state.
    let wallpaper_folder = opts
        .project_dir
        .join("wallpapers")
        .to_string_lossy()
        .into_owned();
    let video_folder = opts
        .project_dir
        .join("dynamic")
        .to_string_lossy()
        .into_owned();
    let ui_cfg = PickerConfig {
        shell_settings_hint: format!("Place your wallpapers in:\n{wallpaper_folder}"),
        wallpaper_folder,
        video_folder,
        has_wallpaper_configured: true,
        ..PickerConfig::default()
    };
    let mut ui = PickerUi::new(ui_cfg, 1920.0, 1080.0);
    let _ = ui.handle_input(PickerInput::SetDaemonState(initial_snapshot.clone()));

    // 5. Build calloop event loop, daemon IPC channel, and signal self-pipe.
    let mut event_loop = EventLoop::<PickerWaylandApp>::try_new()
        .map_err(|e| WaylandOverlayError::Dispatch(e.to_string()))?;
    let loop_handle = event_loop.handle();
    let loop_signal = event_loop.get_signal();

    let (daemon_tx, daemon_rx) = channel::channel::<DaemonEvent>();
    let daemon = DaemonBridge::new(
        opts.daemon_socket.clone(),
        opts.project_dir.clone(),
        daemon_tx,
    );

    let (_ipc_token, socket_guard) =
        ipc_listener.register_in_calloop(&loop_handle, |app, cmd| app.handle_picker_ipc(cmd))?;

    let mut signal_wakeup = SignalWakeup::install().ok();
    if let Some(read_fd) = signal_wakeup.as_mut().and_then(SignalWakeup::take_read_fd) {
        let _ = loop_handle.insert_source(
            Generic::new(read_fd, Interest::READ, Mode::Level),
            |_, wrapped, app| {
                let mut buf = [0u8; 64];
                loop {
                    let n = unsafe {
                        libc::read(
                            std::os::fd::AsRawFd::as_raw_fd(&**wrapped),
                            buf.as_mut_ptr().cast(),
                            buf.len(),
                        )
                    };
                    if n <= 0 {
                        break;
                    }
                }
                debug!("bgpicker-picker: termination signal received; exiting cleanly");
                app.begin_exit();
                Ok(PostAction::Remove)
            },
        );
    }

    loop_handle
        .insert_source(daemon_rx, |event, _, app| {
            if let channel::Event::Msg(msg) = event {
                app.handle_daemon_event(msg);
            }
        })
        .map_err(|e| WaylandOverlayError::Dispatch(e.to_string()))?;

    let mut app = PickerWaylandApp {
        registry_state: RegistryState::new(&globals),
        compositor_state,
        layer_shell,
        output_state,
        seat_state,
        viewporter,
        fractional_scale_mgr,
        cursor_shape_mgr,
        keyboard: None,
        pointer: None,
        cursor_shape_device: None,
        pointer_enter_serial: None,
        last_cursor_shape: None,
        modifiers: Modifiers::default(),
        overlay: None,
        ui,
        thumbs,
        text,
        renderer,
        egl,
        conn: conn.clone(),
        daemon,
        daemon_snapshot: initial_snapshot,
        socket_guard: Some(socket_guard),
        qh: qh.clone(),
        loop_handle: loop_handle.clone(),
        loop_signal,
        pacing_timer: None,
        last_tick: Instant::now(),
        force_redraw: true,
        exiting: false,
    };

    // 6. Roundtrip once so OutputState/SeatState receive output names, scales, and capabilities.
    event_queue
        .roundtrip(&mut app)
        .map_err(|e| WaylandOverlayError::Dispatch(e.to_string()))?;

    // 7. Pick target wl_output matching DaemonStatus.focused_output (fallback: first output).
    let (target_output, fallback_size, initial_scale) =
        select_target_output(&app.output_state, focused_output.as_deref());

    let slot = OverlaySlot::new(
        &app.compositor_state,
        &app.layer_shell,
        &qh,
        target_output.as_ref(),
        fallback_size,
        initial_scale,
        app.viewporter.as_ref(),
        app.fractional_scale_mgr.as_ref(),
    );
    app.overlay = Some(slot);

    // 8. Trigger initial action (opens overlay + fires async Catalog fetch immediately).
    let start_input = match opts.action {
        PickerStartAction::Open | PickerStartAction::Toggle => PickerInput::StartOpen,
        PickerStartAction::CycleNext => PickerInput::CycleNext,
        PickerStartAction::CyclePrevious => PickerInput::CyclePrevious,
        PickerStartAction::Close => PickerInput::StartClose,
    };
    let start_actions = app.ui.handle_input(start_input);
    app.dispatch_ui_actions(start_actions);
    // Always fetch catalog asynchronously on startup even if StartOpen wasn't emitted.
    if !app.exiting {
        app.daemon.request_catalog();
    }
    app.rearm_pacing_timer();

    // 9. Attach WaylandSource to calloop and run until exit.
    let wayland_source = WaylandSource::new(conn, event_queue);
    event_loop
        .handle()
        .insert_source(wayland_source, |_, queue, app| queue.dispatch_pending(app))
        .map_err(|e| WaylandOverlayError::Dispatch(e.to_string()))?;

    let run_res = if app.exiting {
        Ok(())
    } else {
        event_loop
            .run(None, &mut app, |_| {})
            .map_err(|e| WaylandOverlayError::Dispatch(e.to_string()))
    };

    // 10. Clean teardown: destroy GL/Wayland/socket first, then wait for any in-flight Apply IPC!
    app.teardown_gpu_and_surface();
    app.daemon.finish_pending_mutations();
    drop(signal_wakeup);
    drop(app);

    run_res
}

fn select_target_output(
    output_state: &OutputState,
    focused_output: Option<&str>,
) -> (Option<WlOutput>, (u32, u32), i32) {
    let outputs: Vec<WlOutput> = output_state.outputs().collect();

    let chosen = focused_output
        .and_then(|target| {
            outputs.iter().find(|out| {
                output_state
                    .info(out)
                    .and_then(|i| i.name)
                    .as_deref()
                    == Some(target)
            })
        })
        .or_else(|| outputs.first())
        .cloned();

    let (fallback_size, scale) = chosen
        .as_ref()
        .and_then(|out| output_state.info(out))
        .map(|info| {
            let size = info
                .logical_size
                .map(|(w, h)| (w.max(1) as u32, h.max(1) as u32))
                .unwrap_or((1920, 1080));
            (size, info.scale_factor.max(1))
        })
        .unwrap_or(((1920, 1080), 1));

    (chosen, fallback_size, scale)
}
