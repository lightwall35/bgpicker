//! Minimal pure-Rust Wayland `zwlr_layer_shell_v1` + `wl_shm` boot placeholder binary.
//!
//! Renders the foreground surface (`bgpick-fg`) immediately on first compositor configure,
//! signals readiness on `--ready-fd`, lazily renders optional overview backdrop (`wallpaper`),
//! and sleeps at 0% CPU until terminated cleanly via `SIGTERM`/`SIGINT` once `bgpicker-daemon`
//! takes over.

use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicI32, Ordering};

use bgpicker_core::{
    compute_scaled_size_120, resolve_project_dir, resolve_startup_placeholders,
    try_acquire_splash_lock, BgPickerPaths, EngineKind, IpcClient, PersistedState, Request,
};
use bgpicker_splash::{load_splash_into_memfd, parse_splash_args, SplashOptions};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, QueueHandle};
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

static SIGNAL_PIPE_WR: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_term_signal(_sig: libc::c_int) {
    let fd = SIGNAL_PIPE_WR.load(Ordering::Acquire);
    if fd >= 0 {
        let byte: u8 = 1;
        unsafe {
            let _ = libc::write(fd, (&raw const byte).cast(), 1);
        }
    }
}

struct SignalSelfPipe {
    read_fd: OwnedFd,
    _write_fd: OwnedFd,
}

impl SignalSelfPipe {
    fn install() -> std::io::Result<Self> {
        let mut fds = [-1_i32; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        SIGNAL_PIPE_WR.store(fds[1], Ordering::Release);

        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_term_signal as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = libc::SA_RESTART;
            libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        }
        Ok(Self {
            read_fd,
            _write_fd: write_fd,
        })
    }
}

impl Drop for SignalSelfPipe {
    fn drop(&mut self) {
        SIGNAL_PIPE_WR.store(-1, Ordering::Release);
    }
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

struct LayerSlot {
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

impl LayerSlot {
    #[allow(clippy::too_many_arguments)]
    fn create(
        compositor: &WlCompositor,
        layer_shell: &ZwlrLayerShellV1,
        viewporter: Option<&WpViewporter>,
        fractional_mgr: Option<&WpFractionalScaleManagerV1>,
        empty_region: Option<&WlRegion>,
        qh: &QueueHandle<SplashApp>,
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

struct SplashApp {
    opts: SplashOptions,
    compositor: WlCompositor,
    shm: WlShm,
    layer_shell: ZwlrLayerShellV1,
    viewporter: Option<WpViewporter>,
    fractional_mgr: Option<WpFractionalScaleManagerV1>,
    empty_region: Option<WlRegion>,
    qh: QueueHandle<Self>,

    outputs: Vec<OutputInfo>,
    output_indices_by_name: HashMap<u32, usize>,
    slots: Vec<LayerSlot>,

    foreground_committed: bool,
    backdrop_spawned: bool,
    ready_notified: bool,
    exiting: bool,
}

impl SplashApp {
    fn signal_ready(&mut self) {
        if self.ready_notified {
            return;
        }
        self.ready_notified = true;
        if let Some(fd) = self.opts.ready_fd.take() {
            unsafe {
                let b: u8 = 1;
                let _ = libc::write(fd, (&raw const b).cast(), 1);
                let _ = libc::close(fd);
            }
        }
    }

    fn spawn_role_slots(&mut self, role: SlotRole) {
        for out in &self.outputs {
            let slot = LayerSlot::create(
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
                    self.opts.fg_bin.as_deref(),
                    self.opts.fg_image.as_deref(),
                    pw,
                    ph,
                ),
                SlotRole::Backdrop => {
                    load_splash_into_memfd(None, self.opts.bg_image.as_deref(), pw, ph)
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
            let _ = conn.flush();
            self.foreground_committed = true;
            self.signal_ready();

            if !self.backdrop_spawned && self.opts.bg_image.as_ref().is_some_and(|p| p.is_file()) {
                self.backdrop_spawned = true;
                self.spawn_role_slots(SlotRole::Backdrop);
                let _ = conn.flush();
            }
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for SplashApp {
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

impl Dispatch<WlCompositor, ()> for SplashApp {
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

impl Dispatch<WlRegion, ()> for SplashApp {
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

impl Dispatch<WlShm, ()> for SplashApp {
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

impl Dispatch<WlShmPool, ()> for SplashApp {
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

impl Dispatch<WlBuffer, ()> for SplashApp {
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

impl Dispatch<WlSurface, ()> for SplashApp {
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

impl Dispatch<WlOutput, ()> for SplashApp {
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

impl Dispatch<ZwlrLayerShellV1, ()> for SplashApp {
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

impl Dispatch<ZwlrLayerSurfaceV1, ()> for SplashApp {
    fn event(
        state: &mut Self,
        proxy: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                proxy.ack_configure(serial);
                if let Some(slot) = state.slots.iter_mut().find(|s| &s.layer_surface == proxy) {
                    slot.logical_width = width;
                    slot.logical_height = height;
                    slot.configured = true;
                }
                state.render_ready_slots(conn);
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.exiting = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<WpViewporter, ()> for SplashApp {
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

impl Dispatch<WpViewport, ()> for SplashApp {
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

impl Dispatch<WpFractionalScaleManagerV1, ()> for SplashApp {
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

impl Dispatch<WpFractionalScaleV1, WlSurface> for SplashApp {
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

fn main() {
    let mut opts = match parse_splash_args(std::env::args().skip(1)) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("bgpicker-splash: {msg}");
            std::process::exit(2);
        }
    };

    let paths = BgPickerPaths::from_env();
    let _splash_lock = if opts.daemon {
        let persisted = PersistedState::load(&paths);
        if !persisted.startup_placeholder || persisted.engine == EngineKind::Simple {
            return;
        }
        let sock = paths.daemon_socket_path();
        if sock.exists() && IpcClient::new(sock).call(&Request::Ping).is_ok() {
            return;
        }
        let Some(mut lock) = try_acquire_splash_lock(&paths) else {
            return;
        };
        let project_dir = resolve_project_dir(opts.project_dir.take());
        let (fg_img, bg_img) = resolve_startup_placeholders(&paths, &project_dir, &persisted);
        if opts.fg_bin.is_none() && persisted.placeholder_cache {
            let bin = paths.startup_splash_fg_bin_file();
            if bin.is_file() {
                opts.fg_bin = Some(bin);
            }
        }
        if opts.fg_image.is_none() {
            opts.fg_image = fg_img;
        }
        if opts.bg_image.is_none() {
            opts.bg_image = bg_img;
        }
        if opts.fg_bin.is_none() && opts.fg_image.is_none() && opts.bg_image.is_none() {
            return;
        }
        lock.record_pid(&paths.startup_placeholder_front_pid_file(), std::process::id());
        Some(lock)
    } else {
        // Hold exclusive advisory lock if available so concurrent `--daemon` startup exits quietly.
        try_acquire_splash_lock(&paths)
    };

    let sig_pipe = match SignalSelfPipe::install() {
        Ok(p) => p,
        Err(err) => {
            eprintln!("bgpicker-splash: signal pipe failed: {err}");
            std::process::exit(1);
        }
    };

    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(err) => {
            eprintln!("bgpicker-splash: Wayland connect failed: {err}");
            std::process::exit(1);
        }
    };

    let (globals, mut queue) = match registry_queue_init::<SplashApp>(&conn) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("bgpicker-splash: Wayland registry failed: {err}");
            std::process::exit(1);
        }
    };
    let qh = queue.handle();

    let Ok(compositor) = globals.bind::<WlCompositor, _, _>(&qh, 1..=6, ()) else {
        eprintln!("bgpicker-splash: missing wl_compositor");
        std::process::exit(1);
    };
    let Ok(shm) = globals.bind::<WlShm, _, _>(&qh, 1..=1, ()) else {
        eprintln!("bgpicker-splash: missing wl_shm");
        std::process::exit(1);
    };
    let Ok(layer_shell) = globals.bind::<ZwlrLayerShellV1, _, _>(&qh, 1..=4, ()) else {
        eprintln!("bgpicker-splash: missing zwlr_layer_shell_v1");
        std::process::exit(1);
    };
    let viewporter = globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok();
    let fractional_mgr = globals
        .bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ())
        .ok();

    let mut outputs = Vec::new();
    let mut output_indices_by_name = HashMap::new();
    globals.contents().with_list(|list| {
        for global in list {
            if global.interface == "wl_output" {
                let proxy: WlOutput = globals.registry().bind(global.name, global.version.min(4), &qh, ());
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
    let have_fg = opts.fg_bin.as_ref().is_some_and(|p| p.is_file())
        || opts.fg_image.as_ref().is_some_and(|p| p.is_file());

    let mut app = SplashApp {
        opts,
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
        foreground_committed: false,
        backdrop_spawned: false,
        ready_notified: false,
        exiting: false,
    };

    // Roundtrip so initial `wl_output.scale` arrives before layer creation.
    let _ = queue.roundtrip(&mut app);

    if have_fg {
        app.spawn_role_slots(SlotRole::Foreground);
    } else if app.opts.bg_image.as_ref().is_some_and(|p| p.is_file()) {
        app.backdrop_spawned = true;
        app.spawn_role_slots(SlotRole::Backdrop);
    }

    let wl_fd = conn.as_fd().as_raw_fd();
    let sig_fd = sig_pipe.read_fd.as_raw_fd();

    loop {
        if queue.dispatch_pending(&mut app).is_err() || app.exiting {
            break;
        }
        let _ = conn.flush();

        if !app.foreground_committed && !have_fg {
            app.signal_ready();
        }

        let Some(guard) = conn.prepare_read() else {
            continue;
        };
        let mut pfds = [
            libc::pollfd {
                fd: wl_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: sig_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let rc = unsafe { libc::poll(pfds.as_mut_ptr(), 2, -1) };
        if rc < 0 {
            drop(guard);
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if (pfds[1].revents & libc::POLLIN) != 0 {
            drop(guard);
            app.exiting = true;
            break;
        }
        if (pfds[0].revents & libc::POLLIN) != 0 {
            let _ = guard.read();
        } else {
            drop(guard);
        }
    }

    app.signal_ready();
    for mut slot in app.slots.drain(..) {
        slot.destroy();
    }
    let _ = conn.flush();
}
