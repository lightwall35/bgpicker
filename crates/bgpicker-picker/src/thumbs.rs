//! Bounded async thumbnail & hi-res image decoder with strict GPU LRU texture eviction.
//!
//! Decodes JPG/PNG/WebP/BMP/GIF thumbnails on demand for a bounded visible window
//! (`current_index ± 6`, prioritized by distance from center) plus a single cancellable
//! high-resolution original slot for the 1.5s hold-expand preview.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{
    Arc, Condvar, Mutex,
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};

use glow::HasContext;
use image::imageops::FilterType;

use crate::state::MediaEntry;

/// Bounded LRU map that reports evicted `(K, V)` pairs so GPU resources can be freed immediately.
#[derive(Debug)]
pub struct LruCache<K, V> {
    capacity: usize,
    tick: u64,
    entries: HashMap<K, (u64, V)>,
}

impl<K, V> LruCache<K, V>
where
    K: Eq + std::hash::Hash,
{
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            tick: 0,
            entries: HashMap::with_capacity(capacity.max(1)),
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.entries.contains_key(key)
    }

    /// Looks up `key` and marks it most-recently-used.
    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.tick = self.tick.wrapping_add(1);
        let t = self.tick;
        self.entries.get_mut(key).map(|(last_used, val)| {
            *last_used = t;
            &*val
        })
    }

    /// Inserts `(key, value)` as most-recently-used, returning any replaced or
    /// least-recently-used evicted `(K, V)` pairs so GPU resources can be freed immediately.
    pub fn insert(&mut self, key: K, value: V) -> Vec<(K, V)>
    where
        K: Clone,
    {
        self.tick = self.tick.wrapping_add(1);
        let t = self.tick;
        let mut evicted = Vec::new();
        if let Some((_, old_val)) = self.entries.insert(key.clone(), (t, value)) {
            evicted.push((key, old_val));
        }
        while self.entries.len() > self.capacity {
            let oldest_key = self
                .entries
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest_key
                && let Some((_, v)) = self.entries.remove(&k)
            {
                evicted.push((k, v));
            } else {
                break;
            }
        }
        evicted
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.entries.remove(key).map(|(_, v)| v)
    }

    pub fn drain_all(&mut self) -> Vec<(K, V)> {
        self.entries.drain().map(|(k, (_, v))| (k, v)).collect()
    }
}

/// GPU texture handle paired with its pixel dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuTexture {
    pub texture: glow::Texture,
    pub width: u32,
    pub height: u32,
}

/// Configuration for [`ThumbnailCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailCacheConfig {
    pub max_thumbnails: usize,
    pub visible_radius: usize,
    pub worker_threads: usize,
    pub max_thumb_width: u32,
    pub max_thumb_height: u32,
    pub max_hires_dim: u32,
}

impl Default for ThumbnailCacheConfig {
    fn default() -> Self {
        let workers = thread::available_parallelism()
            .map(|n| n.get().clamp(2, 4))
            .unwrap_or(2);
        Self {
            max_thumbnails: 24,
            visible_radius: 6,
            worker_threads: workers,
            max_thumb_width: 1280,
            max_thumb_height: 672,
            max_hires_dim: 2560,
        }
    }
}

#[derive(Debug, Clone)]
struct ThumbJob {
    path: String,
    priority: usize,
    max_w: u32,
    max_h: u32,
}

#[derive(Debug, Clone)]
struct HiResJob {
    path: String,
    generation: u64,
    max_dim: u32,
}

#[derive(Debug, Default)]
struct WorkQueue {
    thumb_jobs: Vec<ThumbJob>,
    hires_job: Option<HiResJob>,
    shutdown: bool,
}

enum DecodeResult {
    Thumb {
        path: String,
        result: Result<(u32, u32, Vec<u8>), String>,
    },
    HiRes {
        path: String,
        generation: u64,
        result: Result<(u32, u32, Vec<u8>), String>,
    },
}

/// Async bounded thumbnail + hi-res texture manager.
pub struct ThumbnailCache {
    config: ThumbnailCacheConfig,
    thumbs: LruCache<String, GpuTexture>,
    in_flight_thumbs: HashSet<String>,
    failed_thumbs: HashSet<String>,

    hires_generation: u64,
    hires_requested_path: Option<String>,
    hires_texture: Option<(String, GpuTexture)>,

    queue: Arc<(Mutex<WorkQueue>, Condvar)>,
    result_rx: Receiver<DecodeResult>,
    workers: Vec<JoinHandle<()>>,
}

impl ThumbnailCache {
    /// Creates a new [`ThumbnailCache`] and spawns its background decode worker pool.
    pub fn new(config: ThumbnailCacheConfig) -> Self {
        let worker_count = config.worker_threads.clamp(1, 4);
        let queue = Arc::new((Mutex::new(WorkQueue::default()), Condvar::new()));
        let (tx, rx) = mpsc::channel();
        let mut workers = Vec::with_capacity(worker_count);
        for i in 0..worker_count {
            let q = Arc::clone(&queue);
            let worker_tx = tx.clone();
            let handle = thread::Builder::new()
                .name(format!("bgpicker-thumb-{i}"))
                .spawn(move || worker_loop(q, worker_tx))
                .expect("failed to spawn thumbnail worker thread");
            workers.push(handle);
        }

        Self {
            config,
            thumbs: LruCache::new(config.max_thumbnails),
            in_flight_thumbs: HashSet::new(),
            failed_thumbs: HashSet::new(),
            hires_generation: 0,
            hires_requested_path: None,
            hires_texture: None,
            queue,
            result_rx: rx,
            workers,
        }
    }

    #[inline]
    pub fn config(&self) -> ThumbnailCacheConfig {
        self.config
    }

    #[inline]
    pub fn thumbnail_count(&self) -> usize {
        self.thumbs.len()
    }

    #[inline]
    pub fn has_hires_texture(&self) -> bool {
        self.hires_texture.is_some()
    }

    #[inline]
    pub fn hires_loaded_path(&self) -> Option<&str> {
        self.hires_texture.as_ref().map(|(p, _)| p.as_str())
    }

    /// Returns `true` if any background decode is currently queued or in flight.
    pub fn has_pending_work(&self) -> bool {
        if !self.in_flight_thumbs.is_empty() {
            return true;
        }
        if let Some(ref req) = self.hires_requested_path
            && self.hires_loaded_path() != Some(req.as_str())
        {
            return true;
        }
        false
    }

    /// Synchronizes the background decode queue to the visible window
    /// `[center_index - visible_radius ..= center_index + visible_radius]`,
    /// prioritized by distance from `center_index`.
    pub fn sync_visible_window(&mut self, entries: &[MediaEntry], center_index: usize) {
        if entries.is_empty() {
            let (lock, _) = &*self.queue;
            let mut q = lock.lock().expect("work queue poisoned");
            q.thumb_jobs.clear();
            return;
        }
        let clamped_center = center_index.min(entries.len() - 1);
        let start = clamped_center.saturating_sub(self.config.visible_radius);
        let end = (clamped_center + self.config.visible_radius).min(entries.len() - 1);

        let mut desired: Vec<ThumbJob> = Vec::with_capacity(end - start + 1);
        let mut seen = HashSet::new();
        for (idx, entry) in entries.iter().enumerate().take(end + 1).skip(start) {
            let path = if !entry.thumbnail_path.is_empty() {
                &entry.thumbnail_path
            } else {
                &entry.original_path
            };
            if path.is_empty() || !seen.insert(path.as_str()) {
                continue;
            }
            if self.thumbs.contains_key(path.as_str()) {
                // Touch recency so active visible items aren't evicted before off-screen ones.
                let _ = self.thumbs.get(path.as_str());
                continue;
            }
            if self.in_flight_thumbs.contains(path.as_str())
                || self.failed_thumbs.contains(path.as_str())
            {
                continue;
            }
            desired.push(ThumbJob {
                path: path.clone(),
                priority: idx.abs_diff(clamped_center),
                max_w: self.config.max_thumb_width,
                max_h: self.config.max_thumb_height,
            });
        }
        desired.sort_by_key(|j| j.priority);

        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().expect("work queue poisoned");
        // Cancel previously queued (not yet started) jobs no longer in window,
        // freeing them from `in_flight_thumbs`.
        for dropped in q.thumb_jobs.drain(..) {
            self.in_flight_thumbs.remove(&dropped.path);
        }
        for job in &desired {
            self.in_flight_thumbs.insert(job.path.clone());
        }
        let has_new = !desired.is_empty();
        q.thumb_jobs = desired;
        drop(q);
        if has_new {
            cv.notify_all();
        }
    }

    /// Ensures the single high-res original slot matches `desired_path`.
    ///
    /// Releases any previous GPU hi-res texture immediately when `desired_path` changes
    /// or becomes `None`.
    pub fn sync_hires(&mut self, gl: &glow::Context, desired_path: Option<&str>, max_dim: u32) {
        match desired_path {
            None => {
                if self.hires_requested_path.is_some() || self.hires_texture.is_some() {
                    self.release_hires(gl);
                }
            }
            Some(path) => {
                if self.hires_requested_path.as_deref() == Some(path) {
                    return;
                }
                self.release_hires(gl);
                self.hires_generation = self.hires_generation.wrapping_add(1);
                let gen_id = self.hires_generation;
                self.hires_requested_path = Some(path.to_string());
                let (lock, cv) = &*self.queue;
                let mut q = lock.lock().expect("work queue poisoned");
                q.hires_job = Some(HiResJob {
                    path: path.to_string(),
                    generation: gen_id,
                    max_dim: max_dim.clamp(512, self.config.max_hires_dim),
                });
                drop(q);
                cv.notify_one();
            }
        }
    }

    /// Cancels any pending hi-res decode and immediately deletes the resident hi-res GPU texture.
    pub fn release_hires(&mut self, gl: &glow::Context) {
        self.hires_generation = self.hires_generation.wrapping_add(1);
        self.hires_requested_path = None;
        {
            let (lock, _) = &*self.queue;
            let mut q = lock.lock().expect("work queue poisoned");
            q.hires_job = None;
        }
        if let Some((_, tex)) = self.hires_texture.take() {
            unsafe {
                gl.delete_texture(tex.texture);
            }
        }
    }

    /// Drains ready decoded images and uploads up to `max_uploads` textures to GL.
    ///
    /// Returns `true` if at least one new texture was uploaded.
    pub fn upload_ready(&mut self, gl: &glow::Context, max_uploads: usize) -> bool {
        let limit = max_uploads.max(1);
        let mut uploaded = false;
        let mut count = 0usize;

        while count < limit {
            let Ok(msg) = self.result_rx.try_recv() else {
                break;
            };
            match msg {
                DecodeResult::Thumb { path, result } => {
                    self.in_flight_thumbs.remove(&path);
                    match result {
                        Ok((w, h, rgba)) => {
                            if let Ok(tex) = upload_rgba_texture(gl, w, h, &rgba) {
                                let evicted = self.thumbs.insert(
                                    path,
                                    GpuTexture {
                                        texture: tex,
                                        width: w,
                                        height: h,
                                    },
                                );
                                for (_, old) in evicted {
                                    unsafe {
                                        gl.delete_texture(old.texture);
                                    }
                                }
                                uploaded = true;
                                count += 1;
                            }
                        }
                        Err(err) => {
                            log::debug!("bgpicker-picker: thumbnail decode failed ({path}): {err}");
                            self.failed_thumbs.insert(path);
                        }
                    }
                }
                DecodeResult::HiRes {
                    path,
                    generation,
                    result,
                } => {
                    if generation != self.hires_generation {
                        // Stale hi-res decode from an unheld/switched card — discard immediately.
                        continue;
                    }
                    match result {
                        Ok((w, h, rgba)) => {
                            if let Ok(tex) = upload_rgba_texture(gl, w, h, &rgba) {
                                if let Some((_, old)) = self.hires_texture.take() {
                                    unsafe {
                                        gl.delete_texture(old.texture);
                                    }
                                }
                                self.hires_texture = Some((
                                    path,
                                    GpuTexture {
                                        texture: tex,
                                        width: w,
                                        height: h,
                                    },
                                ));
                                uploaded = true;
                                count += 1;
                            }
                        }
                        Err(err) => {
                            log::debug!("bgpicker-picker: hi-res decode failed ({path}): {err}");
                        }
                    }
                }
            }
        }
        uploaded
    }

    /// Looks up a cached thumbnail GPU texture and refreshes its LRU recency.
    pub fn get_thumbnail(&mut self, path: &str) -> Option<GpuTexture> {
        self.thumbs.get(path).copied()
    }

    /// Looks up the active hi-res GPU texture if it matches `original_path`.
    pub fn get_hires(&self, original_path: &str) -> Option<GpuTexture> {
        self.hires_texture
            .as_ref()
            .filter(|(p, _)| p == original_path)
            .map(|(_, tex)| *tex)
    }

    /// Inserts an already-created GPU thumbnail texture (useful for tests or preloaded textures),
    /// enforcing the bounded LRU limit and deleting any evicted texture.
    pub fn insert_thumbnail_gpu(
        &mut self,
        gl: &glow::Context,
        key: impl Into<String>,
        tex: GpuTexture,
    ) {
        let evicted = self.thumbs.insert(key.into(), tex);
        for (_, old) in evicted {
            unsafe {
                gl.delete_texture(old.texture);
            }
        }
    }

    /// Releases every GPU texture (all thumbnails + hi-res) and clears pending decode state.
    pub fn clear_all_gpu(&mut self, gl: &glow::Context) {
        self.release_hires(gl);
        {
            let (lock, _) = &*self.queue;
            let mut q = lock.lock().expect("work queue poisoned");
            q.thumb_jobs.clear();
            q.hires_job = None;
        }
        self.in_flight_thumbs.clear();
        self.failed_thumbs.clear();
        for (_, tex) in self.thumbs.drain_all() {
            unsafe {
                gl.delete_texture(tex.texture);
            }
        }
        // Drain any already-decoded CPU buffers sitting in the channel.
        while self.result_rx.try_recv().is_ok() {}
    }
}

impl Drop for ThumbnailCache {
    fn drop(&mut self) {
        {
            let (lock, cv) = &*self.queue;
            let mut q = lock.lock().expect("work queue poisoned");
            q.shutdown = true;
            q.thumb_jobs.clear();
            q.hires_job = None;
            cv.notify_all();
        }
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

fn worker_loop(queue: Arc<(Mutex<WorkQueue>, Condvar)>, tx: Sender<DecodeResult>) {
    loop {
        enum NextJob {
            HiRes(HiResJob),
            Thumb(ThumbJob),
            Stop,
        }

        let job = {
            let (lock, cv) = &*queue;
            let mut q = lock.lock().expect("work queue poisoned");
            loop {
                if q.shutdown {
                    break NextJob::Stop;
                }
                // Prioritize active hi-res hold preview when present, otherwise lowest-distance thumbnail.
                if let Some(hr) = q.hires_job.take() {
                    break NextJob::HiRes(hr);
                }
                if !q.thumb_jobs.is_empty() {
                    let t = q.thumb_jobs.remove(0);
                    break NextJob::Thumb(t);
                }
                q = cv.wait(q).expect("work queue condvar poisoned");
            }
        };

        match job {
            NextJob::Stop => return,
            NextJob::Thumb(t) => {
                let res = decode_and_fit(Path::new(&t.path), t.max_w, t.max_h);
                if tx
                    .send(DecodeResult::Thumb {
                        path: t.path,
                        result: res,
                    })
                    .is_err()
                {
                    return;
                }
            }
            NextJob::HiRes(hr) => {
                let res = decode_and_fit(Path::new(&hr.path), hr.max_dim, hr.max_dim);
                if tx
                    .send(DecodeResult::HiRes {
                        path: hr.path,
                        generation: hr.generation,
                        result: res,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

fn decode_and_fit(path: &Path, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    let fitted = if w > max_w || h > max_h {
        img.resize(max_w, max_h, FilterType::Triangle)
    } else {
        img
    };
    let rgba = fitted.to_rgba8();
    let (fw, fh) = rgba.dimensions();
    Ok((fw, fh, rgba.into_raw()))
}

fn upload_rgba_texture(
    gl: &glow::Context,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<glow::Texture, String> {
    unsafe {
        let tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            width as i32,
            height as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(rgba)),
        );
        gl.bind_texture(glow::TEXTURE_2D, None);
        Ok(tex)
    }
}
