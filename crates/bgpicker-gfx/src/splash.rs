//! In-process Wayland `wl_shm` boot splash rendered before EGL/GL initialization.
//!
//! Renders pre-rendered `BGPX` XRGB8888 buffers (`startup-splash-fg.bin`) or
//! decoded fallback images onto temporary `Layer::Background` surfaces before
//! `EglContext::new_wayland` runs, then destroys those temporary surfaces once
//! `bgpicker:wallpaper` presents its first real compositor-acknowledged EGL frame.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bgpicker_core::{
    MemfdBuffer, compute_scaled_size_120, cover_fill_to_xrgb8888, read_splash_header,
    try_load_bgpx_exact_into_memfd, write_pixels_to_memfd,
};
use image::ImageReader;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
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
    surface: WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    viewport: Option<WpViewport>,
    fractional_scale: Option<WpFractionalScaleV1>,
    logical_width: u32,
    logical_height: u32,
    scale_120: u32,
    configured: bool,
    rendered_size: Option<(u32, u32)>,
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
        let fractional_scale =
            fractional_mgr.map(|mgr| mgr.get_fractional_scale(&surface, qh, surface.clone()));

        if viewport.is_some() {
            surface.set_buffer_scale(1);
        } else {
            surface.set_buffer_scale(initial_scale.max(1));
        }

        let ns = match role {
            SlotRole::Foreground => FG_NAMESPACE,
            SlotRole::Backdrop => BG_NAMESPACE,
        };
        let layer_surface =
            layer_shell.get_layer_surface(&surface, Some(output), Layer::Background, ns.into(), qh, ());
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer_surface.set_size(0, 0);
        surface.commit();

        Self {
            role,
            surface,
            layer_surface,
            viewport,
            fractional_scale,
            logical_width: 0,
            logical_height: 0,
            scale_120: (initial_scale.max(1) as u32) * 120,
            configured: false,
            rendered_size: None,
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
    empty_region: Option<WlRegion>,
    qh: QueueHandle<Self>,
    outputs: Vec<OutputInfo>,
    output_indices_by_name: HashMap<u32, usize>,
    slots: Vec<SplashSlot>,
    foreground_rendered: bool,
    backdrop_spawned: bool,
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
        let mut newly_committed_fg = false;
        for slot in &mut self.slots {
            if !slot.configured || slot.logical_width == 0 || slot.logical_height == 0 {
                continue;
            }
            let (pw, ph) =
                compute_scaled_size_120(slot.logical_width, slot.logical_height, slot.scale_120);
            if slot.rendered_size == Some((pw, ph)) {
                continue;
            }

            let memfd = match slot.role {
                SlotRole::Foreground => load_splash_into_memfd(
                    self.spec.fg_bin.as_deref(),
                    self.spec.fg_image.as_deref(),
                    pw,
                    ph,
                ),
                SlotRole::Backdrop => {
                    load_splash_into_memfd(None, self.spec.bg_image.as_deref(), pw, ph)
                }
            };

            let Ok(memfd) = memfd else {
                slot.rendered_size = Some((pw, ph));
                continue;
            };

            if let Some(ref vp) = slot.viewport {
                slot.surface.set_buffer_scale(1);
                vp.set_destination(slot.logical_width as i32, slot.logical_height as i32);
            } else {
                let int_scale = (slot.scale_120 / 120).max(1) as i32;
                slot.surface.set_buffer_scale(int_scale);
            }

            let pool = self
                .shm
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
            }

            slot.surface.attach(Some(&buffer), 0, 0);
            slot.surface.damage_buffer(0, 0, pw as i32, ph as i32);
            slot.surface.commit();

            if let Some(old_buf) = slot.active_buffer.replace(buffer) {
                old_buf.destroy();
            }
            if let Some(old_pool) = slot.active_pool.replace(pool) {
                old_pool.destroy();
            }
            slot.rendered_size = Some((pw, ph));
            if slot.role == SlotRole::Foreground {
                newly_committed_fg = true;
            }
        }

        if newly_committed_fg {
            self.foreground_rendered = true;
            let _ = conn.flush();
            if !self.backdrop_spawned
                && self.spec.bg_image.as_ref().is_some_and(|p| p.is_file())
            {
                self.backdrop_spawned = true;
                self.spawn_role_slots(SlotRole::Backdrop);
                let _ = conn.flush();
            }
        }
    }
}

/// Active in-process `wl_shm` splash surfaces kept displayed until EGL Frame 1 presents.
pub(crate) struct ActiveSplash {
    conn: Connection,
    _queue: EventQueue<SplashState>,
    state: SplashState,
}

impl ActiveSplash {
    /// Displays `spec` synchronously on `conn` via `wl_shm` before EGL/GL initialization.
    pub(crate) fn show(conn: &Connection, spec: &StartupSplashSpec) -> Option<Self> {
        let have_fg = spec.fg_bin.as_ref().is_some_and(|p| p.is_file())
            || spec.fg_image.as_ref().is_some_and(|p| p.is_file());
        let have_bg = spec.bg_image.as_ref().is_some_and(|p| p.is_file());
        if !have_fg && !have_bg {
            return None;
        }

        let (globals, mut queue) = registry_queue_init::<SplashState>(conn).ok()?;
        let qh = queue.handle();

        let compositor = globals.bind::<WlCompositor, _, _>(&qh, 1..=6, ()).ok()?;
        let shm = globals.bind::<WlShm, _, _>(&qh, 1..=1, ()).ok()?;
        let layer_shell = globals.bind::<ZwlrLayerShellV1, _, _>(&qh, 1..=4, ()).ok()?;
        let viewporter = globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok();
        let fractional_mgr = globals
            .bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ())
            .ok();

        let mut outputs = Vec::new();
        let mut output_indices_by_name = HashMap::new();
        globals.contents().with_list(|list| {
            for global in list {
                if global.interface == "wl_output" {
                    let proxy: WlOutput =
                        globals
                            .registry()
                            .bind(global.name, global.version.min(4), &qh, ());
                    let idx = outputs.len();
                    outputs.push(OutputInfo {
                        proxy,
                        integer_scale: 1,
                    });
                    output_indices_by_name.insert(global.name, idx);
                }
            }
        });

        let empty_region = Some(compositor.create_region(&qh, ()));
        let mut state = SplashState {
            spec: spec.clone(),
            compositor,
            shm,
            layer_shell,
            viewporter,
            fractional_mgr,
            empty_region,
            qh,
            outputs,
            output_indices_by_name,
            slots: Vec::new(),
            foreground_rendered: false,
            backdrop_spawned: false,
        };

        let _ = queue.roundtrip(&mut state);
        if state.outputs.is_empty() {
            return None;
        }

        if have_fg {
            state.spawn_role_slots(SlotRole::Foreground);
        } else if have_bg {
            state.backdrop_spawned = true;
            state.spawn_role_slots(SlotRole::Backdrop);
        }

        // Bound roundtrips so Frame 1 reaches the compositor fast without stalling headless boots.
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            if queue.roundtrip(&mut state).is_err() {
                break;
            }
            if (!have_fg || state.foreground_rendered)
                && (!have_bg
                    || state
                        .slots
                        .iter()
                        .filter(|s| s.role == SlotRole::Backdrop)
                        .all(|s| s.rendered_size.is_some()))
            {
                break;
            }
        }
        let _ = conn.flush();

        Some(Self {
            conn: conn.clone(),
            _queue: queue,
            state,
        })
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
        let _ = self.conn.flush();
    }
}

impl Drop for ActiveSplash {
    fn drop(&mut self) {
        self.cleanup_slots();
    }
}

fn load_splash_into_memfd(
    bin_path: Option<&Path>,
    image_path: Option<&Path>,
    target_w: u32,
    target_h: u32,
) -> Result<MemfdBuffer, String> {
    if let Some(bp) = bin_path
        && let Ok(Some(buf)) = try_load_bgpx_exact_into_memfd(bp, target_w, target_h)
    {
        return Ok(buf);
    }

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
        return write_pixels_to_memfd(target_w, target_h, &pixels).map_err(|e| e.to_string());
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
            return write_pixels_to_memfd(target_w, target_h, &pixels).map_err(|e| e.to_string());
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
            let idx = state.outputs.len();
            state.outputs.push(OutputInfo {
                proxy,
                integer_scale: 1,
            });
            state.output_indices_by_name.insert(name, idx);
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
        conn: &Connection,
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
            state.render_ready_slots(conn);
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
        conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let mut changed = false;
            if let Some(slot) = state.slots.iter_mut().find(|s| &s.surface == surface)
                && slot.scale_120 != scale
            {
                slot.scale_120 = scale;
                changed = true;
            }
            if changed {
                state.render_ready_slots(conn);
            }
        }
    }
}
