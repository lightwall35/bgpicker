//! Helpers for `bgpicker-splash`: CLI parsing, anonymous `memfd` shared memory buffers,
//! zero-decode `BGPX` splash loading, and fast image cover-fill fallback.

use std::ffi::CStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::ptr;

use bgpicker_core::{cover_fill_to_xrgb8888, read_splash_header};
use image::ImageReader;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplashOptions {
    pub fg_bin: Option<PathBuf>,
    pub fg_image: Option<PathBuf>,
    pub bg_image: Option<PathBuf>,
    pub ready_fd: Option<i32>,
    pub daemon: bool,
    pub project_dir: Option<PathBuf>,
}

/// Parses `bgpicker-splash` CLI arguments.
pub fn parse_splash_args<I, S>(args: I) -> Result<SplashOptions, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut opts = SplashOptions::default();
    let raw: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    let mut i = 0usize;
    while i < raw.len() {
        let arg = &raw[i];
        if let Some(val) = arg.strip_prefix("--fg-bin=") {
            opts.fg_bin = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--fg-image=") {
            opts.fg_image = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--bg-image=") {
            opts.bg_image = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--project-dir=") {
            opts.project_dir = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--ready-fd=") {
            let fd = val
                .parse::<i32>()
                .map_err(|e| format!("invalid --ready-fd '{val}': {e}"))?;
            opts.ready_fd = Some(fd);
        } else {
            match arg.as_str() {
                "--daemon" => {
                    opts.daemon = true;
                }
                "--project-dir" => {
                    i += 1;
                    let v = raw.get(i).ok_or("--project-dir requires a path")?;
                    opts.project_dir = Some(PathBuf::from(v));
                }
                "--fg-bin" => {
                    i += 1;
                    let v = raw.get(i).ok_or("--fg-bin requires a path")?;
                    opts.fg_bin = Some(PathBuf::from(v));
                }
                "--fg-image" => {
                    i += 1;
                    let v = raw.get(i).ok_or("--fg-image requires a path")?;
                    opts.fg_image = Some(PathBuf::from(v));
                }
                "--bg-image" => {
                    i += 1;
                    let v = raw.get(i).ok_or("--bg-image requires a path")?;
                    opts.bg_image = Some(PathBuf::from(v));
                }
                "--ready-fd" => {
                    i += 1;
                    let v = raw.get(i).ok_or("--ready-fd requires a file descriptor")?;
                    let fd = v
                        .parse::<i32>()
                        .map_err(|e| format!("invalid --ready-fd '{v}': {e}"))?;
                    opts.ready_fd = Some(fd);
                }
                other => return Err(format!("unknown bgpicker-splash argument: {other}")),
            }
        }
        i += 1;
    }

    if !opts.daemon
        && opts.fg_bin.is_none()
        && opts.fg_image.is_none()
        && opts.bg_image.is_none()
    {
        return Err(
            "at least one of --daemon, --fg-bin, --fg-image, or --bg-image is required".into(),
        );
    }
    Ok(opts)
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
    let name: &CStr = c"bgpicker-splash";
    let raw_fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if raw_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let rc = unsafe { libc::ftruncate(std::os::fd::AsRawFd::as_raw_fd(&fd), size as libc::off_t) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
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
        let mut map = MmapGuard::new(std::os::fd::AsRawFd::as_raw_fd(&fd), byte_len)?;
        file.read_exact(map.as_mut_slice())?;
    }
    Ok(Some(MemfdBuffer {
        fd,
        byte_len,
        width: w,
        height: h,
    }))
}

/// Fallback path: decodes `image_path` (or mismatched `BGPX` bitmap) and renders
/// centered cover-fill `Xrgb8888` directly into an anonymous `memfd`.
pub fn load_splash_into_memfd(
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

    // Fallback: rescale mismatched BGPX buffer if `image_path` wasn't provided.
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

fn write_pixels_to_memfd(width: u32, height: u32, pixels: &[u8]) -> io::Result<MemfdBuffer> {
    let fd = create_memfd(pixels.len())?;
    {
        let mut map = MmapGuard::new(std::os::fd::AsRawFd::as_raw_fd(&fd), pixels.len())?;
        map.as_mut_slice().copy_from_slice(pixels);
    }
    Ok(MemfdBuffer {
        fd,
        byte_len: pixels.len(),
        width,
        height,
    })
}
