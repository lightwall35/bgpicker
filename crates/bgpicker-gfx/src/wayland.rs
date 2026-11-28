//! Wayland `wlr-layer-shell` runtime using `smithay-client-toolkit` + `calloop` + `WlEglSurface`.
//!
//! Manages per-monitor `bgpicker:wallpaper` and `bgpicker:backdrop` layer surfaces
//! across multi-monitor hotplug and fractional scaling (`wp_fractional_scale_v1` + `wp_viewporter`),
//! pacing animations cleanly via Wayland frame callbacks.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use calloop::channel::{self, Sender};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopSignal};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use khronos_egl as egl;
use log::{debug, warn};
use smithay_client_toolkit::compositor::{
    CompositorHandler, CompositorState, FrameCallbackData, Region,
};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_output;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_egl::WlEglSurface;
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::egl::EglContext;
use crate::error::{GfxError, Result};
use crate::frame::FrameInput;
use crate::layer::{LayerPipeline, LayerRole};
use crate::math::{KawaseSettings, ParallaxSettings, TransitionSpec};
use crate::shaders::ShaderPrograms;
use crate::splash::{ActiveSplash, StartupSplashSpec};

const ACTIVE_TIMER_INTERVAL: Duration = Duration::from_millis(4);
const IDLE_TIMER_INTERVAL: Duration = Duration::from_millis(50);
const FRAME_CALLBACK_TIMEOUT: Duration = Duration::from_millis(100);

/// Target layer selection for renderer commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerTargets {
    WallpaperOnly,
    BackdropOnly,
    Both,
}

impl LayerTargets {
    #[inline]
    pub const fn includes_wallpaper(self) -> bool {
        matches!(self, Self::WallpaperOnly | Self::Both)
    }

    #[inline]
    pub const fn includes_backdrop(self) -> bool {
        matches!(self, Self::BackdropOnly | Self::Both)
    }
}

/// Optional per-layer parallax mode targets applied atomically with a frame commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameModeTargets {
    pub wallpaper_parallax: Option<bool>,
    pub backdrop_parallax: Option<bool>,
}

/// Commands accepted by a running [`WaylandRenderer`] via [`RendererHandle`].
#[derive(Debug, Clone)]
pub enum RenderCommand {
    ApplyFrame {
        frame: FrameInput,
        transition: TransitionSpec,
        targets: LayerTargets,
        modes: FrameModeTargets,
    },
    StageFrame {
        frame: FrameInput,
        targets: LayerTargets,
    },
    CommitStaged {
        transition: TransitionSpec,
        targets: LayerTargets,
        modes: FrameModeTargets,
    },
    CancelStaged {
        targets: LayerTargets,
    },
    /// Feeds a decoded video frame from the shared video stream to foreground
    /// and (optionally) backdrop without duplicating CPU buffers or decoders.
    PushVideoFrame {
        frame: FrameInput,
        sync_backdrop: bool,
    },
    SetProgress {
        output: Option<String>,
        progress: f32,
        duration_ms: Option<u32>,
    },
    SetMode {
        parallax: bool,
        duration_ms: Option<u32>,
        targets: LayerTargets,
    },
    SetBackdropEnabled(bool),
    UpdateSettings {
        parallax: ParallaxSettings,
        kawase: KawaseSettings,
    },
    WaitPresented {
        reply: SyncSender<()>,
    },
    TrimIdle,
    Clear,
    Shutdown,
}

/// Snapshot of one active surface for status/diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceSnapshot {
    pub role: LayerRole,
    pub logical_size: (u32, u32),
    pub pixel_size: (u32, u32),
    pub scale_120: u32,
    pub presented: bool,
    pub transition_active: bool,
    pub transition_progress: f32,
    pub animated_progress: f32,
    pub mode_blend: f32,
}

/// Snapshot of one monitor output managed by the Wayland renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSnapshot {
    pub name: String,
    pub wallpaper: SurfaceSnapshot,
    pub backdrop: Option<SurfaceSnapshot>,
}

/// Thread-safe control handle to a running [`WaylandRenderer`].
#[derive(Clone)]
pub struct RendererHandle {
    tx: Sender<RenderCommand>,
    snapshots: Arc<Mutex<Vec<OutputSnapshot>>>,
    is_nvidia_gpu: bool,
}

impl RendererHandle {
    pub fn send(&self, cmd: RenderCommand) -> Result<()> {
        self.tx.send(cmd).map_err(|_| GfxError::ChannelClosed)
    }

    pub fn snapshots(&self) -> Vec<OutputSnapshot> {
        self.snapshots.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Returns `true` when the active Wayland EGL/GL vendor is NVIDIA, so video decoding
    /// should prefer same-GPU CUDA/NVDEC rather than copying from an unattached iGPU.
    pub fn prefers_cuda_decode(&self) -> bool {
        self.is_nvidia_gpu
    }

    /// Blocks until every configured output has swapped a live frame and received
    /// Wayland `wl_surface.frame` acknowledgement, bounded by `timeout`.
    pub fn wait_presented(&self, timeout: Duration) {
        let (tx, rx) = mpsc::sync_channel(1);
        if self.send(RenderCommand::WaitPresented { reply: tx }).is_ok() {
            let _ = rx.recv_timeout(timeout);
        }
    }
}

/// Computes scaled buffer pixel dimensions `(width, height)` from a surface-local
/// logical size and a `wp_fractional_scale_v1` numerator (`scale_120 / 120.0`).
#[inline]
pub fn compute_scaled_size(logical_w: u32, logical_h: u32, scale_120: u32) -> (u32, u32) {
    let scale = (scale_120.max(30) as f64) / 120.0;
    let pw = ((logical_w.max(1) as f64) * scale).round().max(1.0) as u32;
    let ph = ((logical_h.max(1) as f64) * scale).round().max(1.0) as u32;
    (pw, ph)
}

#[derive(Debug, Clone)]
struct ActiveState {
    parallax: ParallaxSettings,
    kawase: KawaseSettings,
    backdrop_enabled: bool,
    wallpaper_parallax: bool,
    backdrop_parallax: bool,
    wallpaper_frame: Option<FrameInput>,
    backdrop_frame: Option<FrameInput>,
    per_output_progress: HashMap<String, f32>,
}

impl Default for ActiveState {
    fn default() -> Self {
        Self {
            parallax: ParallaxSettings::default(),
            kawase: KawaseSettings::default(),
            backdrop_enabled: true,
            wallpaper_parallax: true,
            backdrop_parallax: true,
            wallpaper_frame: None,
            backdrop_frame: None,
            per_output_progress: HashMap::new(),
        }
    }
}

struct LayerSurfaceSlot {
    // Drop EGLSurface -> WlEglSurface -> add-on protocols -> LayerSurface (WlSurface) in order.
    egl_surface: Option<egl::Surface>,
    wl_egl: Option<WlEglSurface>,
    fractional_scale: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    layer: LayerSurface,
    logical_width: u32,
    logical_height: u32,
    pixel_width: u32,
    pixel_height: u32,
    scale_120: u32,
    configured: bool,
    opaque_region_size: Option<(u32, u32)>,
    waiting_frame_since: Option<Instant>,
    committed_serial: u64,
    media_committed_serial: u64,
    pending_frame_serial: u64,
    acknowledged_serial: u64,
    pipeline: LayerPipeline,
}

impl LayerSurfaceSlot {
    #[allow(clippy::too_many_arguments)]
    fn new(
        compositor: &CompositorState,
        layer_shell: &LayerShell,
        qh: &QueueHandle<WaylandApp>,
        output: &wl_output::WlOutput,
        role: LayerRole,
        initial_parallax: bool,
        viewporter: Option<&WpViewporter>,
        scale_mgr: Option<&WpFractionalScaleManagerV1>,
        initial_integer_scale: i32,
    ) -> Self {
        let wl_surface = compositor.create_surface(qh);
        if let Ok(region) = Region::new(compositor) {
            wl_surface.set_input_region(Some(region.wl_region()));
        }

        let viewport = viewporter.map(|vp| vp.get_viewport(&wl_surface, qh, ()));
        let fractional_scale = scale_mgr
            .map(|mgr| mgr.get_fractional_scale(&wl_surface, qh, wl_surface.clone()));

        if viewport.is_some() {
            wl_surface.set_buffer_scale(1);
        } else {
            wl_surface.set_buffer_scale(initial_integer_scale.max(1));
        }

        let layer = layer_shell.create_layer_surface(
            qh,
            wl_surface,
            Layer::Background,
            Some(role.namespace()),
            Some(output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, 0);
        layer.commit();

        let scale_120 = (initial_integer_scale.max(1) as u32) * 120;

        Self {
            egl_surface: None,
            wl_egl: None,
            fractional_scale,
            viewport,
            layer,
            logical_width: 0,
            logical_height: 0,
            pixel_width: 0,
            pixel_height: 0,
            scale_120,
            configured: false,
            opaque_region_size: None,
            waiting_frame_since: None,
            committed_serial: 0,
            media_committed_serial: 0,
            pending_frame_serial: 0,
            acknowledged_serial: 0,
            pipeline: LayerPipeline::new(role, initial_parallax),
        }
    }

    fn sync_opaque_region(&mut self, compositor: &CompositorState) {
        let desired = (self.pipeline.role == LayerRole::Wallpaper
            && self.pipeline.is_presented()
            && self.logical_width > 0
            && self.logical_height > 0)
            .then_some((self.logical_width, self.logical_height));
        if desired == self.opaque_region_size {
            return;
        }
        if let Some((w, h)) = desired {
            if let Ok(region) = Region::new(compositor) {
                region.add(0, 0, w as i32, h as i32);
                self.layer.wl_surface().set_opaque_region(Some(region.wl_region()));
            }
        } else {
            self.layer.wl_surface().set_opaque_region(None);
        }
        self.opaque_region_size = desired;
    }

    fn is_waiting_for_frame(&mut self, now: Instant) -> bool {
        let Some(since) = self.waiting_frame_since else {
            return false;
        };
        if waiting_frame_expired(since, now) {
            self.waiting_frame_since = None;
            return false;
        }
        true
    }

    fn apply_geometry(&mut self, egl: &EglContext) -> Result<()> {
        if self.logical_width == 0 || self.logical_height == 0 {
            return Ok(());
        }
        let (pw, ph) = compute_scaled_size(self.logical_width, self.logical_height, self.scale_120);

        if let Some(ref vp) = self.viewport {
            self.layer.wl_surface().set_buffer_scale(1);
            vp.set_destination(self.logical_width as i32, self.logical_height as i32);
        } else {
            let int_scale = (self.scale_120 / 120).max(1) as i32;
            self.layer.wl_surface().set_buffer_scale(int_scale);
        }

        if let Some(ref wl_egl) = self.wl_egl {
            if self.pixel_width != pw || self.pixel_height != ph {
                wl_egl.resize(pw as i32, ph as i32, 0, 0);
                self.pixel_width = pw;
                self.pixel_height = ph;
                if self.pipeline.is_presented() || self.committed_serial > 0 {
                    self.pipeline.mark_dirty();
                }
            }
        } else {
            let wl_egl = WlEglSurface::new(
                self.layer.wl_surface().id(),
                pw as i32,
                ph as i32,
            )
            .map_err(|e| GfxError::EglSurface(format!("WlEglSurface::new failed: {e}")))?;
            let egl_surf = unsafe { egl.create_window_surface(wl_egl.ptr() as *mut _)? };
            self.wl_egl = Some(wl_egl);
            self.egl_surface = Some(egl_surf);
            self.pixel_width = pw;
            self.pixel_height = ph;
            if self.pipeline.is_presented() || self.committed_serial > 0 {
                self.pipeline.mark_dirty();
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> SurfaceSnapshot {
        SurfaceSnapshot {
            role: self.pipeline.role,
            logical_size: (self.logical_width, self.logical_height),
            pixel_size: (self.pixel_width, self.pixel_height),
            scale_120: self.scale_120,
            presented: self.pipeline.is_presented(),
            transition_active: self.pipeline.is_transition_active(),
            transition_progress: self.pipeline.transition_progress(),
            animated_progress: self.pipeline.animated_progress(),
            mode_blend: self.pipeline.mode_blend(),
        }
    }

    fn destroy(&mut self, egl: &EglContext) {
        let _ = egl.make_fallback_current();
        self.pipeline.release_all(&egl.gl);
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

struct OutputSurfaceSet {
    wl_output: wl_output::WlOutput,
    name: String,
    fallback_logical_size: (u32, u32),
    integer_scale: i32,
    backdrop: Option<LayerSurfaceSlot>,
    wallpaper: LayerSurfaceSlot,
}

impl OutputSurfaceSet {
    fn snapshot(&self) -> OutputSnapshot {
        OutputSnapshot {
            name: self.name.clone(),
            wallpaper: self.wallpaper.snapshot(),
            backdrop: self.backdrop.as_ref().map(LayerSurfaceSlot::snapshot),
        }
    }

    fn destroy(&mut self, egl: &EglContext) {
        if let Some(ref mut b) = self.backdrop {
            b.destroy(egl);
        }
        self.wallpaper.destroy(egl);
    }
}

struct WaylandApp {
    registry_state: RegistryState,
    compositor_state: CompositorState,
    layer_shell: LayerShell,
    output_state: OutputState,
    viewporter: Option<WpViewporter>,
    fractional_scale_mgr: Option<WpFractionalScaleManagerV1>,

    outputs: Vec<OutputSurfaceSet>,
    programs: ShaderPrograms,
    egl: EglContext,
    // Keep a Connection handle last so wl_display outlives all EGL/Wayland surfaces & EglContext.
    _conn: Connection,
    active: ActiveState,
    snapshots: Arc<Mutex<Vec<OutputSnapshot>>>,
    present_waiters: Vec<SyncSender<()>>,
    startup_splash: Option<ActiveSplash>,
    loop_signal: LoopSignal,
    last_tick: Instant,
}

impl WaylandApp {
    fn sync_snapshots(&self) {
        if let Ok(mut guard) = self.snapshots.lock() {
            *guard = self.outputs.iter().map(OutputSurfaceSet::snapshot).collect();
        }
    }

    fn all_outputs_presented(&self) -> bool {
        !self.outputs.is_empty()
            && self.outputs.iter().all(|out| {
                let wp_ok = out.wallpaper.configured
                    && out.wallpaper.pipeline.is_presented()
                    && out.wallpaper.media_committed_serial > 0
                    && out.wallpaper.acknowledged_serial >= out.wallpaper.media_committed_serial;
                let bg_ok = out.backdrop.as_ref().is_none_or(|b| {
                    !b.configured
                        || !b.pipeline.is_presented()
                        || (b.media_committed_serial > 0
                            && b.acknowledged_serial >= b.media_committed_serial)
                });
                wp_ok && bg_ok
            })
    }

    fn notify_present_waiters(&mut self) {
        if !self.all_outputs_presented() {
            return;
        }
        if let Some(splash) = self.startup_splash.take() {
            splash.retire();
            let _ = self._conn.flush();
        }
        for waiter in self.present_waiters.drain(..) {
            let _ = waiter.send(());
        }
    }

    fn create_output_surfaces(&mut self, qh: &QueueHandle<Self>, wl_output: wl_output::WlOutput) {
        if self.outputs.iter().any(|o| o.wl_output == wl_output) {
            return;
        }
        let Some(info) = self.output_state.info(&wl_output) else {
            return;
        };
        let name = info.name.unwrap_or_else(|| format!("output-{}", info.id));
        let fallback_logical_size = info
            .logical_size
            .map(|(w, h)| (w.max(1) as u32, h.max(1) as u32))
            .unwrap_or((1920, 1080));
        let integer_scale = info.scale_factor.max(1);

        // Create backdrop BEFORE wallpaper so non-rule compositors place backdrop underneath.
        let mut backdrop = if self.active.backdrop_enabled {
            Some(LayerSurfaceSlot::new(
                &self.compositor_state,
                &self.layer_shell,
                qh,
                &wl_output,
                LayerRole::Backdrop,
                self.active.backdrop_parallax,
                self.viewporter.as_ref(),
                self.fractional_scale_mgr.as_ref(),
                integer_scale,
            ))
        } else {
            None
        };

        let mut wallpaper = LayerSurfaceSlot::new(
            &self.compositor_state,
            &self.layer_shell,
            qh,
            &wl_output,
            LayerRole::Wallpaper,
            self.active.wallpaper_parallax,
            self.viewporter.as_ref(),
            self.fractional_scale_mgr.as_ref(),
            integer_scale,
        );

        let _ = self.egl.make_fallback_current();
        if let Some(progress) = self.active.per_output_progress.get(&name).copied() {
            wallpaper.pipeline.set_progress(progress, 0);
            if let Some(ref mut b) = backdrop {
                b.pipeline.set_progress(progress, 0);
            }
        }
        if let Some(ref frame) = self.active.wallpaper_frame {
            let _ = wallpaper.pipeline.apply_frame(
                &self.egl.gl,
                frame,
                TransitionSpec::instant(),
                self.egl.bgra_ext_supported,
            );
        }
        if let (Some(b), Some(frame)) = (backdrop.as_mut(), self.active.backdrop_frame.as_ref()) {
            let _ = b.pipeline.apply_frame(
                &self.egl.gl,
                frame,
                TransitionSpec::instant(),
                self.egl.bgra_ext_supported,
            );
        }

        self.outputs.push(OutputSurfaceSet {
            wl_output,
            name,
            fallback_logical_size,
            integer_scale,
            backdrop,
            wallpaper,
        });
        self.sync_snapshots();
        let _ = self._conn.flush();
        self.notify_present_waiters();
    }

    fn find_slot_mut_by_layer<'a>(
        outputs: &'a mut [OutputSurfaceSet],
        layer: &LayerSurface,
    ) -> Option<(&'a mut LayerSurfaceSlot, (u32, u32))> {
        for out in outputs {
            if out.wallpaper.layer == *layer {
                return Some((&mut out.wallpaper, out.fallback_logical_size));
            }
            if let Some(ref mut b) = out.backdrop
                && b.layer == *layer
            {
                return Some((b, out.fallback_logical_size));
            }
        }
        None
    }

    fn find_slot_mut_by_wl_surface<'a>(
        outputs: &'a mut [OutputSurfaceSet],
        surface: &WlSurface,
    ) -> Option<&'a mut LayerSurfaceSlot> {
        for out in outputs {
            if out.wallpaper.layer.wl_surface() == surface {
                return Some(&mut out.wallpaper);
            }
            if let Some(ref mut b) = out.backdrop
                && b.layer.wl_surface() == surface
            {
                return Some(b);
            }
        }
        None
    }

    fn advance_active_ticks(&mut self, now: Instant) {
        let dt_s = now.saturating_duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        for out in &mut self.outputs {
            out.wallpaper.pipeline.tick(&self.egl.gl, dt_s);
            if let Some(ref mut b) = out.backdrop {
                b.pipeline.tick(&self.egl.gl, dt_s);
            }
        }
    }

    fn handle_command(&mut self, qh: &QueueHandle<Self>, cmd: RenderCommand) {
        let _ = self.egl.make_fallback_current();
        let now = Instant::now();
        let was_animating = self.any_animating();
        if was_animating {
            // Advance existing animations to `now` before starting/updating targets
            // without double-swapping buffers in one command.
            self.advance_active_ticks(now);
        } else {
            // Refresh stale idle clock so Frame 0 of any new animation starts at dt_s ≈ 0.
            self.last_tick = now;
        }
        match cmd {
            RenderCommand::ApplyFrame {
                frame,
                transition,
                targets,
                modes,
            } => {
                if targets.includes_wallpaper() {
                    self.active.wallpaper_frame = Some(frame.clone());
                    if let Some(p) = modes.wallpaper_parallax {
                        self.active.wallpaper_parallax = p;
                    }
                }
                if targets.includes_backdrop() {
                    self.active.backdrop_frame = Some(frame.clone());
                    if let Some(p) = modes.backdrop_parallax {
                        self.active.backdrop_parallax = p;
                    }
                }
                for out in &mut self.outputs {
                    if targets.includes_wallpaper() {
                        let _ = out.wallpaper.pipeline.apply_frame_with_mode(
                            &self.egl.gl,
                            &frame,
                            transition,
                            modes.wallpaper_parallax,
                            self.egl.bgra_ext_supported,
                        );
                    }
                    if targets.includes_backdrop()
                        && let Some(ref mut b) = out.backdrop
                    {
                        let _ = b.pipeline.apply_frame_with_mode(
                            &self.egl.gl,
                            &frame,
                            transition,
                            modes.backdrop_parallax,
                            self.egl.bgra_ext_supported,
                        );
                    }
                }
            }
            RenderCommand::StageFrame { frame, targets } => {
                if targets.includes_wallpaper() {
                    self.active.wallpaper_frame = Some(frame.clone());
                }
                if targets.includes_backdrop() {
                    self.active.backdrop_frame = Some(frame.clone());
                }
                for out in &mut self.outputs {
                    if targets.includes_wallpaper() {
                        let _ = out.wallpaper.pipeline.stage_frame(
                            &self.egl.gl,
                            &frame,
                            self.egl.bgra_ext_supported,
                        );
                    }
                    if targets.includes_backdrop()
                        && let Some(ref mut b) = out.backdrop
                    {
                        let _ = b.pipeline.stage_frame(
                            &self.egl.gl,
                            &frame,
                            self.egl.bgra_ext_supported,
                        );
                    }
                }
            }
            RenderCommand::CommitStaged {
                transition,
                targets,
                modes,
            } => {
                if targets.includes_wallpaper()
                    && let Some(p) = modes.wallpaper_parallax
                {
                    self.active.wallpaper_parallax = p;
                }
                if targets.includes_backdrop()
                    && let Some(p) = modes.backdrop_parallax
                {
                    self.active.backdrop_parallax = p;
                }
                for out in &mut self.outputs {
                    if targets.includes_wallpaper() {
                        out.wallpaper.pipeline.commit_staged_with_mode(
                            &self.egl.gl,
                            transition,
                            modes.wallpaper_parallax,
                        );
                    }
                    if targets.includes_backdrop()
                        && let Some(ref mut b) = out.backdrop
                    {
                        b.pipeline.commit_staged_with_mode(
                            &self.egl.gl,
                            transition,
                            modes.backdrop_parallax,
                        );
                    }
                }
            }
            RenderCommand::CancelStaged { targets } => {
                for out in &mut self.outputs {
                    if targets.includes_wallpaper() {
                        out.wallpaper.pipeline.cancel_staged(&self.egl.gl);
                    }
                    if targets.includes_backdrop()
                        && let Some(ref mut b) = out.backdrop
                    {
                        b.pipeline.cancel_staged(&self.egl.gl);
                    }
                }
            }
            RenderCommand::PushVideoFrame {
                frame,
                sync_backdrop,
            } => {
                self.active.wallpaper_frame = Some(frame.clone());
                if sync_backdrop {
                    self.active.backdrop_frame = Some(frame.clone());
                }
                for out in &mut self.outputs {
                    let _ = out.wallpaper.pipeline.push_video_frame(
                        &self.egl.gl,
                        &frame,
                        self.egl.bgra_ext_supported,
                    );
                    if sync_backdrop
                        && let Some(ref mut b) = out.backdrop
                    {
                        let _ = b.pipeline.push_video_frame(
                            &self.egl.gl,
                            &frame,
                            self.egl.bgra_ext_supported,
                        );
                    }
                }
            }
            RenderCommand::SetProgress {
                output,
                progress,
                duration_ms,
            } => {
                let dur = duration_ms.unwrap_or(self.active.parallax.duration_ms);
                if let Some(ref name) = output {
                    self.active.per_output_progress.insert(name.clone(), progress);
                } else {
                    for out in &self.outputs {
                        self.active.per_output_progress.insert(out.name.clone(), progress);
                    }
                }
                for out in &mut self.outputs {
                    if output.as_ref().is_none_or(|n| n == &out.name) {
                        out.wallpaper.pipeline.set_progress(progress, dur);
                        if let Some(ref mut b) = out.backdrop {
                            b.pipeline.set_progress(progress, dur);
                        }
                    }
                }
            }
            RenderCommand::SetMode {
                parallax,
                duration_ms,
                targets,
            } => {
                let dur = duration_ms.unwrap_or(self.active.parallax.mode_transition_ms);
                if targets.includes_wallpaper() {
                    self.active.wallpaper_parallax = parallax;
                }
                if targets.includes_backdrop() {
                    self.active.backdrop_parallax = parallax;
                }
                for out in &mut self.outputs {
                    if targets.includes_wallpaper() {
                        out.wallpaper.pipeline.set_mode(parallax, dur);
                    }
                    if targets.includes_backdrop()
                        && let Some(ref mut b) = out.backdrop
                    {
                        b.pipeline.set_mode(parallax, dur);
                    }
                }
            }
            RenderCommand::SetBackdropEnabled(enabled) => {
                if self.active.backdrop_enabled != enabled {
                    self.active.backdrop_enabled = enabled;
                    if enabled {
                        for out in &mut self.outputs {
                            if out.backdrop.is_none() {
                                let mut slot = LayerSurfaceSlot::new(
                                    &self.compositor_state,
                                    &self.layer_shell,
                                    qh,
                                    &out.wl_output,
                                    LayerRole::Backdrop,
                                    self.active.backdrop_parallax,
                                    self.viewporter.as_ref(),
                                    self.fractional_scale_mgr.as_ref(),
                                    out.integer_scale,
                                );
                                if let Some(p) = self.active.per_output_progress.get(&out.name).copied() {
                                    slot.pipeline.set_progress(p, 0);
                                }
                                if let Some(ref frame) = self.active.backdrop_frame {
                                    let _ = slot.pipeline.apply_frame(
                                        &self.egl.gl,
                                        frame,
                                        TransitionSpec::instant(),
                                        self.egl.bgra_ext_supported,
                                    );
                                }
                                out.backdrop = Some(slot);
                            }
                        }
                    } else {
                        for out in &mut self.outputs {
                            if let Some(mut slot) = out.backdrop.take() {
                                slot.destroy(&self.egl);
                            }
                        }
                    }
                }
            }
            RenderCommand::UpdateSettings { parallax, kawase } => {
                self.active.parallax = parallax.clamped();
                self.active.kawase = kawase;
                for out in &mut self.outputs {
                    out.wallpaper.pipeline.mark_dirty();
                    if let Some(ref mut b) = out.backdrop {
                        b.pipeline.mark_dirty();
                    }
                }
            }
            RenderCommand::WaitPresented { reply } => {
                self.present_waiters.push(reply);
            }
            RenderCommand::TrimIdle => {
                self.active.wallpaper_frame = None;
                self.active.backdrop_frame = None;
                for out in &mut self.outputs {
                    out.wallpaper.pipeline.trim_idle(&self.egl.gl);
                    if let Some(ref mut b) = out.backdrop {
                        b.pipeline.trim_idle(&self.egl.gl);
                    }
                }
            }
            RenderCommand::Clear => {
                self.active.wallpaper_frame = None;
                self.active.backdrop_frame = None;
                for out in &mut self.outputs {
                    out.wallpaper.pipeline.release_all(&self.egl.gl);
                    if let Some(ref mut b) = out.backdrop {
                        b.pipeline.release_all(&self.egl.gl);
                    }
                }
            }
            RenderCommand::Shutdown => {
                self.loop_signal.stop();
            }
        }
        self.render_due(qh);
    }

    fn render_due(&mut self, qh: &QueueHandle<Self>) {
        let now = Instant::now();
        let dt_s = now.saturating_duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;

        let zoom = self.active.parallax.zoom;
        let kawase = self.active.kawase;

        for out in &mut self.outputs {
            render_slot(
                &self.egl,
                &self.programs,
                &self.compositor_state,
                qh,
                &mut out.wallpaper,
                dt_s,
                zoom,
                kawase,
                now,
            );
            if let Some(ref mut b) = out.backdrop {
                render_slot(
                    &self.egl,
                    &self.programs,
                    &self.compositor_state,
                    qh,
                    b,
                    dt_s,
                    zoom,
                    kawase,
                    now,
                );
            }
        }
        self.sync_snapshots();
        let _ = self._conn.flush();
        self.notify_present_waiters();
    }

    fn any_animating(&self) -> bool {
        self.outputs.iter().any(|out| {
            out.wallpaper.pipeline.is_animating()
                || out
                    .backdrop
                    .as_ref()
                    .is_some_and(|b| b.pipeline.is_animating())
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn render_slot(
    egl: &EglContext,
    programs: &ShaderPrograms,
    compositor: &CompositorState,
    qh: &QueueHandle<WaylandApp>,
    slot: &mut LayerSurfaceSlot,
    dt_s: f32,
    zoom: f32,
    kawase: KawaseSettings,
    now: Instant,
) {
    let should_draw = slot.pipeline.tick(&egl.gl, dt_s);
    if !should_draw
        || !slot.configured
        || (!slot.pipeline.is_presented() && slot.committed_serial == 0)
        || slot.is_waiting_for_frame(now)
    {
        return;
    }
    let Some(egl_surface) = slot.egl_surface else {
        return;
    };
    if egl.make_current(Some(egl_surface)).is_err() {
        return;
    }

    let w = slot.pixel_width;
    let h = slot.pixel_height;
    if let Err(err) = slot
        .pipeline
        .render(&egl.gl, programs, None, w, h, zoom, kawase)
    {
        warn!("bgpicker-gfx render error: {err}");
        return;
    }

    slot.sync_opaque_region(compositor);

    let next_serial = slot.committed_serial + 1;
    if slot.pipeline.is_animating() || slot.pending_frame_serial < next_serial {
        let wl_surface = slot.layer.wl_surface();
        wl_surface.frame(qh, FrameCallbackData(wl_surface.clone()));
        slot.waiting_frame_since = Some(Instant::now());
        slot.pending_frame_serial = next_serial;
    }

    slot.layer
        .wl_surface()
        .damage_buffer(0, 0, w as i32, h as i32);
    if let Err(err) = egl.swap_buffers(egl_surface) {
        warn!("bgpicker-gfx eglSwapBuffers error: {err}");
    } else {
        slot.committed_serial = next_serial;
        slot.media_committed_serial = if slot.pipeline.is_presented() {
            next_serial
        } else {
            0
        };
    }
}

impl Drop for WaylandApp {
    fn drop(&mut self) {
        if let Some(splash) = self.startup_splash.take() {
            splash.retire();
        }
        let _ = self.egl.make_fallback_current();
        for mut out in self.outputs.drain(..) {
            out.destroy(&self.egl);
        }
        self.programs.destroy(&self.egl.gl);
    }
}

impl CompositorHandler for WaylandApp {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &WlSurface,
        new_factor: i32,
    ) {
        if self.fractional_scale_mgr.is_some() {
            return;
        }
        let scale_120 = (new_factor.max(1) as u32) * 120;
        if let Some(slot) = Self::find_slot_mut_by_wl_surface(&mut self.outputs, surface)
            && slot.scale_120 != scale_120
        {
            slot.scale_120 = scale_120;
            let _ = slot.apply_geometry(&self.egl);
        }
        self.render_due(qh);
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
        qh: &QueueHandle<Self>,
        surface: &WlSurface,
        _time: u32,
    ) {
        if let Some(slot) = Self::find_slot_mut_by_wl_surface(&mut self.outputs, surface) {
            slot.waiting_frame_since = None;
            slot.acknowledged_serial = slot.acknowledged_serial.max(slot.pending_frame_serial);
        }
        self.render_due(qh);
        self.notify_present_waiters();
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for WaylandApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.create_output_surfaces(qh, output);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if !self.outputs.iter().any(|o| o.wl_output == output) {
            self.create_output_surfaces(qh, output);
            return;
        }
        if let Some(info) = self.output_state.info(&output) {
            for out in &mut self.outputs {
                if out.wl_output == output {
                    if let Some(name) = info.name.clone() {
                        out.name = name;
                    }
                    if let Some((w, h)) = info.logical_size {
                        out.fallback_logical_size = (w.max(1) as u32, h.max(1) as u32);
                    }
                    out.integer_scale = info.scale_factor.max(1);
                }
            }
            self.sync_snapshots();
        }
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(pos) = self.outputs.iter().position(|o| o.wl_output == output) {
            let mut removed = self.outputs.remove(pos);
            debug!("output destroyed: {}", removed.name);
            removed.destroy(&self.egl);
            self.sync_snapshots();
        }
    }
}

impl LayerShellHandler for WaylandApp {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        for out in &mut self.outputs {
            if out.wallpaper.layer == *layer {
                out.wallpaper.destroy(&self.egl);
            } else if let Some(ref mut b) = out.backdrop
                && b.layer == *layer
            {
                let mut dead = out.backdrop.take().unwrap();
                dead.destroy(&self.egl);
            }
        }
        self.sync_snapshots();
        let _ = self._conn.flush();
        self.notify_present_waiters();
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some((slot, fallback)) = Self::find_slot_mut_by_layer(&mut self.outputs, layer) else {
            return;
        };
        let lw = NonZeroU32::new(configure.new_size.0).map_or(fallback.0, NonZeroU32::get);
        let lh = NonZeroU32::new(configure.new_size.1).map_or(fallback.1, NonZeroU32::get);
        slot.logical_width = lw;
        slot.logical_height = lh;
        slot.configured = true;
        if let Err(err) = slot.apply_geometry(&self.egl) {
            warn!("failed to resize Wayland EGL surface: {err}");
        }
        self.render_due(qh);
    }
}

impl ProvidesRegistryState for WaylandApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

impl Dispatch<WpViewporter, ()> for WaylandApp {
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

impl Dispatch<WpViewport, ()> for WaylandApp {
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

impl Dispatch<WpFractionalScaleManagerV1, ()> for WaylandApp {
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

impl Dispatch<WpFractionalScaleV1, WlSurface> for WaylandApp {
    fn event(
        state: &mut Self,
        _proxy: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        surface: &WlSurface,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            if let Some(slot) = Self::find_slot_mut_by_wl_surface(&mut state.outputs, surface)
                && slot.scale_120 != scale
            {
                slot.scale_120 = scale;
                if let Err(err) = slot.apply_geometry(&state.egl) {
                    warn!("fractional scale resize failed: {err}");
                }
            }
            state.render_due(qh);
        }
    }
}

delegate_registry!(WaylandApp);
delegate_dispatch2!(WaylandApp);

/// Runs the Wayland layer-shell + EGL GPU renderer event loop on the current thread.
pub struct WaylandRenderer {
    // Drop `app` (outputs, GL programs, EGL context) BEFORE `event_loop` (WaylandSource / wl_display).
    app: WaylandApp,
    event_loop: EventLoop<'static, WaylandApp>,
    qh: QueueHandle<WaylandApp>,
    handle: RendererHandle,
}

impl WaylandRenderer {
    /// Connects to the active Wayland compositor, binds `zwlr_layer_shell_v1`, `wl_output`,
    /// `wp_viewporter`, and `wp_fractional_scale_manager_v1`, and initializes the EGL/GL pipeline.
    pub fn connect(backdrop_enabled: bool) -> Result<Self> {
        Self::connect_with_splash(backdrop_enabled, None)
    }

    /// Connects to Wayland, optionally displaying an immediate in-process `wl_shm` boot splash
    /// before initializing EGL/shaders, then keeps that splash alive until EGL Frame 1 presents.
    pub fn connect_with_splash(
        backdrop_enabled: bool,
        splash: Option<StartupSplashSpec>,
    ) -> Result<Self> {
        let conn = Connection::connect_to_env()
            .map_err(|e| GfxError::WaylandConnect(e.to_string()))?;

        // Show `wl_shm` splash BEFORE EGL/GPU initialization so cold-boot driver/shader
        // warmup never delays Frame 1 or causes multi-process handoff races.
        let startup_splash = splash
            .as_ref()
            .and_then(|spec| ActiveSplash::show(&conn, spec));

        let (globals, event_queue) = registry_queue_init::<WaylandApp>(&conn)
            .map_err(|e| GfxError::WaylandGlobal(e.to_string()))?;
        let qh = event_queue.handle();

        let compositor_state = CompositorState::bind(&globals, &qh)
            .map_err(|e| GfxError::WaylandGlobal(format!("wl_compositor: {e}")))?;
        let layer_shell = LayerShell::bind(&globals, &qh)
            .map_err(|e| GfxError::WaylandGlobal(format!("zwlr_layer_shell_v1: {e}")))?;
        let output_state = OutputState::new(&globals, &qh);
        let viewporter = globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok();
        let fractional_scale_mgr = globals
            .bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ())
            .ok();

        let wl_display_ptr = conn.backend().display_ptr() as *mut std::ffi::c_void;
        let egl = unsafe { EglContext::new_wayland(wl_display_ptr)? };
        let is_nvidia_gpu = egl.is_nvidia;
        let programs = ShaderPrograms::compile(&egl.gl, egl.is_gles)?;

        let event_loop = EventLoop::<WaylandApp>::try_new()
            .map_err(|e| GfxError::WaylandDispatch(e.to_string()))?;
        let loop_signal = event_loop.get_signal();
        let snapshots = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel::channel::<RenderCommand>();

        let active = ActiveState {
            backdrop_enabled,
            ..Default::default()
        };

        let mut app = WaylandApp {
            registry_state: RegistryState::new(&globals),
            compositor_state,
            layer_shell,
            output_state,
            viewporter,
            fractional_scale_mgr,
            outputs: Vec::new(),
            programs,
            egl,
            _conn: conn.clone(),
            active,
            snapshots: Arc::clone(&snapshots),
            present_waiters: Vec::new(),
            startup_splash,
            loop_signal,
            last_tick: Instant::now(),
        };

        // Initialize surfaces for any outputs already advertised during registry init.
        let initial_outputs: Vec<_> = app.output_state.outputs().collect();
        for out in initial_outputs {
            app.create_output_surfaces(&qh, out);
        }

        let wayland_source = WaylandSource::new(conn, event_queue);
        event_loop
            .handle()
            .insert_source(wayland_source, |_, queue, app| queue.dispatch_pending(app))
            .map_err(|e| GfxError::WaylandDispatch(e.to_string()))?;

        let qh_chan = qh.clone();
        event_loop
            .handle()
            .insert_source(rx, move |event, _, app| {
                if let channel::Event::Msg(cmd) = event {
                    app.handle_command(&qh_chan, cmd);
                }
            })
            .map_err(|e| GfxError::WaylandDispatch(e.to_string()))?;

        let qh_timer = qh.clone();
        event_loop
            .handle()
            .insert_source(
                Timer::from_duration(ACTIVE_TIMER_INTERVAL),
                move |_, _, app| {
                    if app.any_animating() {
                        app.render_due(&qh_timer);
                        TimeoutAction::ToDuration(ACTIVE_TIMER_INTERVAL)
                    } else {
                        app.last_tick = Instant::now();
                        TimeoutAction::ToDuration(IDLE_TIMER_INTERVAL)
                    }
                },
            )
            .map_err(|e| GfxError::WaylandDispatch(e.to_string()))?;

        let handle = RendererHandle {
            tx,
            snapshots,
            is_nvidia_gpu,
        };

        Ok(Self {
            app,
            event_loop,
            qh,
            handle,
        })
    }

    pub fn handle(&self) -> RendererHandle {
        self.handle.clone()
    }

    /// Dispatches one event loop iteration (blocking up to `timeout`), useful for
    /// tests or custom event loop integration.
    pub fn step(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.event_loop
            .dispatch(timeout, &mut self.app)
            .map_err(|e| GfxError::WaylandDispatch(e.to_string()))?;
        self.app.render_due(&self.qh);
        Ok(())
    }

    /// Runs the renderer event loop until [`RenderCommand::Shutdown`] is received.
    pub fn run(mut self) -> Result<()> {
        self.event_loop
            .run(None, &mut self.app, |_| {})
            .map_err(|e| GfxError::WaylandDispatch(e.to_string()))
    }
}

#[inline]
fn waiting_frame_expired(since: Instant, now: Instant) -> bool {
    now.saturating_duration_since(since) >= FRAME_CALLBACK_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_waiting_frame_expires_after_timeout() {
        let start = Instant::now();
        assert!(!waiting_frame_expired(start, start + Duration::from_millis(50)));
        assert!(!waiting_frame_expired(start, start + Duration::from_millis(99)));
        assert!(waiting_frame_expired(start, start + FRAME_CALLBACK_TIMEOUT));
        assert!(waiting_frame_expired(start, start + Duration::from_millis(250)));
    }
}
