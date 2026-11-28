//! In-process Wayland `wl_shm` boot splash rendered before EGL/GL initialization.
//!
//! Renders pre-rendered `BGPX` XRGB8888 buffers (`startup-splash-fg.bin`) or
//! decoded fallback images onto temporary `Layer::Background` surfaces before
//! `EglContext::new_wayland` runs, then destroys those temporary surfaces once
//! `bgpicker:wallpaper` presents its first real compositor-acknowledged EGL frame.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bgpicker_core::splash::{load_bgpx_into_memfd, trace_startup};
use bgpicker_core::{
    MemfdBuffer, compute_scaled_size_120, cover_fill_to_xrgb8888, read_splash_header,
    write_pixels_to_memfd,
};
use image::ImageReader;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, WpPresentationFeedback},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

const FG_NAMESPACE: &str = "bgpick-fg";
const BG_NAMESPACE: &str = "wallpaper";
const SPLASH_WAIT: Duration = Duration::from_millis(250);

/// Input media paths for the in-process pre-EGL `wl_shm` boot splash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupSplashSpec {
    pub fg_bin: Option<PathBuf>,
    pub fg_image: Option<PathBuf>,
    pub bg_image: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotRole {
    Foreground,
    Backdrop,
}

struct OutputInfo {
    proxy: WlOutput,
    integer_scale: i32,
}

struct SplashSlot {
    role: SlotRole,
    output: WlOutput,
    surface: WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    viewport: Option<WpViewport>,
    fractional_scale: Option<WpFractionalScaleV1>,
    logical_width: u32,
    logical_height: u32,
    scale_120: u32,
    has_preferred_scale: bool,
    configured: bool,
    rendered_geometry: Option<(u32, u32, u32)>,
    presented: bool,
    active_pool: Option<WlShmPool>,
    active_buffer: Option<WlBuffer>,
}

impl SplashSlot {
    #[allow(clippy::too_many_arguments)]
    fn create(
        compositor: &WlCompositor,
        layer_shell: &ZwlrLayerShellV1,
        viewporter: Option<&WpViewporter>,
        fractional_mgr: Option<&WpFractionalScaleManagerV1>,
        empty_region: Option<&WlRegion>,
        qh: &QueueHandle<SplashState>,
        output: &WlOutput,
        initial_scale: i32,
        role: SlotRole,
    ) -> Self {
        let surface = compositor.create_surface(qh, ());
        if let Some(region) = empty_region {
            surface.set_input_region(Some(region));
        }

        let viewport = viewporter.map(|vp| vp.get_viewport(&surface, qh, ()));
        let fractional_scale = viewport
            .as_ref()
            .and(fractional_mgr)
            .map(|mgr| mgr.get_fractional_scale(&surface, qh, surface.clone()));

        if viewport.is_some() {
            surface.set_buffer_scale(1);
        } else {
            surface.set_buffer_scale(initial_scale.max(1));
        }

        let ns = match role {
            SlotRole::Foreground => FG_NAMESPACE,
            SlotRole::Backdrop => BG_NAMESPACE,
        };
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            Some(output),
            Layer::Background,
            ns.into(),
            qh,
            (),
        );
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer_surface.set_size(0, 0);
        surface.commit();

        Self {
            role,
            output: output.clone(),
            surface,
            layer_surface,
            viewport,
            fractional_scale,
            logical_width: 0,
            logical_height: 0,
            scale_120: (initial_scale.max(1) as u32) * 120,
            has_preferred_scale: false,
            configured: false,
            rendered_geometry: None,
            presented: false,
            active_pool: None,
            active_buffer: None,
        }
    }

    fn destroy(&mut self) {
        if let Some(buf) = self.active_buffer.take() {
            buf.destroy();
        }
        if let Some(pool) = self.active_pool.take() {
            pool.destroy();
        }
        if let Some(fs) = self.fractional_scale.take() {
            fs.destroy();
        }
        if let Some(vp) = self.viewport.take() {
            vp.destroy();
        }
        self.layer_surface.destroy();
        self.surface.destroy();
    }
}

struct SplashState {
    spec: StartupSplashSpec,
    compositor: WlCompositor,
    shm: WlShm,
    layer_shell: ZwlrLayerShellV1,
    viewporter: Option<WpViewporter>,
    fractional_mgr: Option<WpFractionalScaleManagerV1>,
    presentation: Option<WpPresentation>,
    presentation_clock: Option<u32>,
    foreground_bitmap: Option<MemfdBuffer>,
    empty_region: Option<WlRegion>,
    qh: QueueHandle<Self>,
    outputs: Vec<OutputInfo>,
    slots: Vec<SplashSlot>,
}

impl SplashState {
    fn spawn_role_slots(&mut self, role: SlotRole) {
        for out in &self.outputs {
            let slot = SplashSlot::create(
                &self.compositor,
                &self.layer_shell,
                self.viewporter.as_ref(),
                self.fractional_mgr.as_ref(),
                self.empty_region.as_ref(),
                &self.qh,
                &out.proxy,
                out.integer_scale,
                role,
            );
            self.slots.push(slot);
        }
    }

    fn render_ready_slots(&mut self, conn: &Connection) {
        for slot in &mut self.slots {
            if !slot.configured || slot.logical_width == 0 || slot.logical_height == 0 {
                continue;
            }
            let (pw, ph) =
                compute_scaled_size_120(slot.logical_width, slot.logical_height, slot.scale_120);
            let bitmap = self.foreground_bitmap.as_ref().filter(|bitmap| {
                slot.role == SlotRole::Foreground
                    && ((bitmap.width(), bitmap.height()) == (pw, ph)
                        || (slot.viewport.is_some()
                            && same_aspect((bitmap.width(), bitmap.height()), (pw, ph))))
            });
            let scale_key = if bitmap.is_some() && slot.viewport.is_some() {
                0 // Fractional scale changes reuse the same buffer and logical destination.
            } else {
                slot.scale_120
            };
            let geometry = (slot.logical_width, slot.logical_height, scale_key);
            if slot.rendered_geometry == Some(geometry) {
                continue;
            }
            slot.rendered_geometry = Some(geometry);

            let load_started = Instant::now();
            let loaded;
            let (memfd, source) = if let Some(bitmap) = bitmap {
                (bitmap, "bgpx")
            } else {
                let (bin, image) = match slot.role {
                    SlotRole::Foreground => {
                        (self.spec.fg_bin.as_deref(), self.spec.fg_image.as_deref())
                    }
                    SlotRole::Backdrop => (None, self.spec.bg_image.as_deref()),
                };
                match load_splash_fallback(bin, image, pw, ph) {
                    Ok((memfd, source)) => {
                        loaded = memfd;
                        (&loaded, source)
                    }
                    Err(err) => {
                        trace_startup(
                            "shm_buffer_failed",
                            format_args!("role={:?} error={err}", slot.role),
                        );
                        continue;
                    }
                }
            };

            if let Some(ref vp) = slot.viewport {
                slot.surface.set_buffer_scale(1);
                if bitmap.is_some() {
                    set_viewport_crop(vp, memfd, slot.logical_width, slot.logical_height);
                } else {
                    vp.set_source(-1.0, -1.0, -1.0, -1.0);
                }
                vp.set_destination(slot.logical_width as i32, slot.logical_height as i32);
            } else {
                let int_scale = (slot.scale_120 / 120).max(1) as i32;
                slot.surface.set_buffer_scale(int_scale);
            }

            let pool =
                self.shm
                    .create_pool(memfd.fd().as_fd(), memfd.byte_len() as i32, &self.qh, ());
            let buffer = pool.create_buffer(
                0,
                memfd.width() as i32,
                memfd.height() as i32,
                (memfd.width() * 4) as i32,
                wl_shm::Format::Xrgb8888,
                &self.qh,
                (),
            );

            if slot.role == SlotRole::Foreground {
                let region = self.compositor.create_region(&self.qh, ());
                region.add(0, 0, slot.logical_width as i32, slot.logical_height as i32);
                slot.surface.set_opaque_region(Some(&region));
                region.destroy();
                if !slot.presented
                    && let Some(presentation) = &self.presentation
                {
                    presentation.feedback(&slot.surface, &self.qh, slot.surface.clone());
                }
            }

            slot.surface.attach(Some(&buffer), 0, 0);
            slot.surface
                .damage_buffer(0, 0, memfd.width() as i32, memfd.height() as i32);
            slot.surface.commit();
            // Send each output as soon as it is ready, including ahead of any image decode on another output.
            let _ = conn.flush();
            trace_startup(
                "shm_commit",
                format_args!(
                    "role={:?} surface={} source={source} buffer={}x{} target={pw}x{ph} load_ms={:.2}",
                    slot.role,
                    slot.surface.id().protocol_id(),
                    memfd.width(),
                    memfd.height(),
                    load_started.elapsed().as_secs_f64() * 1000.0,
                ),
            );

            if let Some(old_buf) = slot.active_buffer.replace(buffer) {
                old_buf.destroy();
            }
            if let Some(old_pool) = slot.active_pool.replace(pool) {
                old_pool.destroy();
            }
        }
    }

    fn dispatch_until(
        &mut self,
        conn: &Connection,
        queue: &mut EventQueue<Self>,
        deadline: Instant,
        ready: impl Fn(&Self) -> bool,
    ) -> Result<bool, String> {
        loop {
            queue
                .dispatch_pending(self)
                .map_err(|err| err.to_string())?;
            self.render_ready_slots(conn);
            if ready(self) {
                return Ok(true);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            let interest = match queue.flush() {
                Ok(()) => PollFlags::IN,
                Err(WaylandError::Io(err)) if err.kind() == ErrorKind::WouldBlock => {
                    PollFlags::IN | PollFlags::OUT
                }
                Err(err) => return Err(err.to_string()),
            };
            let Some(read) = queue.prepare_read() else {
                continue;
            };
            let mut fds = [PollFd::new(queue, interest)];
            let timeout = Timespec::try_from(remaining).map_err(|err| err.to_string())?;
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) => return Ok(false),
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(err) => return Err(err.to_string()),
            }
            if fds[0]
                .revents()
                .intersects(PollFlags::IN | PollFlags::ERR | PollFlags::HUP)
            {
                match read.read() {
                    Ok(_) => {}
                    Err(WaylandError::Io(err)) if err.kind() == ErrorKind::WouldBlock => {}
                    Err(err) => return Err(err.to_string()),
                }
            }
        }
    }

    fn has_buffer(&self, role: SlotRole) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.role == role && slot.active_buffer.is_some())
    }
}

/// Active in-process `wl_shm` splash surfaces kept displayed until EGL Frame 1 presents.
pub(crate) struct ActiveSplash {
    conn: Connection,
    queue: EventQueue<SplashState>,
    state: SplashState,
}

impl ActiveSplash {
    /// Displays `spec` synchronously on `conn` via `wl_shm` before EGL/GL initialization.
    pub(crate) fn show(conn: &Connection, spec: &StartupSplashSpec) -> Option<Self> {
        trace_startup("shm_start", "");
        let have_fg = spec.fg_bin.as_ref().is_some_and(|p| p.is_file())
            || spec.fg_image.as_ref().is_some_and(|p| p.is_file());
        let have_bg = spec.bg_image.as_ref().is_some_and(|p| p.is_file());
        if !have_fg && !have_bg {
            trace_startup("splash_gate", "result=no_media");
            return None;
        }

        let (globals, mut queue) = registry_queue_init::<SplashState>(conn)
            .inspect_err(|err| trace_startup("shm_registry_failed", format_args!("error={err}")))
            .ok()?;
        trace_startup("shm_registry_ready", "");
        let qh = queue.handle();

        let compositor = globals.bind::<WlCompositor, _, _>(&qh, 1..=6, ()).ok()?;
        let shm = globals.bind::<WlShm, _, _>(&qh, 1..=1, ()).ok()?;
        let layer_shell = globals
            .bind::<ZwlrLayerShellV1, _, _>(&qh, 1..=4, ())
            .ok()?;
        let viewporter = globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok();
        let fractional_mgr = globals
            .bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ())
            .ok();
        let presentation = globals.bind::<WpPresentation, _, _>(&qh, 1..=2, ()).ok();

        let mut outputs = Vec::new();
        globals.contents().with_list(|list| {
            for global in list {
                if global.interface == "wl_output" {
                    let proxy: WlOutput =
                        globals
                            .registry()
                            .bind(global.name, global.version.min(4), &qh, ());
                    outputs.push(OutputInfo {
                        proxy,
                        integer_scale: 1,
                    });
                }
            }
        });
        if outputs.is_empty() {
            if let Some(presentation) = presentation {
                presentation.destroy();
            }
            trace_startup("splash_gate", "result=no_output");
            return None;
        }

        let empty_region = Some(compositor.create_region(&qh, ()));
        let mut state = SplashState {
            spec: spec.clone(),
            compositor,
            shm,
            layer_shell,
            viewporter,
            fractional_mgr,
            presentation,
            presentation_clock: None,
            foreground_bitmap: None,
            empty_region,
            qh,
            outputs,
            slots: Vec::new(),
        };

        let role = if have_fg {
            SlotRole::Foreground
        } else {
            SlotRole::Backdrop
        };
        state.spawn_role_slots(role);
        let _ = conn.flush();
        if let Some(bin) = spec.fg_bin.as_deref() {
            let started = Instant::now();
            match load_bgpx_into_memfd(bin) {
                Ok(bitmap) => {
                    trace_startup(
                        "shm_cache",
                        format_args!(
                            "result=hit size={}x{} read_ms={:.2}",
                            bitmap.width(),
                            bitmap.height(),
                            started.elapsed().as_secs_f64() * 1000.0,
                        ),
                    );
                    state.foreground_bitmap = Some(bitmap);
                }
                Err(err) => trace_startup("shm_cache", format_args!("result=fallback error={err}")),
            }
        } else if have_fg {
            trace_startup("shm_cache", "result=unavailable");
        }

        let committed =
            state.dispatch_until(conn, &mut queue, Instant::now() + SPLASH_WAIT, |state| {
                state.has_buffer(role)
            });
        let present_deadline = Instant::now() + SPLASH_WAIT;
        let gate_result = match committed {
            Ok(false) => "buffer_timeout",
            Ok(true) if !have_fg => "backdrop_only",
            Ok(true) if state.presentation.is_none() => "presentation_unsupported",
            Ok(true) => match state.dispatch_until(conn, &mut queue, present_deadline, |state| {
                state.slots.iter().any(|slot| slot.presented)
            }) {
                Ok(true) => "presented",
                Ok(false) => "presentation_timeout",
                Err(err) => {
                    trace_startup("shm_wait_failed", format_args!("error={err}"));
                    "wayland_error"
                }
            },
            Err(err) => {
                trace_startup("shm_wait_failed", format_args!("error={err}"));
                "wayland_error"
            }
        };
        trace_startup("splash_gate", format_args!("result={gate_result}"));

        // Prepare the overview behind an already presented foreground using the remaining wait budget.
        if have_fg && have_bg && matches!(gate_result, "presented" | "presentation_unsupported") {
            state.spawn_role_slots(SlotRole::Backdrop);
            let _ = state.dispatch_until(conn, &mut queue, present_deadline, |state| {
                state.has_buffer(SlotRole::Backdrop)
            });
        }
        let _ = conn.flush();

        Some(Self {
            conn: conn.clone(),
            queue,
            state,
        })
    }

    pub(crate) fn dispatch_pending(&mut self) {
        if self
            .queue
            .dispatch_pending(&mut self.state)
            .is_ok_and(|count| count > 0)
        {
            self.state.render_ready_slots(&self.conn);
        }
    }

    pub(crate) fn retire(mut self) {
        self.cleanup_slots();
    }

    fn cleanup_slots(&mut self) {
        for mut slot in self.state.slots.drain(..) {
            slot.destroy();
        }
        if let Some(region) = self.state.empty_region.take() {
            region.destroy();
        }
        if let Some(presentation) = self.state.presentation.take() {
            presentation.destroy();
        }
        let _ = self.conn.flush();
    }
}

impl Drop for ActiveSplash {
    fn drop(&mut self) {
        self.cleanup_slots();
    }
}

fn same_aspect(first: (u32, u32), second: (u32, u32)) -> bool {
    let (w1, h1) = (u64::from(first.0), u64::from(first.1));
    let (w2, h2) = (u64::from(second.0), u64::from(second.1));
    // Pixel rounding on both axes can differ by one between logical and scaled sizes.
    (w1 * h2).abs_diff(w2 * h1) <= w1.max(w2) + h1.max(h2)
}

fn set_viewport_crop(viewport: &WpViewport, bitmap: &MemfdBuffer, target_w: u32, target_h: u32) {
    let (w, h) = (f64::from(bitmap.width()), f64::from(bitmap.height()));
    let target_w = f64::from(target_w);
    let target_h = f64::from(target_h);
    let scale = (w / target_w).min(h / target_h);
    let crop_w = (target_w * scale).min(w);
    let crop_h = (target_h * scale).min(h);
    viewport.set_source((w - crop_w) * 0.5, (h - crop_h) * 0.5, crop_w, crop_h);
}

fn load_splash_fallback(
    bin_path: Option<&Path>,
    image_path: Option<&Path>,
    target_w: u32,
    target_h: u32,
) -> Result<(MemfdBuffer, &'static str), String> {
    if let Some(ip) = image_path
        && ip.is_file()
    {
        let reader = ImageReader::open(ip)
            .map_err(|e| format!("failed to open {}: {e}", ip.display()))?
            .with_guessed_format()
            .map_err(|e| format!("failed to probe {}: {e}", ip.display()))?;
        let decoded = reader
            .decode()
            .map_err(|e| format!("failed to decode {}: {e}", ip.display()))?
            .to_rgba8();
        let (sw, sh) = decoded.dimensions();
        let mut pixels = Vec::new();
        cover_fill_to_xrgb8888(
            sw,
            sh,
            sw * 4,
            decoded.as_raw(),
            false,
            target_w,
            target_h,
            &mut pixels,
        )
        .map_err(|e| e.to_string())?;
        return write_pixels_to_memfd(target_w, target_h, &pixels)
            .map(|memfd| (memfd, "image"))
            .map_err(|e| e.to_string());
    }

    if let Some(bp) = bin_path
        && let Ok(mut file) = File::open(bp)
        && let Ok((sw, sh)) = read_splash_header(&mut file)
    {
        let src_bytes = (sw as usize).saturating_mul(sh as usize).saturating_mul(4);
        let mut src = vec![0u8; src_bytes];
        if file.read_exact(&mut src).is_ok() {
            let mut pixels = Vec::new();
            cover_fill_to_xrgb8888(sw, sh, sw * 4, &src, true, target_w, target_h, &mut pixels)
                .map_err(|e| e.to_string())?;
            return write_pixels_to_memfd(target_w, target_h, &pixels)
                .map(|memfd| (memfd, "resampled_bgpx"))
                .map_err(|e| e.to_string());
        }
    }

    Err("no readable splash bitmap or fallback image available".into())
}

impl Dispatch<WlRegistry, GlobalListContents> for SplashState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "wl_output"
        {
            let proxy: WlOutput = registry.bind(name, version.min(4), qh, ());
            state.outputs.push(OutputInfo {
                proxy,
                integer_scale: 1,
            });
        }
    }
}

impl Dispatch<WlCompositor, ()> for SplashState {
    fn event(
        _state: &mut Self,
        _proxy: &WlCompositor,
        _event: <WlCompositor as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlRegion, ()> for SplashState {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegion,
        _event: <WlRegion as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShm, ()> for SplashState {
    fn event(
        _state: &mut Self,
        _proxy: &WlShm,
        _event: wl_shm::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for SplashState {
    fn event(
        _state: &mut Self,
        _proxy: &WlShmPool,
        _event: <WlShmPool as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, ()> for SplashState {
    fn event(
        _state: &mut Self,
        _proxy: &WlBuffer,
        _event: wl_buffer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSurface, ()> for SplashState {
    fn event(
        _state: &mut Self,
        _proxy: &WlSurface,
        _event: wl_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlOutput, ()> for SplashState {
    fn event(
        state: &mut Self,
        proxy: &WlOutput,
        event: wl_output::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Scale { factor } = event
            && let Some(out) = state.outputs.iter_mut().find(|o| &o.proxy == proxy)
        {
            out.integer_scale = factor.max(1);
            for slot in &mut state.slots {
                if slot.output == *proxy && !slot.has_preferred_scale {
                    slot.scale_120 = (out.integer_scale as u32) * 120;
                }
            }
        }
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for SplashState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrLayerShellV1,
        _event: <ZwlrLayerShellV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for SplashState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            proxy.ack_configure(serial);
            if let Some(slot) = state.slots.iter_mut().find(|s| &s.layer_surface == proxy) {
                slot.logical_width = width;
                slot.logical_height = height;
                slot.configured = true;
            }
        }
    }
}

impl Dispatch<WpPresentation, ()> for SplashState {
    fn event(
        state: &mut Self,
        _proxy: &WpPresentation,
        event: wp_presentation::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.presentation_clock = Some(clk_id);
        }
    }
}

impl Dispatch<WpPresentationFeedback, WlSurface> for SplashState {
    fn event(
        state: &mut Self,
        _proxy: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        surface: &WlSurface,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let id = surface.id().protocol_id();
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                ..
            } => {
                if let Some(slot) = state.slots.iter_mut().find(|slot| slot.surface == *surface)
                    && !slot.presented
                {
                    slot.presented = true;
                    let seconds = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                    trace_startup(
                        "shm_presented",
                        format_args!(
                            "surface={id} display_time={seconds}.{tv_nsec:09} display_clock={:?}",
                            state.presentation_clock,
                        ),
                    );
                }
            }
            wp_presentation_feedback::Event::Discarded => {
                trace_startup("shm_discarded", format_args!("surface={id}"));
            }
            _ => {}
        }
    }
}

impl Dispatch<WpViewporter, ()> for SplashState {
    fn event(
        _state: &mut Self,
        _proxy: &WpViewporter,
        _event: <WpViewporter as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewport, ()> for SplashState {
    fn event(
        _state: &mut Self,
        _proxy: &WpViewport,
        _event: <WpViewport as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleManagerV1, ()> for SplashState {
    fn event(
        _state: &mut Self,
        _proxy: &WpFractionalScaleManagerV1,
        _event: <WpFractionalScaleManagerV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleV1, WlSurface> for SplashState {
    fn event(
        state: &mut Self,
        _proxy: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        surface: &WlSurface,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(slot) = state.slots.iter_mut().find(|s| &s.surface == surface)
        {
            slot.scale_120 = scale;
            slot.has_preferred_scale = true;
        }
    }
}
