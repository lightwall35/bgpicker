use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use crate::error::VideoError;
use crate::ffi::{DecodeStep, RawDecoder, RawOpenParams};
use crate::frame::{
    BufferPool, CpuPixelFormat, DecodedFrame, FrameTarget, HwAccelPreference, StreamInfo,
};
use crate::triple_buffer::{triple_buffer, TripleConsumer, TripleProducer};

/// Configuration for [`VideoPlayer`].
#[derive(Clone)]
pub struct DecoderConfig {
    pub hw_accel: HwAccelPreference,
    pub prefer_dmabuf: bool,
    pub cpu_format: CpuPixelFormat,
    pub sw_threads: usize,
    pub vaapi_device: Option<String>,
    pub looping: bool,
    pub overview_open: bool,
    pub min_ready_frames: u64,
    pub backdrop_seed_frames: u32,
    pub pace_frames: bool,
    pub buffer_pool_size: usize,
    pub frame_callback: Option<Arc<dyn Fn(FrameTarget) + Send + Sync>>,
}

impl std::fmt::Debug for DecoderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecoderConfig")
            .field("hw_accel", &self.hw_accel)
            .field("prefer_dmabuf", &self.prefer_dmabuf)
            .field("cpu_format", &self.cpu_format)
            .field("sw_threads", &self.sw_threads)
            .field("vaapi_device", &self.vaapi_device)
            .field("looping", &self.looping)
            .field("overview_open", &self.overview_open)
            .field("min_ready_frames", &self.min_ready_frames)
            .field("backdrop_seed_frames", &self.backdrop_seed_frames)
            .field("pace_frames", &self.pace_frames)
            .field("buffer_pool_size", &self.buffer_pool_size)
            .field("has_frame_callback", &self.frame_callback.is_some())
            .finish()
    }
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            hw_accel: HwAccelPreference::Auto,
            prefer_dmabuf: true,
            cpu_format: CpuPixelFormat::Nv12,
            sw_threads: 0,
            vaapi_device: None,
            looping: true,
            overview_open: false,
            min_ready_frames: 2,
            backdrop_seed_frames: 4,
            pace_frames: true,
            buffer_pool_size: 8,
            frame_callback: None,
        }
    }
}

/// High-level lifecycle state of the background video decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Idle,
    Opening,
    Playing,
    Paused,
    Ended,
    Error,
}

/// Snapshot of player state and telemetry safe to read from the render thread.
#[derive(Debug, Clone)]
pub struct PlayerStatusSnapshot {
    pub state: PlaybackState,
    pub stream_id: u64,
    pub ready: bool,
    pub overview_open: bool,
    pub paused: bool,
    pub looping: bool,
    pub frames_decoded: u64,
    pub foreground_frames_published: u64,
    pub backdrop_frames_published: u64,
    pub position_ms: u64,
    pub stream_info: Option<StreamInfo>,
    pub last_error: Option<String>,
    pub worker_thread_id: Option<ThreadId>,
    pub teardown_thread_id: Option<ThreadId>,
}

/// Lock-free consumer handle for either the foreground or backdrop layer.
pub struct FrameConsumer {
    rx: TripleConsumer<Arc<DecodedFrame>>,
    role: FrameTarget,
}

impl FrameConsumer {
    /// Returns the role of this consumer (`Foreground` or `Backdrop`).
    pub fn role(&self) -> FrameTarget {
        self.role
    }

    /// Returns `true` if an unpolled frame is waiting in the triple buffer.
    pub fn has_pending(&self) -> bool {
        self.rx.has_pending()
    }

    /// Polls for a newly published frame since the previous `poll()` call.
    ///
    /// When overview is closed and backdrop seeding has completed, backdrop `poll()`
    /// returns `None` to prevent redundant GPU uploads/blur passes while [`Self::current`]
    /// continues to hold the warm seeded frame.
    pub fn poll(&mut self) -> Option<Arc<DecodedFrame>> {
        self.rx.poll_cloned()
    }

    /// Returns the currently retained frame (warm seeded frame or latest polled frame).
    pub fn current(&self) -> Option<Arc<DecodedFrame>> {
        self.rx.current_cloned()
    }

    /// Polls any pending frame and otherwise returns the currently retained frame.
    pub fn latest(&mut self) -> Option<Arc<DecodedFrame>> {
        self.rx.poll_cloned().or_else(|| self.rx.current_cloned())
    }

    /// Clears the retained frame in this consumer slot.
    pub fn clear(&mut self) {
        self.rx.clear();
    }
}

enum Command {
    Open { path: PathBuf, stream_id: u64 },
    Stop { stream_id: u64 },
    SeekMs(u64),
    SetPaused(bool),
    SetLooping(bool),
    SetOverviewOpen(bool),
    ReseedBackdrop,
    Shutdown,
}

struct SharedStatus {
    inner: Mutex<PlayerStatusSnapshot>,
    ready: AtomicBool,
    stream_id: AtomicU64,
}

impl SharedStatus {
    fn new(config: &DecoderConfig) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(PlayerStatusSnapshot {
                state: PlaybackState::Idle,
                stream_id: 0,
                ready: false,
                overview_open: config.overview_open,
                paused: false,
                looping: config.looping,
                frames_decoded: 0,
                foreground_frames_published: 0,
                backdrop_frames_published: 0,
                position_ms: 0,
                stream_info: None,
                last_error: None,
                worker_thread_id: None,
                teardown_thread_id: None,
            }),
            ready: AtomicBool::new(false),
            stream_id: AtomicU64::new(0),
        })
    }

    fn update(&self, f: impl FnOnce(&mut PlayerStatusSnapshot)) {
        let mut guard = self.inner.lock().expect("status mutex poisoned");
        f(&mut guard);
        self.ready.store(guard.ready, Ordering::Release);
    }
}

/// Async off-thread video decoder driving ONE active stream shared by both
/// foreground and backdrop render consumers via lock-free triple buffers.
///
/// All FFmpeg demuxing, hardware context initialization (VAAPI / CUDA), decoding,
/// seeking, looping, and old-decoder teardown execute strictly off the caller/render thread.
pub struct VideoPlayer {
    cmd_tx: Option<Sender<Command>>,
    next_stream_id: AtomicU64,
    status: Arc<SharedStatus>,
    fg_consumer: Option<FrameConsumer>,
    bg_consumer: Option<FrameConsumer>,
    worker_handle: Option<JoinHandle<()>>,
}

impl VideoPlayer {
    /// Spawns the dedicated decode and teardown worker threads and returns a non-blocking handle.
    pub fn new(config: DecoderConfig) -> Result<Self, VideoError> {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (fg_tx, fg_rx) = triple_buffer::<Arc<DecodedFrame>>();
        let (bg_tx, bg_rx) = triple_buffer::<Arc<DecodedFrame>>();
        let status = SharedStatus::new(&config);

        let worker_status = Arc::clone(&status);
        let worker_config = config.clone();
        let worker_handle = thread::Builder::new()
            .name("bgpicker-video-worker".into())
            .spawn(move || {
                run_worker(worker_config, cmd_rx, fg_tx, bg_tx, worker_status);
            })?;

        Ok(Self {
            cmd_tx: Some(cmd_tx),
            next_stream_id: AtomicU64::new(1),
            status,
            fg_consumer: Some(FrameConsumer {
                rx: fg_rx,
                role: FrameTarget::Foreground,
            }),
            bg_consumer: Some(FrameConsumer {
                rx: bg_rx,
                role: FrameTarget::Backdrop,
            }),
            worker_handle: Some(worker_handle),
        })
    }

    /// Queues opening `path` on the background worker thread and returns its unique `stream_id`.
    ///
    /// Returns immediately without blocking on FFmpeg/VAAPI/CUDA initialization or old-decoder teardown.
    pub fn open(&self, path: impl Into<PathBuf>) -> u64 {
        let stream_id = self.next_stream_id.fetch_add(1, Ordering::Relaxed);
        self.status.stream_id.store(stream_id, Ordering::Release);
        self.status.ready.store(false, Ordering::Release);
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(Command::Open {
                path: path.into(),
                stream_id,
            });
        }
        stream_id
    }

    /// Stops decoding and tears down the active decoder off the render thread.
    pub fn stop(&self) {
        let stream_id = self.next_stream_id.fetch_add(1, Ordering::Relaxed);
        self.status.stream_id.store(stream_id, Ordering::Release);
        self.status.ready.store(false, Ordering::Release);
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(Command::Stop { stream_id });
        }
    }

    /// Requests an asynchronous seek to `position_ms` on the worker thread.
    pub fn seek_ms(&self, position_ms: u64) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(Command::SeekMs(position_ms));
        }
    }

    /// Pauses or resumes playback without tearing down the decoder.
    pub fn set_paused(&self, paused: bool) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(Command::SetPaused(paused));
        }
    }

    /// Enables or disables seamless looping at end-of-stream.
    pub fn set_looping(&self, looping: bool) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(Command::SetLooping(looping));
        }
    }

    /// Switches backdrop delivery between overview-closed seeding (`false`) and
    /// overview-open live streaming (`true`).
    pub fn set_overview_open(&self, open: bool) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(Command::SetOverviewOpen(open));
        }
    }

    /// Forces the backdrop triple buffer to receive a fresh batch of seed frames.
    pub fn reseed_backdrop(&self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(Command::ReseedBackdrop);
        }
    }

    /// Takes ownership of the lock-free foreground consumer handle (once).
    pub fn take_foreground(&mut self) -> Option<FrameConsumer> {
        self.fg_consumer.take()
    }

    /// Takes ownership of the lock-free backdrop consumer handle (once).
    pub fn take_backdrop(&mut self) -> Option<FrameConsumer> {
        self.bg_consumer.take()
    }

    /// Convenience non-blocking poll on the foreground triple buffer if not taken via [`Self::take_foreground`].
    pub fn poll_foreground(&mut self) -> Option<Arc<DecodedFrame>> {
        self.fg_consumer.as_mut().and_then(FrameConsumer::poll)
    }

    /// Convenience read of the foreground consumer's retained frame if not taken via [`Self::take_foreground`].
    pub fn current_foreground(&self) -> Option<Arc<DecodedFrame>> {
        self.fg_consumer.as_ref().and_then(FrameConsumer::current)
    }

    /// Convenience non-blocking poll on the backdrop triple buffer if not taken via [`Self::take_backdrop`].
    pub fn poll_backdrop(&mut self) -> Option<Arc<DecodedFrame>> {
        self.bg_consumer.as_mut().and_then(FrameConsumer::poll)
    }

    /// Convenience read of the backdrop consumer's retained frame if not taken via [`Self::take_backdrop`].
    pub fn current_backdrop(&self) -> Option<Arc<DecodedFrame>> {
        self.bg_consumer.as_ref().and_then(FrameConsumer::current)
    }

    /// Polls any pending foreground frame or returns the currently retained one.
    pub fn latest_foreground(&mut self) -> Option<Arc<DecodedFrame>> {
        self.fg_consumer.as_mut().and_then(FrameConsumer::latest)
    }

    /// Polls any pending backdrop frame or returns the currently retained one.
    pub fn latest_backdrop(&mut self) -> Option<Arc<DecodedFrame>> {
        self.bg_consumer.as_mut().and_then(FrameConsumer::latest)
    }

    /// Fast lock-free readiness check for the latest requested stream.
    pub fn is_ready(&self) -> bool {
        self.status.ready.load(Ordering::Acquire)
    }

    /// Returns the latest `stream_id` requested via [`Self::open`].
    pub fn current_stream_id(&self) -> u64 {
        self.status.stream_id.load(Ordering::Acquire)
    }

    /// Returns a snapshot of decoder status and telemetry.
    pub fn status(&self) -> PlayerStatusSnapshot {
        self.status
            .inner
            .lock()
            .expect("status mutex poisoned")
            .clone()
    }

    /// Signals background shutdown without blocking the caller thread.
    pub fn shutdown_async(&mut self) {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(Command::Shutdown);
        }
    }

    /// Signals background shutdown and joins the worker and teardown threads cleanly.
    pub fn shutdown(mut self) {
        self.shutdown_async();
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        // Never block the render thread on Drop; teardown happens strictly on worker/teardown threads.
        self.shutdown_async();
    }
}

struct WorkerState {
    config: DecoderConfig,
    pool: Arc<BufferPool>,
    status: Arc<SharedStatus>,
    fg_tx: TripleProducer<Arc<DecodedFrame>>,
    bg_tx: TripleProducer<Arc<DecodedFrame>>,
    teardown_tx: Sender<RawDecoder>,
    decoder: Option<RawDecoder>,
    active_path: Option<PathBuf>,
    stream_id: u64,
    frame_seq: u64,
    frames_in_pass: u64,
    fg_published: u64,
    bg_published: u64,
    last_frame: Option<Arc<DecodedFrame>>,
    last_bg_seq: u64,
    backdrop_seed_remaining: u32,
    overview_open: bool,
    paused: bool,
    looping: bool,
    eof_paused: bool,
    clock_anchor: Option<(Instant, u64)>,
    hw_fallback_used: bool,
    should_exit: bool,
}

impl WorkerState {
    fn retire_decoder(&mut self) {
        if let Some(dec) = self.decoder.take() {
            let _ = self.teardown_tx.send(dec);
        }
    }

    fn open_stream(&mut self, path: PathBuf, stream_id: u64, hw_override: Option<HwAccelPreference>) {
        self.retire_decoder();
        self.active_path = Some(path.clone());
        self.stream_id = stream_id;
        self.frame_seq = 0;
        self.frames_in_pass = 0;
        self.last_frame = None;
        self.last_bg_seq = 0;
        self.backdrop_seed_remaining = self.config.backdrop_seed_frames;
        self.eof_paused = false;
        self.clock_anchor = None;
        if hw_override.is_none() {
            self.hw_fallback_used = false;
        }

        self.status.update(|s| {
            s.state = PlaybackState::Opening;
            s.stream_id = stream_id;
            s.ready = false;
            s.frames_decoded = 0;
            s.position_ms = 0;
            s.stream_info = None;
            s.last_error = None;
        });

        let params = RawOpenParams {
            hw_accel: hw_override.unwrap_or(self.config.hw_accel),
            prefer_dmabuf: self.config.prefer_dmabuf,
            cpu_format: self.config.cpu_format,
            sw_threads: self.config.sw_threads,
            vaapi_device: self.config.vaapi_device.clone(),
        };

        match RawDecoder::open(&path, &params) {
            Ok(dec) => {
                let info = dec.info.clone();
                self.decoder = Some(dec);
                let paused = self.paused;
                self.status.update(|s| {
                    s.state = if paused {
                        PlaybackState::Paused
                    } else {
                        PlaybackState::Playing
                    };
                    s.stream_info = Some(info);
                });
            }
            Err(err) => {
                self.status.update(|s| {
                    s.state = PlaybackState::Error;
                    s.ready = false;
                    s.last_error = Some(err.to_string());
                });
            }
        }
    }

    fn stop_stream(&mut self, stream_id: u64) {
        self.retire_decoder();
        self.active_path = None;
        self.stream_id = stream_id;
        self.frame_seq = 0;
        self.frames_in_pass = 0;
        self.last_frame = None;
        self.last_bg_seq = 0;
        self.backdrop_seed_remaining = 0;
        self.eof_paused = false;
        self.clock_anchor = None;
        self.status.update(|s| {
            s.state = PlaybackState::Idle;
            s.stream_id = stream_id;
            s.ready = false;
            s.frames_decoded = 0;
            s.position_ms = 0;
            s.stream_info = None;
            s.last_error = None;
        });
    }

    fn handle_seek(&mut self, target_ms: u64) {
        let Some(dec) = self.decoder.as_mut() else {
            return;
        };
        if let Err(err) = dec.seek_ms(target_ms) {
            self.status.update(|s| s.last_error = Some(err.to_string()));
            return;
        }
        self.eof_paused = false;
        self.clock_anchor = None;
        self.frames_in_pass = 0;
        self.backdrop_seed_remaining = self.config.backdrop_seed_frames;
        let paused = self.paused;
        self.status.update(|s| {
            s.position_ms = target_ms;
            s.state = if paused {
                PlaybackState::Paused
            } else {
                PlaybackState::Playing
            };
        });
    }

    fn set_overview_open(&mut self, open: bool) {
        self.overview_open = open;
        self.status.update(|s| s.overview_open = open);
        if open {
            if let Some(frame) = self.last_frame.clone() {
                if frame.frame_seq > self.last_bg_seq {
                    self.bg_tx.publish(Arc::clone(&frame));
                    self.last_bg_seq = frame.frame_seq;
                    self.bg_published += 1;
                    self.status
                        .update(|s| s.backdrop_frames_published = self.bg_published);
                    if let Some(cb) = &self.config.frame_callback {
                        cb(FrameTarget::Backdrop);
                    }
                }
            }
        }
    }

    fn reseed_backdrop(&mut self) {
        self.backdrop_seed_remaining = self.config.backdrop_seed_frames;
        if let Some(frame) = self.last_frame.clone() {
            if frame.frame_seq > self.last_bg_seq {
                self.bg_tx.publish(Arc::clone(&frame));
                self.last_bg_seq = frame.frame_seq;
                if !self.overview_open && self.backdrop_seed_remaining > 0 {
                    self.backdrop_seed_remaining -= 1;
                }
                self.bg_published += 1;
                self.status
                    .update(|s| s.backdrop_frames_published = self.bg_published);
                if let Some(cb) = &self.config.frame_callback {
                    cb(FrameTarget::Backdrop);
                }
            }
        }
    }

    fn publish_frame(&mut self, frame: Arc<DecodedFrame>) {
        let seq = frame.frame_seq;
        let pts_ms = frame.pts_ms;
        let backend = frame.backend;

        self.fg_tx.publish(Arc::clone(&frame));
        self.fg_published += 1;

        let mut target = FrameTarget::Foreground;
        if self.overview_open || self.backdrop_seed_remaining > 0 {
            self.bg_tx.publish(Arc::clone(&frame));
            self.last_bg_seq = seq;
            self.bg_published += 1;
            if !self.overview_open && self.backdrop_seed_remaining > 0 {
                self.backdrop_seed_remaining -= 1;
            }
            target = FrameTarget::Both;
        }

        self.last_frame = Some(frame);
        let ready_now = seq >= self.config.min_ready_frames.max(1);
        let fg_count = self.fg_published;
        let bg_count = self.bg_published;

        self.status.update(|s| {
            s.frames_decoded = seq;
            s.foreground_frames_published = fg_count;
            s.backdrop_frames_published = bg_count;
            s.position_ms = pts_ms;
            if let Some(info) = s.stream_info.as_mut() {
                info.backend = backend;
            }
            if ready_now {
                s.ready = true;
            }
        });

        if let Some(cb) = &self.config.frame_callback {
            cb(target);
        }
    }

    /// Processes a command. Returns `true` if any in-flight decoded frame waiting
    /// for PTS pacing should be discarded because the stream state changed.
    fn apply_command(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Open { path, stream_id } => {
                self.open_stream(path, stream_id, None);
                true
            }
            Command::Stop { stream_id } => {
                self.stop_stream(stream_id);
                true
            }
            Command::SeekMs(ms) => {
                self.handle_seek(ms);
                true
            }
            Command::SetPaused(paused) => {
                self.paused = paused;
                if paused {
                    self.clock_anchor = None;
                }
                let eof = self.eof_paused;
                let has_dec = self.decoder.is_some();
                self.status.update(|s| {
                    s.paused = paused;
                    if has_dec && !eof {
                        s.state = if paused {
                            PlaybackState::Paused
                        } else {
                            PlaybackState::Playing
                        };
                    }
                });
                paused
            }
            Command::SetLooping(looping) => {
                self.looping = looping;
                self.status.update(|s| s.looping = looping);
                if looping && self.eof_paused {
                    self.handle_seek(0);
                    return true;
                }
                false
            }
            Command::SetOverviewOpen(open) => {
                self.set_overview_open(open);
                false
            }
            Command::ReseedBackdrop => {
                self.reseed_backdrop();
                false
            }
            Command::Shutdown => {
                self.should_exit = true;
                true
            }
        }
    }
}

fn drain_pending_commands(rx: &Receiver<Command>, state: &mut WorkerState) -> bool {
    let mut cancelled = false;
    while let Ok(cmd) = rx.try_recv() {
        cancelled |= state.apply_command(cmd);
        if state.should_exit {
            return true;
        }
    }
    cancelled
}

fn wait_until_pts(
    rx: &Receiver<Command>,
    state: &mut WorkerState,
    pts_ms: u64,
) -> bool {
    if !state.config.pace_frames {
        return !drain_pending_commands(rx, state);
    }

    let now = Instant::now();
    let (anchor_wall, anchor_pts) = match state.clock_anchor {
        Some((w, p)) if pts_ms >= p => (w, p),
        _ => {
            state.clock_anchor = Some((now, pts_ms));
            return !drain_pending_commands(rx, state);
        }
    };

    let target = anchor_wall + Duration::from_millis(pts_ms - anchor_pts);
    loop {
        let now = Instant::now();
        if now >= target {
            return !drain_pending_commands(rx, state);
        }
        match rx.recv_timeout(target - now) {
            Ok(cmd) => {
                let cancelled = state.apply_command(cmd);
                if state.should_exit || cancelled {
                    return false;
                }
            }
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) => {
                state.should_exit = true;
                return false;
            }
        }
    }
}

fn run_worker(
    config: DecoderConfig,
    cmd_rx: Receiver<Command>,
    fg_tx: TripleProducer<Arc<DecodedFrame>>,
    bg_tx: TripleProducer<Arc<DecodedFrame>>,
    status: Arc<SharedStatus>,
) {
    let (teardown_tx, teardown_rx) = mpsc::channel::<RawDecoder>();
    let teardown_status = Arc::clone(&status);
    let teardown_handle = thread::Builder::new()
        .name("bgpicker-video-teardown".into())
        .spawn(move || {
            let tid = thread::current().id();
            teardown_status.update(|s| s.teardown_thread_id = Some(tid));
            while let Ok(dec) = teardown_rx.recv() {
                drop(dec);
            }
        })
        .expect("failed to spawn bgpicker-video-teardown thread");

    status.update(|s| s.worker_thread_id = Some(thread::current().id()));

    let mut state = WorkerState {
        pool: BufferPool::new(config.buffer_pool_size.max(2)),
        overview_open: config.overview_open,
        looping: config.looping,
        config,
        status,
        fg_tx,
        bg_tx,
        teardown_tx,
        decoder: None,
        active_path: None,
        stream_id: 0,
        frame_seq: 0,
        frames_in_pass: 0,
        fg_published: 0,
        bg_published: 0,
        last_frame: None,
        last_bg_seq: 0,
        backdrop_seed_remaining: 0,
        paused: false,
        eof_paused: false,
        clock_anchor: None,
        hw_fallback_used: false,
        should_exit: false,
    };

    while !state.should_exit {
        // Always drain pending commands before decoding.
        let _ = drain_pending_commands(&cmd_rx, &mut state);
        if state.should_exit {
            break;
        }

        // Idle / paused / ended: block with zero CPU until a command arrives.
        if state.decoder.is_none() || state.paused || state.eof_paused {
            match cmd_rx.recv() {
                Ok(cmd) => {
                    state.apply_command(cmd);
                }
                Err(_) => break,
            }
            continue;
        }

        let next_seq = state.frame_seq + 1;
        let stream_id = state.stream_id;
        let pool = Arc::clone(&state.pool);
        let step = state
            .decoder
            .as_mut()
            .expect("decoder checked")
            .decode_next(stream_id, next_seq, &pool);

        match step {
            Ok(DecodeStep::Frame(frame)) => {
                state.frame_seq = next_seq;
                state.frames_in_pass += 1;
                let pts_ms = frame.pts_ms;
                let frame = Arc::new(frame);
                // Hold the first frame immediately on open/seek, pace subsequent frames by PTS.
                let should_publish = if next_seq == 1 || state.clock_anchor.is_none() {
                    if state.config.pace_frames {
                        state.clock_anchor = Some((Instant::now(), pts_ms));
                    }
                    !drain_pending_commands(&cmd_rx, &mut state)
                } else {
                    wait_until_pts(&cmd_rx, &mut state, pts_ms)
                };
                if should_publish && !state.should_exit {
                    state.publish_frame(frame);
                }
            }
            Ok(DecodeStep::Eof) => {
                if state.looping && state.frames_in_pass > 0 {
                    if let Some(dec) = state.decoder.as_mut() {
                        match dec.seek_ms(0) {
                            Ok(()) => {
                                state.frames_in_pass = 0;
                                state.clock_anchor = None;
                            }
                            Err(err) => {
                                state.retire_decoder();
                                state.status.update(|s| {
                                    s.state = PlaybackState::Error;
                                    s.last_error = Some(err.to_string());
                                });
                            }
                        }
                    }
                } else if state.frames_in_pass == 0 {
                    state.retire_decoder();
                    state.status.update(|s| {
                        s.state = PlaybackState::Error;
                        s.ready = false;
                        s.last_error = Some("video stream produced zero frames".into());
                    });
                } else {
                    state.eof_paused = true;
                    state.clock_anchor = None;
                    state.status.update(|s| s.state = PlaybackState::Ended);
                }
            }
            Err(VideoError::HardwareFallback(reason))
                if !state.hw_fallback_used
                    && matches!(
                        state.config.hw_accel,
                        HwAccelPreference::Auto | HwAccelPreference::CudaFirst
                    ) =>
            {
                state.hw_fallback_used = true;
                if let Some(path) = state.active_path.clone() {
                    let id = state.stream_id;
                    state.open_stream(path, id, Some(HwAccelPreference::SoftwareOnly));
                } else {
                    state.retire_decoder();
                    state.status.update(|s| {
                        s.state = PlaybackState::Error;
                        s.last_error = Some(reason);
                    });
                }
            }
            Err(err) => {
                state.retire_decoder();
                state.status.update(|s| {
                    s.state = PlaybackState::Error;
                    s.ready = false;
                    s.last_error = Some(err.to_string());
                });
            }
        }
    }

    state.retire_decoder();
    drop(state.teardown_tx);
    let _ = teardown_handle.join();
}
