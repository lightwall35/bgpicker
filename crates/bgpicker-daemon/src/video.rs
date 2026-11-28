use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bgpicker_gfx::FrameInput;
use bgpicker_video::{
    CpuPixelFormat, DecodedFrame, DecoderConfig, FrameConsumer, FrameData, HwAccelPreference,
    PlaybackState, VideoPlayer,
};

use crate::renderer::RendererBackend;

/// Initial decoded video frame(s) returned once a stream is ready for presentation.
pub struct VideoReadyFrame {
    pub stream_id: u64,
    pub fg_frame: FrameInput,
    pub bg_frame: Option<FrameInput>,
}

/// Abstract video backend allowing `RealVideoBackend` and deterministic tests.
pub trait VideoBackend: Send + Sync {
    fn open_and_wait(
        &self,
        path: &Path,
        overview_open: bool,
        timeout: Duration,
    ) -> Result<VideoReadyFrame, String>;
    fn activate_stream(&self, stream_id: u64);
    fn stop(&self);
    fn set_overview_open(&self, open: bool);
    fn is_playing_path(&self, path: &Path) -> bool;
    fn shutdown(&self);
}

struct Consumers {
    fg: FrameConsumer,
    bg: FrameConsumer,
}

#[derive(Debug)]
enum PumpSignal {
    Wake,
    Shutdown,
}

struct SharedPump {
    /// Holds the currently committed `stream_id` (0 when inactive).
    /// Locking prevents any stale frame from pushing after `stop()`/`deactivate()`.
    active_stream: Mutex<u64>,
    active_media_key: Mutex<String>,
    consumers: Mutex<Consumers>,
}

struct PlayerState {
    player: VideoPlayer,
    active_path: Option<PathBuf>,
    pump_handle: Option<JoinHandle<()>>,
}

/// Real `bgpicker-video` backend driving one active stream shared across foreground & backdrop.
pub struct RealVideoBackend {
    renderer: Arc<dyn RendererBackend>,
    inner: Mutex<Option<PlayerState>>,
    shared: Mutex<Option<Arc<SharedPump>>>,
    pump_tx: Mutex<Option<Sender<PumpSignal>>>,
}

impl RealVideoBackend {
    pub fn new(renderer: Arc<dyn RendererBackend>) -> Self {
        Self {
            renderer,
            inner: Mutex::new(None),
            shared: Mutex::new(None),
            pump_tx: Mutex::new(None),
        }
    }

    fn ensure_started(&self, overview_open: bool) -> Result<(), String> {
        let mut guard = self.inner.lock().expect("video backend mutex poisoned");
        if guard.is_some() {
            return Ok(());
        }

        let hw_accel = match std::env::var("BGPICKER_VIDEO_HW")
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("cuda-first") => HwAccelPreference::CudaFirst,
            Some("cuda") => HwAccelPreference::CudaOnly,
            Some("vaapi") => HwAccelPreference::VaapiOnly,
            Some("software") => HwAccelPreference::SoftwareOnly,
            Some("auto") => HwAccelPreference::Auto,
            _ => {
                if self.renderer.prefers_cuda_decode() {
                    HwAccelPreference::CudaFirst
                } else {
                    HwAccelPreference::Auto
                }
            }
        };
        let (pump_tx, pump_rx) = mpsc::channel::<PumpSignal>();
        let cb_tx = pump_tx.clone();
        let config = DecoderConfig {
            hw_accel,
            prefer_dmabuf: false,
            cpu_format: CpuPixelFormat::Nv12,
            sw_threads: 0,
            vaapi_device: None,
            looping: true,
            overview_open,
            min_ready_frames: 2,
            backdrop_seed_frames: 4,
            pace_frames: true,
            buffer_pool_size: 8,
            frame_callback: Some(Arc::new(move |_target| {
                let _ = cb_tx.send(PumpSignal::Wake);
            })),
        };

        let mut player = VideoPlayer::new(config).map_err(|e| e.to_string())?;
        let fg = player
            .take_foreground()
            .ok_or_else(|| "missing foreground video consumer".to_string())?;
        let bg = player
            .take_backdrop()
            .ok_or_else(|| "missing backdrop video consumer".to_string())?;

        let shared = Arc::new(SharedPump {
            active_stream: Mutex::new(0),
            active_media_key: Mutex::new(String::new()),
            consumers: Mutex::new(Consumers { fg, bg }),
        });

        let pump_shared = Arc::clone(&shared);
        let pump_renderer = Arc::clone(&self.renderer);
        let pump_handle = thread::Builder::new()
            .name("bgpicker-video-pump".into())
            .spawn(move || run_pump_loop(pump_rx, pump_shared, pump_renderer))
            .map_err(|e| format!("failed to spawn video pump thread: {e}"))?;

        *self.shared.lock().expect("shared pump mutex poisoned") = Some(shared);
        *self.pump_tx.lock().expect("pump tx mutex poisoned") = Some(pump_tx);
        *guard = Some(PlayerState {
            player,
            active_path: None,
            pump_handle: Some(pump_handle),
        });
        Ok(())
    }
}

impl VideoBackend for RealVideoBackend {
    fn open_and_wait(
        &self,
        path: &Path,
        overview_open: bool,
        timeout: Duration,
    ) -> Result<VideoReadyFrame, String> {
        self.ensure_started(overview_open)?;
        let shared = self
            .shared
            .lock()
            .expect("shared pump mutex poisoned")
            .clone()
            .ok_or_else(|| "video pump not initialized".to_string())?;

        // Prevent pump from pushing until caller commits the first frame.
        {
            let mut active = shared.active_stream.lock().expect("active stream lock poisoned");
            *active = 0;
        }

        let media_key = path.to_string_lossy().into_owned();
        {
            let mut key_guard = shared
                .active_media_key
                .lock()
                .expect("media key lock poisoned");
            *key_guard = media_key.clone();
        }

        let stream_id = {
            let mut guard = self.inner.lock().expect("video backend mutex poisoned");
            let st = guard.as_mut().expect("ensured started");
            st.active_path = None;
            st.player.set_overview_open(overview_open);
            st.player.open(path)
        };

        let deadline = Instant::now() + timeout;
        loop {
            let snap = {
                let guard = self.inner.lock().expect("video backend mutex poisoned");
                guard.as_ref().expect("ensured started").player.status()
            };

            if snap.stream_id == stream_id {
                if snap.ready {
                    break;
                }
                if snap.state == PlaybackState::Error {
                    let err = snap
                        .last_error
                        .unwrap_or_else(|| "video decoder error".to_string());
                    self.stop();
                    return Err(err);
                }
            }
            if Instant::now() >= deadline {
                self.stop();
                return Err(format!("timed out waiting for video stream {}", path.display()));
            }
            thread::sleep(Duration::from_millis(10));
        }

        let (fg_decoded, bg_decoded) = {
            let mut cons = shared.consumers.lock().expect("consumers lock poisoned");
            let fg = cons
                .fg
                .latest()
                .filter(|f| f.stream_id == stream_id)
                .ok_or_else(|| "video stream became ready without a foreground frame".to_string())?;
            let bg = cons.bg.latest().filter(|f| f.stream_id == stream_id);
            (fg, bg)
        };

        let fg_frame = decoded_to_frame_input(&media_key, &fg_decoded)?;
        let bg_frame = match bg_decoded {
            Some(ref b) if Arc::ptr_eq(&fg_decoded, b) => None,
            Some(ref b) => Some(decoded_to_frame_input(&media_key, b)?),
            None => None,
        };

        if let Some(st) = self.inner.lock().expect("video backend mutex poisoned").as_mut() {
            st.active_path = Some(path.to_path_buf());
        }

        Ok(VideoReadyFrame {
            stream_id,
            fg_frame,
            bg_frame,
        })
    }

    fn activate_stream(&self, stream_id: u64) {
        if let Some(shared) = self.shared.lock().expect("shared pump mutex poisoned").as_ref() {
            let mut active = shared.active_stream.lock().expect("active stream lock poisoned");
            *active = stream_id;
        }
        if let Some(tx) = self.pump_tx.lock().expect("pump tx mutex poisoned").as_ref() {
            let _ = tx.send(PumpSignal::Wake);
        }
    }

    fn stop(&self) {
        if let Some(shared) = self.shared.lock().expect("shared pump mutex poisoned").as_ref() {
            let mut active = shared.active_stream.lock().expect("active stream lock poisoned");
            *active = 0;
            if let Ok(mut cons) = shared.consumers.lock() {
                cons.fg.clear();
                cons.bg.clear();
            }
        }
        if let Some(st) = self.inner.lock().expect("video backend mutex poisoned").as_mut() {
            st.active_path = None;
            st.player.stop();
        }
    }

    fn set_overview_open(&self, open: bool) {
        if let Some(st) = self.inner.lock().expect("video backend mutex poisoned").as_ref() {
            st.player.set_overview_open(open);
        }
    }

    fn is_playing_path(&self, path: &Path) -> bool {
        let guard = self.inner.lock().expect("video backend mutex poisoned");
        let Some(st) = guard.as_ref() else {
            return false;
        };
        st.active_path.as_deref() == Some(path) && st.player.is_ready()
    }

    fn shutdown(&self) {
        self.stop();
        if let Some(tx) = self.pump_tx.lock().expect("pump tx mutex poisoned").take() {
            let _ = tx.send(PumpSignal::Shutdown);
        }
        if let Some(mut st) = self.inner.lock().expect("video backend mutex poisoned").take() {
            if let Some(handle) = st.pump_handle.take() {
                let _ = handle.join();
            }
            st.player.shutdown();
        }
        *self.shared.lock().expect("shared pump mutex poisoned") = None;
    }
}

impl Drop for RealVideoBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_pump_loop(
    rx: mpsc::Receiver<PumpSignal>,
    shared: Arc<SharedPump>,
    renderer: Arc<dyn RendererBackend>,
) {
    while let Ok(sig) = rx.recv() {
        let mut should_exit = matches!(sig, PumpSignal::Shutdown);
        while let Ok(more) = rx.try_recv() {
            if matches!(more, PumpSignal::Shutdown) {
                should_exit = true;
            }
        }
        if should_exit {
            break;
        }

        let active_guard = shared.active_stream.lock().expect("active stream lock poisoned");
        let active_id = *active_guard;
        if active_id == 0 {
            continue;
        }
        let media_key = shared
            .active_media_key
            .lock()
            .expect("media key lock poisoned")
            .clone();

        let (fg_opt, bg_opt) = {
            let mut cons = shared.consumers.lock().expect("consumers lock poisoned");
            (
                cons.fg.poll().filter(|f| f.stream_id == active_id),
                cons.bg.poll().filter(|f| f.stream_id == active_id),
            )
        };

        if let Some(fg) = fg_opt {
            let sync_backdrop = bg_opt
                .as_ref()
                .is_some_and(|bg| Arc::ptr_eq(&fg, bg) || bg.frame_seq == fg.frame_seq);
            if let Ok(input) = decoded_to_frame_input(&media_key, &fg) {
                let _ = renderer.push_video_frame(input, sync_backdrop);
            }
            if !sync_backdrop {
                if let Some(bg) = bg_opt {
                    if let Ok(bg_input) = decoded_to_frame_input(&media_key, &bg) {
                        let _ = renderer.push_video_frame(bg_input, true);
                    }
                }
            }
        } else if let Some(bg) = bg_opt {
            if let Ok(bg_input) = decoded_to_frame_input(&media_key, &bg) {
                let _ = renderer.push_video_frame(bg_input, true);
            }
        }
    }
}

/// Converts a CPU-resident [`DecodedFrame`] into a [`FrameInput`] suitable for `bgpicker-gfx`.
pub fn decoded_to_frame_input(media_key: &str, frame: &DecodedFrame) -> Result<FrameInput, String> {
    let FrameData::Cpu(cpu) = &frame.data else {
        return Err("DMA-BUF frames require CPU upload when feeding FrameInput".into());
    };
    let generation = frame.frame_seq;
    match cpu.format {
        CpuPixelFormat::Nv12 => {
            let y_stride = cpu.planes.first().map(|p| p.stride).unwrap_or(cpu.width);
            let uv_stride = cpu.planes.get(1).map(|p| p.stride).unwrap_or(y_stride);
            let min_uv = cpu.width.div_ceil(2) * 2;
            if uv_stride >= min_uv {
                let raw: Arc<[u8]> = Arc::from(cpu.as_bytes());
                FrameInput::from_nv12(
                    media_key,
                    generation,
                    cpu.width,
                    cpu.height,
                    y_stride,
                    uv_stride,
                    raw,
                )
                .map_err(|e| e.to_string())
            } else {
                // Handle odd-width NV12 safely by padding UV rows to even `min_uv`.
                let uv_rows = cpu.height.div_ceil(2) as usize;
                let y_bytes = (y_stride as usize) * (cpu.height as usize);
                let mut padded = Vec::with_capacity(y_bytes + (min_uv as usize) * uv_rows);
                let y_src = cpu.plane_bytes(0).unwrap_or(cpu.as_bytes());
                padded.extend_from_slice(&y_src[..y_bytes]);
                let uv_src = cpu.plane_bytes(1).unwrap_or(&[]);
                for r in 0..uv_rows {
                    let row_start = r * (uv_stride as usize);
                    let row_end = (row_start + (uv_stride as usize)).min(uv_src.len());
                    padded.extend_from_slice(&uv_src[row_start..row_end]);
                    let missing = (min_uv as usize).saturating_sub(row_end - row_start);
                    padded.resize(padded.len() + missing, 128);
                }
                let raw: Arc<[u8]> = Arc::from(padded.into_boxed_slice());
                FrameInput::from_nv12(
                    media_key,
                    generation,
                    cpu.width,
                    cpu.height,
                    y_stride,
                    min_uv,
                    raw,
                )
                .map_err(|e| e.to_string())
            }
        }
        CpuPixelFormat::Rgba8 => {
            let stride = cpu.planes.first().map(|p| p.stride).unwrap_or(cpu.width * 4);
            let raw: Arc<[u8]> = Arc::from(cpu.as_bytes());
            FrameInput::from_rgba(media_key, generation, cpu.width, cpu.height, Some(stride), raw)
                .map_err(|e| e.to_string())
        }
        CpuPixelFormat::Bgra8 => {
            let stride = cpu.planes.first().map(|p| p.stride).unwrap_or(cpu.width * 4);
            let raw: Arc<[u8]> = Arc::from(cpu.as_bytes());
            FrameInput::from_bgra(media_key, generation, cpu.width, cpu.height, Some(stride), raw)
                .map_err(|e| e.to_string())
        }
    }
}
