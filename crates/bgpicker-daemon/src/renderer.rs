use std::path::Path;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bgpicker_core::{ParallaxSettings as CoreParallaxSettings, ResolvedTransition};
use bgpicker_gfx::{
    FrameInput, FrameModeTargets, KawaseSettings, LayerTargets, OutputSnapshot,
    ParallaxSettings as GfxParallaxSettings, RenderCommand, RendererHandle, StartupSplashSpec,
    TransitionEffect, TransitionSpec, WaylandRenderer,
};

/// Converts core `ParallaxSettings` to `bgpicker-gfx` `ParallaxSettings`.
pub fn to_gfx_parallax(p: CoreParallaxSettings) -> GfxParallaxSettings {
    GfxParallaxSettings {
        zoom: p.zoom as f32,
        duration_ms: p.duration_ms,
        mode_transition_ms: p.mode_transition_ms,
        transition_ms: p.transition_ms,
        max_texture_size: p.max_texture_size,
    }
    .clamped()
}

/// Converts core `ResolvedTransition` to `bgpicker-gfx` `TransitionSpec`.
pub fn to_gfx_transition(t: ResolvedTransition) -> TransitionSpec {
    let effect = match t.transition_type {
        bgpicker_core::TransitionType::Fade | bgpicker_core::TransitionType::Random => {
            TransitionEffect::Fade
        }
        bgpicker_core::TransitionType::Wipe => TransitionEffect::Wipe,
        bgpicker_core::TransitionType::Wave => TransitionEffect::Wave,
        bgpicker_core::TransitionType::Grow => TransitionEffect::Grow,
        bgpicker_core::TransitionType::Outer => TransitionEffect::Outer,
        bgpicker_core::TransitionType::None => TransitionEffect::None,
    };
    TransitionSpec {
        effect,
        duration_ms: t.duration_ms,
        angle_deg: t.angle as f32,
        position: [t.position[0] as f32, t.position[1] as f32],
        wave: [t.wave[0] as f32, t.wave[1] as f32],
    }
    .clamped()
}

/// Abstract renderer backend allowing live Wayland+EGL rendering and deterministic headless tests.
pub trait RendererBackend: Send + Sync {
    fn decode_image(&self, path: &Path, max_texture_size: u32) -> Result<FrameInput, String>;
    fn apply_frame(
        &self,
        frame: FrameInput,
        transition: TransitionSpec,
        targets: LayerTargets,
        modes: FrameModeTargets,
    ) -> Result<(), String>;
    fn push_video_frame(&self, frame: FrameInput, sync_backdrop: bool) -> Result<(), String>;
    fn set_progress(
        &self,
        output: Option<String>,
        progress: f32,
        duration_ms: Option<u32>,
    ) -> Result<(), String>;
    fn set_mode(
        &self,
        parallax: bool,
        duration_ms: Option<u32>,
        targets: LayerTargets,
    ) -> Result<(), String>;
    fn update_settings(
        &self,
        parallax: GfxParallaxSettings,
        kawase: KawaseSettings,
    ) -> Result<(), String>;
    fn trim_idle(&self) -> Result<(), String>;
    fn clear(&self) -> Result<(), String>;
    fn wait_presented(&self, _timeout: Duration) {}
    fn prefers_cuda_decode(&self) -> bool {
        false
    }
    fn shutdown(&self);
    fn snapshots(&self) -> Vec<OutputSnapshot>;
}

/// Live Wayland + EGL `bgpicker-gfx` backend running `WaylandRenderer` on a dedicated thread.
pub struct GfxRendererBackend {
    handle: RendererHandle,
    thread: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl GfxRendererBackend {
    /// Connects to Wayland (`backdrop_enabled = true`) on a dedicated `bgpicker-wayland` thread
    /// and waits for connection/EGL initialization before returning.
    pub fn connect() -> Result<Self, String> {
        Self::connect_with_splash(None)
    }

    /// Connects to Wayland (`backdrop_enabled = true`), optionally presenting an in-process
    /// pre-EGL `wl_shm` boot splash until the real EGL surface presents its first frame.
    pub fn connect_with_splash(splash: Option<StartupSplashSpec>) -> Result<Self, String> {
        let (init_tx, init_rx) = mpsc::channel::<Result<RendererHandle, String>>();
        let handle_thread = thread::Builder::new()
            .name("bgpicker-wayland".into())
            .spawn(move || match WaylandRenderer::connect_with_splash(true, splash) {
                Ok(renderer) => {
                    let handle = renderer.handle();
                    let _ = init_tx.send(Ok(handle));
                    if let Err(err) = renderer.run() {
                        log::error!("Wayland renderer loop exited with error: {err}");
                    }
                }
                Err(err) => {
                    let _ = init_tx.send(Err(err.to_string()));
                }
            })
            .map_err(|e| format!("failed to spawn Wayland thread: {e}"))?;

        match init_rx.recv() {
            Ok(Ok(handle)) => Ok(Self {
                handle,
                thread: std::sync::Mutex::new(Some(handle_thread)),
            }),
            Ok(Err(err)) => {
                let _ = handle_thread.join();
                Err(err)
            }
            Err(_) => {
                let _ = handle_thread.join();
                Err("Wayland initialization thread exited unexpectedly".into())
            }
        }
    }
}

impl RendererBackend for GfxRendererBackend {
    fn decode_image(&self, path: &Path, max_texture_size: u32) -> Result<FrameInput, String> {
        FrameInput::from_image_path(path, max_texture_size).map_err(|e| e.to_string())
    }

    fn apply_frame(
        &self,
        frame: FrameInput,
        transition: TransitionSpec,
        targets: LayerTargets,
        modes: FrameModeTargets,
    ) -> Result<(), String> {
        self.handle
            .send(RenderCommand::ApplyFrame {
                frame,
                transition,
                targets,
                modes,
            })
            .map_err(|e| e.to_string())
    }

    fn push_video_frame(&self, frame: FrameInput, sync_backdrop: bool) -> Result<(), String> {
        self.handle
            .send(RenderCommand::PushVideoFrame {
                frame,
                sync_backdrop,
            })
            .map_err(|e| e.to_string())
    }

    fn set_progress(
        &self,
        output: Option<String>,
        progress: f32,
        duration_ms: Option<u32>,
    ) -> Result<(), String> {
        self.handle
            .send(RenderCommand::SetProgress {
                output,
                progress,
                duration_ms,
            })
            .map_err(|e| e.to_string())
    }

    fn set_mode(
        &self,
        parallax: bool,
        duration_ms: Option<u32>,
        targets: LayerTargets,
    ) -> Result<(), String> {
        self.handle
            .send(RenderCommand::SetMode {
                parallax,
                duration_ms,
                targets,
            })
            .map_err(|e| e.to_string())
    }

    fn update_settings(
        &self,
        parallax: GfxParallaxSettings,
        kawase: KawaseSettings,
    ) -> Result<(), String> {
        self.handle
            .send(RenderCommand::UpdateSettings { parallax, kawase })
            .map_err(|e| e.to_string())
    }

    fn trim_idle(&self) -> Result<(), String> {
        self.handle
            .send(RenderCommand::TrimIdle)
            .map_err(|e| e.to_string())
    }

    fn clear(&self) -> Result<(), String> {
        self.handle
            .send(RenderCommand::Clear)
            .map_err(|e| e.to_string())
    }

    fn wait_presented(&self, timeout: Duration) {
        self.handle.wait_presented(timeout);
    }

    fn prefers_cuda_decode(&self) -> bool {
        self.handle.prefers_cuda_decode()
    }

    fn shutdown(&self) {
        let _ = self.handle.send(RenderCommand::Shutdown);
        if let Ok(mut guard) = self.thread.lock() {
            if let Some(join) = guard.take() {
                let _ = join.join();
            }
        }
    }

    fn snapshots(&self) -> Vec<OutputSnapshot> {
        self.handle.snapshots()
    }
}

impl Drop for GfxRendererBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}
