//! Shared pre-rendered `BGPX` XRGB8888 boot splash bitmap helpers used by both
//! `bgpicker-daemon` (background cache writer) and `bgpicker-splash` (zero-decode
//! Wayland `wl_shm` boot placeholder renderer).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use crate::catalog::{
    cached_startup_placeholder_path, cached_video_thumbnail_path, list_videos_sorted,
};
use crate::paths::{atomic_write, BgPickerPaths, EngineKind, PersistedState};

pub const SPLASH_BIN_MAGIC: &[u8; 4] = b"BGPX";
pub const SPLASH_HEADER_BYTES: usize = 12;
const MAX_SPLASH_DIM: u32 = 16384;

/// Computes scaled buffer dimensions from a logical surface size and 1/120th
/// Wayland fractional scale factor (`180` = `1.5x`).
#[inline]
pub fn compute_scaled_size_120(logical_w: u32, logical_h: u32, scale_120: u32) -> (u32, u32) {
    let scale = scale_120.max(30);
    let pw = ((logical_w.max(1) as u64 * scale as u64 + 60) / 120).max(1) as u32;
    let ph = ((logical_h.max(1) as u64 * scale as u64 + 60) / 120).max(1) as u32;
    (pw, ph)
}

/// Renders packed RGBA8 (`rgba_is_bgra = false`) or BGRA8 (`rgba_is_bgra = true`)
/// source pixels into a tightly packed Wayland `wl_shm::Format::Xrgb8888`
/// (`[B, G, R, 0xFF]`) buffer sized `(dst_w, dst_h)` using centered cover-fill crop
/// (`swaybg -m fill` / `compute_geometry(..., zoom = 1.0)`).
#[allow(clippy::too_many_arguments)]
pub fn cover_fill_to_xrgb8888(
    src_w: u32,
    src_h: u32,
    src_stride_bytes: u32,
    src: &[u8],
    src_is_bgra: bool,
    dst_w: u32,
    dst_h: u32,
    out: &mut Vec<u8>,
) -> io::Result<()> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "splash dimensions must be non-zero",
        ));
    }
    let min_stride = (src_w as usize)
        .checked_mul(4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "stride overflow"))?;
    let stride = src_stride_bytes as usize;
    if stride < min_stride
        || src.len()
            < (src_h as usize)
                .checked_sub(1)
                .and_then(|r| r.checked_mul(stride))
                .and_then(|off| off.checked_add(min_stride))
                .unwrap_or(usize::MAX)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source pixel buffer smaller than declared geometry",
        ));
    }

    let pixel_bytes = (dst_w as usize)
        .checked_mul(dst_h as usize)
        .and_then(|p| p.checked_mul(4))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output buffer overflow"))?;
    out.resize(pixel_bytes, 0xff);

    let vw = dst_w as f64;
    let vh = dst_h as f64;
    let aspect = (src_w as f64) / (src_h as f64);
    let canvas_w = (vh * aspect).max(vw);
    let canvas_h = (vw / aspect).max(vh);
    let offset_x = (vw - canvas_w) * 0.5;
    let offset_y = (vh - canvas_h) * 0.5;

    let inv_scale_x = (src_w as f64) / canvas_w;
    let inv_scale_y = (src_h as f64) / canvas_h;
    let max_x = src_w - 1;
    let max_y = src_h - 1;

    // Precompute source X coordinates and fractional blend weights for each destination column.
    let mut x_map: Vec<(usize, usize, u32)> = Vec::with_capacity(dst_w as usize);
    for dx in 0..dst_w {
        let sx = (((dx as f64 + 0.5) - offset_x) * inv_scale_x - 0.5).clamp(0.0, max_x as f64);
        let x0 = (sx.floor() as u32).min(max_x);
        let x1 = (x0 + 1).min(max_x);
        let fx = ((sx - x0 as f64) * 256.0).round() as u32;
        x_map.push((x0 as usize * 4, x1 as usize * 4, fx.min(256)));
    }

    let (r_idx, b_idx) = if src_is_bgra { (2, 0) } else { (0, 2) };
    let dst_row_bytes = (dst_w as usize) * 4;

    for dy in 0..dst_h {
        let sy = (((dy as f64 + 0.5) - offset_y) * inv_scale_y - 0.5).clamp(0.0, max_y as f64);
        let y0 = (sy.floor() as u32).min(max_y) as usize;
        let y1 = (y0 + 1).min(max_y as usize);
        let fy = (((sy - y0 as f64) * 256.0).round() as u32).min(256);
        let inv_fy = 256 - fy;

        let row0 = &src[y0 * stride..y0 * stride + min_stride];
        let row1 = &src[y1 * stride..y1 * stride + min_stride];
        let dst_row = &mut out[dy as usize * dst_row_bytes..(dy as usize + 1) * dst_row_bytes];

        for (dx, &(ox0, ox1, fx)) in x_map.iter().enumerate() {
            let inv_fx = 256 - fx;
            let w00 = inv_fx * inv_fy;
            let w10 = fx * inv_fy;
            let w01 = inv_fx * fy;
            let w11 = fx * fy;

            let d = dx * 4;
            let r = (u32::from(row0[ox0 + r_idx]) * w00
                + u32::from(row0[ox1 + r_idx]) * w10
                + u32::from(row1[ox0 + r_idx]) * w01
                + u32::from(row1[ox1 + r_idx]) * w11
                + 32768)
                >> 16;
            let g = (u32::from(row0[ox0 + 1]) * w00
                + u32::from(row0[ox1 + 1]) * w10
                + u32::from(row1[ox0 + 1]) * w01
                + u32::from(row1[ox1 + 1]) * w11
                + 32768)
                >> 16;
            let b = (u32::from(row0[ox0 + b_idx]) * w00
                + u32::from(row0[ox1 + b_idx]) * w10
                + u32::from(row1[ox0 + b_idx]) * w01
                + u32::from(row1[ox1 + b_idx]) * w11
                + 32768)
                >> 16;

            dst_row[d] = b as u8;
            dst_row[d + 1] = g as u8;
            dst_row[d + 2] = r as u8;
            dst_row[d + 3] = 0xff;
        }
    }

    Ok(())
}

/// Atomically writes a pre-scaled `BGPX` splash bitmap (`[magic, w, h, Xrgb8888 pixels]`)
/// ready for direct `read()` into a Wayland `wl_shm` buffer by `bgpicker-splash`.
#[allow(clippy::too_many_arguments)]
pub fn write_splash_bin_from_packed(
    target: &Path,
    src_w: u32,
    src_h: u32,
    src_stride_bytes: u32,
    src: &[u8],
    src_is_bgra: bool,
    dst_w: u32,
    dst_h: u32,
) -> io::Result<()> {
    let mut pixels = Vec::new();
    cover_fill_to_xrgb8888(
        src_w,
        src_h,
        src_stride_bytes,
        src,
        src_is_bgra,
        dst_w,
        dst_h,
        &mut pixels,
    )?;

    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let tmp = parent.join(format!(
        ".splash-{}.{:?}.tmp",
        std::process::id(),
        std::thread::current().id()
    ));

    let res = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        let mut header = [0u8; SPLASH_HEADER_BYTES];
        header[0..4].copy_from_slice(SPLASH_BIN_MAGIC);
        header[4..8].copy_from_slice(&dst_w.to_le_bytes());
        header[8..12].copy_from_slice(&dst_h.to_le_bytes());
        file.write_all(&header)?;
        file.write_all(&pixels)?;
        file.flush()?;
        drop(file);
        fs::rename(&tmp, target)?;
        Ok(())
    })();

    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Validates and reads the `(width, height)` header of a `BGPX` splash bitmap file,
/// leaving the cursor positioned immediately at the start of the `Xrgb8888` pixel payload.
pub fn read_splash_header(file: &mut File) -> io::Result<(u32, u32)> {
    let meta = file.metadata()?;
    let mut header = [0u8; SPLASH_HEADER_BYTES];
    file.read_exact(&mut header)?;
    if &header[0..4] != SPLASH_BIN_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid BGPX splash magic",
        ));
    }
    let width = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    let height = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    if width == 0 || height == 0 || width > MAX_SPLASH_DIM || height > MAX_SPLASH_DIM {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid BGPX splash dimensions",
        ));
    }
    let expected = (SPLASH_HEADER_BYTES as u64)
        .checked_add((width as u64) * (height as u64) * 4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "splash size overflow"))?;
    if meta.len() != expected {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "BGPX file length mismatch",
        ));
    }
    Ok((width, height))
}

/// Resolves the foreground and overview backdrop placeholder images from persisted state.
pub fn resolve_startup_placeholders(
    paths: &BgPickerPaths,
    project_dir: &Path,
    persisted: &PersistedState,
) -> (Option<PathBuf>, Option<PathBuf>) {
    let static_wp = persisted
        .current_wallpaper
        .as_ref()
        .filter(|p| p.is_file())
        .and_then(|p| fs::canonicalize(p).ok().or_else(|| Some(p.clone())));

    let fg = match persisted.engine {
        EngineKind::Full => static_wp.as_ref().map(|wp| {
            if persisted.placeholder_cache {
                cached_startup_placeholder_path(wp, &paths.cache_dir, false)
                    .unwrap_or_else(|| wp.clone())
            } else {
                wp.clone()
            }
        }),
        EngineKind::Video => {
            let video = persisted
                .video_wallpaper
                .as_ref()
                .filter(|p| p.is_file())
                .cloned()
                .or_else(|| list_videos_sorted(&project_dir.join("dynamic")).into_iter().next());
            video
                .and_then(|v| {
                    if persisted.placeholder_cache {
                        cached_startup_placeholder_path(&v, &paths.cache_dir, true)
                    } else {
                        None
                    }
                    .or_else(|| cached_video_thumbnail_path(&v, &paths.cache_dir))
                })
                .or(static_wp)
        }
        EngineKind::Simple => None,
    };

    let cached_blur = if persisted.engine == EngineKind::Video {
        None
    } else {
        fs::read_to_string(paths.legacy_swaybg_cache_file())
            .ok()
            .map(|s| PathBuf::from(s.trim()))
            .filter(|p| p.is_file())
    };
    let bg = cached_blur.or_else(|| fg.clone());
    (fg, bg)
}

/// Anonymous shared memory (`memfd_create` + `MAP_SHARED`) ready for `wl_shm.create_pool`.
pub struct MemfdBuffer {
    fd: OwnedFd,
    byte_len: usize,
    width: u32,
    height: u32,
}

impl MemfdBuffer {
    pub fn fd(&self) -> &OwnedFd {
        &self.fd
    }

    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

struct MmapGuard {
    ptr: *mut libc::c_void,
    len: usize,
}

impl MmapGuard {
    fn new(fd: i32, len: usize) -> io::Result<Self> {
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { ptr, len })
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.cast::<u8>(), self.len) }
    }
}

impl Drop for MmapGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::munmap(self.ptr, self.len);
        }
    }
}

fn create_memfd(size: usize) -> io::Result<OwnedFd> {
    let raw_fd = unsafe { libc::memfd_create(c"bgpicker-splash".as_ptr(), libc::MFD_CLOEXEC) };
    if raw_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let rc = unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Copies tightly packed `Xrgb8888` pixels into an anonymous `memfd` shared memory buffer.
pub fn write_pixels_to_memfd(width: u32, height: u32, pixels: &[u8]) -> io::Result<MemfdBuffer> {
    let fd = create_memfd(pixels.len())?;
    {
        let mut map = MmapGuard::new(fd.as_raw_fd(), pixels.len())?;
        map.as_mut_slice().copy_from_slice(pixels);
    }
    Ok(MemfdBuffer {
        fd,
        byte_len: pixels.len(),
        width,
        height,
    })
}

/// Fast path: streams pre-rendered `BGPX` pixel data straight into `wl_shm` `memfd`
/// if dimensions match `(target_w, target_h)`.
pub fn try_load_bgpx_exact_into_memfd(
    bin_path: &Path,
    target_w: u32,
    target_h: u32,
) -> io::Result<Option<MemfdBuffer>> {
    let mut file = File::open(bin_path)?;
    let (w, h) = read_splash_header(&mut file)?;
    if w != target_w || h != target_h {
        return Ok(None);
    }
    let byte_len = (w as usize)
        .checked_mul(h as usize)
        .and_then(|p| p.checked_mul(4))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "pixel buffer overflow"))?;

    let fd = create_memfd(byte_len)?;
    {
        let mut map = MmapGuard::new(fd.as_raw_fd(), byte_len)?;
        file.read_exact(map.as_mut_slice())?;
    }
    Ok(Some(MemfdBuffer {
        fd,
        byte_len,
        width: w,
        height: h,
    }))
}

/// Exclusive non-blocking advisory lock held by the active `bgpicker-splash` process.
pub struct SplashInstanceLock {
    _file: File,
    owned_pid_file: Option<(PathBuf, u32)>,
}

impl SplashInstanceLock {
    /// Records `pid` in `pid_file` so `bgpicker-daemon` / `bgpickerctl` can cleanly retire this
    /// splash instance once the compositor presents the live daemon frame.
    pub fn record_pid(&mut self, pid_file: &Path, pid: u32) {
        if let Some(start) = read_proc_start_time(pid) {
            let _ = atomic_write(pid_file, format!("{pid} {start}\n").as_bytes());
            self.owned_pid_file = Some((pid_file.to_path_buf(), pid));
        }
    }
}

impl Drop for SplashInstanceLock {
    fn drop(&mut self) {
        if let Some((ref pid_file, pid)) = self.owned_pid_file {
            if let Ok(content) = fs::read_to_string(pid_file) {
                if content
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    == Some(pid)
                {
                    let _ = fs::remove_file(pid_file);
                }
            }
        }
    }
}

/// Tries to acquire the exclusive non-blocking `startup-placeholder.lock`.
/// Returns `None` if another `bgpicker-splash` instance currently holds it.
pub fn try_acquire_splash_lock(paths: &BgPickerPaths) -> Option<SplashInstanceLock> {
    let lock_path = paths.startup_placeholder_lock_file();
    if let Some(parent) = lock_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return None;
    }
    Some(SplashInstanceLock {
        _file: file,
        owned_pid_file: None,
    })
}

/// Returns `true` when another process currently holds `startup-placeholder.lock`.
pub fn is_splash_lock_held(paths: &BgPickerPaths) -> bool {
    let lock_path = paths.startup_placeholder_lock_file();
    let Ok(file) = OpenOptions::new().read(true).write(true).open(&lock_path) else {
        return false;
    };
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock
    } else {
        false
    }
}

/// Reads process `starttime` field (field 22 of `/proc/{pid}/stat`) to guard against PID reuse.
pub fn read_proc_start_time(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rparen = stat.rfind(')')?;
    let after_comm = stat.get(rparen + 2..)?;
    after_comm.split_whitespace().nth(19)?.parse::<u64>().ok()
}

/// Records `{pid} {starttime}` atomically into `pid_file`.
pub fn record_placeholder_pid(pid_file: &Path, pid: u32) {
    if let Some(start) = read_proc_start_time(pid) {
        let _ = atomic_write(pid_file, format!("{pid} {start}\n").as_bytes());
    }
}

/// Verifies whether `pid` belongs to a known placeholder executable (`bgpicker-splash`, `swaybg`,
/// `swaybg-front`, or any path in `extra_bins`).
pub fn process_matches_placeholder(pid: u32, extra_bins: &[&Path]) -> bool {
    let extra_real: Vec<PathBuf> = extra_bins
        .iter()
        .filter_map(|p| fs::canonicalize(p).ok())
        .collect();
    if let Ok(exe) = fs::read_link(format!("/proc/{pid}/exe")) {
        if extra_bins.contains(&exe.as_path())
            || extra_real.contains(&exe)
            || matches!(
                exe.file_name().and_then(|n| n.to_str()),
                Some("bgpicker-splash" | "swaybg" | "swaybg-front")
            )
        {
            return true;
        }
    }
    if let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) {
        for arg in cmdline.split(|&b| b == 0).take(4) {
            if arg.is_empty() {
                continue;
            }
            let Ok(arg_str) = std::str::from_utf8(arg) else {
                continue;
            };
            let arg_path = Path::new(arg_str);
            let file_name = arg_path.file_name().and_then(|n| n.to_str());
            if extra_bins.contains(&arg_path)
                || matches!(
                    file_name,
                    Some("bgpicker-splash" | "swaybg" | "swaybg-front")
                )
            {
                return true;
            }
        }
    }
    false
}

/// Returns `true` if `pid_file` references a currently live placeholder process matching `extra_bins`.
pub fn is_placeholder_pid_alive(pid_file: &Path, extra_bins: &[&Path]) -> bool {
    let Ok(content) = fs::read_to_string(pid_file) else {
        return false;
    };
    let mut parts = content.split_whitespace();
    if let (Some(pid_s), Some(start_s)) = (parts.next(), parts.next()) {
        if let (Ok(pid), Ok(start)) = (pid_s.parse::<u32>(), start_s.parse::<u64>()) {
            return read_proc_start_time(pid) == Some(start)
                && process_matches_placeholder(pid, extra_bins);
        }
    }
    false
}

/// Cleanly stops a placeholder recorded in `pid_file` (`SIGTERM` then bounded `SIGKILL` fallback)
/// and removes `pid_file`.
pub fn stop_placeholder_pid_file(pid_file: &Path, extra_bins: &[&Path]) {
    let Ok(content) = fs::read_to_string(pid_file) else {
        return;
    };
    let mut parts = content.split_whitespace();
    if let (Some(pid_s), Some(start_s)) = (parts.next(), parts.next()) {
        if let (Ok(pid), Ok(start)) = (pid_s.parse::<u32>(), start_s.parse::<u64>()) {
            if read_proc_start_time(pid) == Some(start)
                && process_matches_placeholder(pid, extra_bins)
            {
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
                let deadline = Instant::now() + Duration::from_millis(250);
                while Instant::now() < deadline {
                    if read_proc_start_time(pid) != Some(start) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                if read_proc_start_time(pid) == Some(start) {
                    unsafe {
                        libc::kill(pid as libc::pid_t, libc::SIGKILL);
                    }
                }
            }
        }
    }
    let _ = fs::remove_file(pid_file);
}

/// Stops any active startup placeholder (`front` and `back` PID files) cleanly.
pub fn retire_startup_placeholders(paths: &BgPickerPaths, extra_bins: &[&Path]) {
    stop_placeholder_pid_file(&paths.startup_placeholder_front_pid_file(), extra_bins);
    stop_placeholder_pid_file(&paths.startup_placeholder_back_pid_file(), extra_bins);
}
