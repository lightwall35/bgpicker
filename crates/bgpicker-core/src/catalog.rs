//! Media catalog scanner and thumbnail cache contract matching `media_catalog.py`
//! and `video_wallpaper.py`.

use crate::math::file_url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Supported static wallpaper extensions (lowercase without leading dot).
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "gif", "bmp", "jxl", "avif", "heif", "exr",
];

/// Supported video wallpaper extensions (lowercase without leading dot).
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "webm", "mov"];

/// Errors returned while scanning media directories or generating thumbnails.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("{kind} directory is not a directory: {path}")]
    NotADirectory { kind: &'static str, path: PathBuf },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("thumbnail generation failed: {0}")]
    ThumbnailFailed(String),
}

/// One media entry serialized identically to `media_catalog.py`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub file_name: String,
    pub file_url: String,
    pub thumbnail_url: String,
    pub original_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_error: Option<String>,
}

/// Combined `--bundle` output `{ "images": [...], "videos": [...] }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CatalogBundle {
    pub images: Vec<CatalogEntry>,
    pub videos: Vec<CatalogEntry>,
}

fn media_file_cache_key(prefix: &str, resolved_path: &Path) -> Result<String, CatalogError> {
    let meta = fs::metadata(resolved_path)?;
    let mtime_ns = (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128);
    let mut hasher = Sha256::new();
    hasher.update(prefix.as_bytes());
    hasher.update(b"\0");
    hasher.update(resolved_path.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(meta.size().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(mtime_ns.to_string().as_bytes());
    Ok(hex_encode(&hasher.finalize()))
}

/// Computes the exact SHA-256 cache key used by `video_wallpaper.prepare_thumbnail`.
///
/// Format: `sha256("bgpicker-video-thumb-v3-1280x672\0" + resolved_path + "\0" + size + "\0" + mtime_ns)`.
pub fn video_thumbnail_cache_key(resolved_path: &Path) -> Result<String, CatalogError> {
    media_file_cache_key("bgpicker-video-thumb-v3-1280x672", resolved_path)
}

/// Computes the exact SHA-256 cache key used by `video_wallpaper.prepare_image_thumbnail`.
///
/// Format: `sha256("bgpicker-image-thumb-v1-1280x672\0" + resolved_path + "\0" + size + "\0" + mtime_ns)`.
pub fn image_thumbnail_cache_key(resolved_path: &Path) -> Result<String, CatalogError> {
    media_file_cache_key("bgpicker-image-thumb-v1-1280x672", resolved_path)
}

fn startup_placeholder_target_box() -> (u32, u32) {
    if let Ok(raw) = std::env::var("BGPICKER_PLACEHOLDER_SIZE") {
        if let Some((w_s, h_s)) = raw.trim().split_once('x') {
            if let (Ok(w), Ok(h)) = (w_s.parse::<u32>(), h_s.parse::<u32>()) {
                if w >= 320 && h >= 240 {
                    return (w.min(7680), h.min(4320));
                }
            }
        }
    }
    (2560, 1600)
}

/// Computes the SHA-256 cache key for a high-resolution boot placeholder JPG (`swaybg` fast load).
pub fn startup_placeholder_cache_key(
    resolved_path: &Path,
    is_video: bool,
) -> Result<String, CatalogError> {
    let (w, h) = startup_placeholder_target_box();
    let kind = if is_video { "video" } else { "image" };
    let prefix = format!("bgpicker-startup-placeholder-v1\0{kind}\0{w}x{h}");
    media_file_cache_key(&prefix, resolved_path)
}

/// Returns an already-cached high-res startup placeholder JPG under `cache_dir/startup-placeholder`
/// if present and non-empty (without spawning `magick` or `ffmpeg`).
pub fn cached_startup_placeholder_path(
    media: &Path,
    cache_dir: &Path,
    is_video: bool,
) -> Option<PathBuf> {
    let source = fs::canonicalize(media).ok()?;
    let key = startup_placeholder_cache_key(&source, is_video).ok()?;
    let target = cache_dir
        .join("startup-placeholder")
        .join(format!("{key}.jpg"));
    is_nonempty_file(&target).then_some(target)
}

/// Generates (or reuses) a high-resolution, fast-decoding JPEG startup placeholder under
/// `cache_dir/startup-placeholder` for either a static wallpaper or video wallpaper.
pub fn prepare_startup_placeholder(
    media: &Path,
    cache_dir: &Path,
    is_video: bool,
) -> Result<PathBuf, CatalogError> {
    let source = fs::canonicalize(media)?;
    let (w, h) = startup_placeholder_target_box();
    let key = startup_placeholder_cache_key(&source, is_video)?;
    let dir = cache_dir.join("startup-placeholder");
    fs::create_dir_all(&dir)?;
    let target = dir.join(format!("{key}.jpg"));
    if is_nonempty_file(&target) {
        return Ok(target);
    }
    let tmp = dir.join(format!(
        ".{key}.{}.{:?}.jpg",
        std::process::id(),
        std::thread::current().id()
    ));

    let output = if is_video {
        Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-i",
            ])
            .arg(&source)
            .args([
                "-frames:v",
                "1",
                "-vf",
                &format!("scale={w}:{h}:force_original_aspect_ratio=decrease"),
                "-q:v",
                "2",
            ])
            .arg(&tmp)
            .output()
    } else {
        let mut source_frame = source.as_os_str().to_os_string();
        source_frame.push("[0]");
        Command::new("magick")
            .args(["-limit", "thread", "2"])
            .arg(&source_frame)
            .args([
                "-auto-orient",
                "-thumbnail",
                &format!("{w}x{h}>"),
                "-quality",
                "93",
                "-strip",
            ])
            .arg(&tmp)
            .output()
    };

    finalize_thumbnail(output, &tmp, &target, "startup placeholder failed")
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Returns the cached video thumbnail path under `cache_dir/video-thumbnails`
/// if it already exists on disk and is non-empty (without invoking `ffmpeg`).
pub fn cached_video_thumbnail_path(video: &Path, cache_dir: &Path) -> Option<PathBuf> {
    let source = fs::canonicalize(video).ok()?;
    let key = video_thumbnail_cache_key(&source).ok()?;
    let target = cache_dir.join("video-thumbnails").join(format!("{key}.jpg"));
    is_nonempty_file(&target).then_some(target)
}

/// Returns the cached video thumbnail path under `cache_dir/video-thumbnails`,
/// generating it via `ffmpeg` on cache miss.
pub fn prepare_video_thumbnail(video: &Path, cache_dir: &Path) -> Result<PathBuf, CatalogError> {
    let source = fs::canonicalize(video)?;
    let key = video_thumbnail_cache_key(&source)?;
    let dir = cache_dir.join("video-thumbnails");
    fs::create_dir_all(&dir)?;
    let target = dir.join(format!("{key}.jpg"));
    if is_nonempty_file(&target) {
        return Ok(target);
    }
    let tmp = dir.join(format!(
        ".{key}.{}.{:?}.jpg",
        std::process::id(),
        std::thread::current().id()
    ));
    let output = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
        ])
        .arg(&source)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale=1280:672:force_original_aspect_ratio=decrease",
            "-q:v",
            "2",
        ])
        .arg(&tmp)
        .output();

    finalize_thumbnail(output, &tmp, &target, "video thumbnail failed")
}

/// Returns the cached image thumbnail path under `cache_dir/image-thumbnails`,
/// generating it via `magick` on cache miss.
pub fn prepare_image_thumbnail(image: &Path, cache_dir: &Path) -> Result<PathBuf, CatalogError> {
    let source = fs::canonicalize(image)?;
    let key = image_thumbnail_cache_key(&source)?;
    let dir = cache_dir.join("image-thumbnails");
    fs::create_dir_all(&dir)?;
    let target = dir.join(format!("{key}.jpg"));
    if is_nonempty_file(&target) {
        return Ok(target);
    }
    let tmp = dir.join(format!(
        ".{key}.{}.{:?}.jpg",
        std::process::id(),
        std::thread::current().id()
    ));
    let mut source_frame = source.as_os_str().to_os_string();
    source_frame.push("[0]");

    let output = Command::new("magick")
        .args(["-limit", "thread", "2"])
        .arg(&source_frame)
        .args([
            "-auto-orient",
            "-thumbnail",
            "1280x672>",
            "-quality",
            "88",
            "-strip",
        ])
        .arg(&tmp)
        .output();

    finalize_thumbnail(output, &tmp, &target, "image thumbnail failed")
}

fn finalize_thumbnail(
    output: std::io::Result<std::process::Output>,
    tmp: &Path,
    target: &Path,
    prefix: &str,
) -> Result<PathBuf, CatalogError> {
    let res = (|| -> Result<PathBuf, CatalogError> {
        let out = output?;
        if !out.status.success() || !is_nonempty_file(tmp) {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let tail: String = stderr
                .chars()
                .rev()
                .take(500)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return Err(CatalogError::ThumbnailFailed(format!("{prefix}: {tail}")));
        }
        fs::rename(tmp, target)?;
        Ok(target.to_path_buf())
    })();
    if res.is_err() {
        let _ = fs::remove_file(tmp);
    }
    res
}

fn is_nonempty_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

fn has_extension(path: &Path, allowed: &[&str]) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let lower = ext.to_ascii_lowercase();
    allowed.contains(&lower.as_str())
}

/// Scans `directory` for matching files sorted by `(name.to_lowercase(), name)`
/// and resolves thumbnails via `thumbnail_fn` (up to 4 concurrent workers).
///
/// Missing `directory` returns `Ok(vec![])`; an existing non-directory returns
/// `Err(CatalogError::NotADirectory)`. Thumbnail errors stay local to each entry.
pub fn scan_media_catalog_with<F>(
    directory: &Path,
    allowed_extensions: &[&str],
    kind: &'static str,
    thumbnail_fn: F,
) -> Result<Vec<CatalogEntry>, CatalogError>
where
    F: Fn(&Path) -> Result<PathBuf, CatalogError> + Sync,
{
    if !directory.exists() {
        return Ok(Vec::new());
    }
    if !directory.is_dir() {
        return Err(CatalogError::NotADirectory {
            kind,
            path: directory.to_path_buf(),
        });
    }

    let mut sources: Vec<(String, PathBuf)> = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && has_extension(&path, allowed_extensions) {
            let name = entry.file_name().to_string_lossy().into_owned();
            sources.push((name, path));
        }
    }
    sources.sort_by(|(a, _), (b, _)| (a.to_lowercase(), a).cmp(&(b.to_lowercase(), b)));

    if sources.is_empty() {
        return Ok(Vec::new());
    }

    let make_entry = |name: &str, source: &Path| -> CatalogEntry {
        let original = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
        let original_path = original.to_string_lossy().into_owned();
        let file_url_str = file_url(&original);
        match thumbnail_fn(&original) {
            Ok(thumb_path) => {
                let resolved_thumb = fs::canonicalize(&thumb_path).unwrap_or(thumb_path);
                CatalogEntry {
                    file_name: name.to_string(),
                    file_url: file_url_str,
                    thumbnail_url: file_url(&resolved_thumb),
                    original_path,
                    thumbnail_error: None,
                }
            }
            Err(err) => CatalogEntry {
                file_name: name.to_string(),
                file_url: file_url_str,
                thumbnail_url: String::new(),
                original_path,
                thumbnail_error: Some(err.to_string()),
            },
        }
    };

    if sources.len() == 1 {
        let (name, path) = &sources[0];
        return Ok(vec![make_entry(name, path)]);
    }

    let worker_count = sources.len().min(4);
    let next_idx = Mutex::new(0usize);
    let mut results: Vec<Option<CatalogEntry>> = vec![None; sources.len()];
    let results_mutex = Mutex::new(&mut results);

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| loop {
                let idx = {
                    let mut guard = next_idx.lock().expect("index mutex poisoned");
                    if *guard >= sources.len() {
                        return;
                    }
                    let current = *guard;
                    *guard += 1;
                    current
                };
                let (name, path) = &sources[idx];
                let entry = make_entry(name, path);
                let mut out_guard = results_mutex.lock().expect("results mutex poisoned");
                out_guard[idx] = Some(entry);
            });
        }
    });

    Ok(results.into_iter().flatten().collect())
}

/// Scans `directory` for static wallpapers using the `image-thumbnails` cache.
pub fn build_image_catalog(
    directory: &Path,
    cache_dir: &Path,
) -> Result<Vec<CatalogEntry>, CatalogError> {
    scan_media_catalog_with(directory, IMAGE_EXTENSIONS, "image", |src| {
        prepare_image_thumbnail(src, cache_dir)
    })
}

/// Scans `directory` for video wallpapers using the `video-thumbnails` cache.
pub fn build_video_catalog(
    directory: &Path,
    cache_dir: &Path,
) -> Result<Vec<CatalogEntry>, CatalogError> {
    scan_media_catalog_with(directory, VIDEO_EXTENSIONS, "video", |src| {
        prepare_video_thumbnail(src, cache_dir)
    })
}

/// Lists all supported videos in `dir` sorted case-insensitively by `(name.to_lowercase(), name)`.
pub fn list_videos_sorted(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let lower = ext.to_ascii_lowercase();
        if VIDEO_EXTENSIONS.contains(&lower.as_str()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            entries.push((name, path));
        }
    }
    entries.sort_by(|(a, _), (b, _)| (a.to_lowercase(), a).cmp(&(b.to_lowercase(), b)));
    entries.into_iter().map(|(_, p)| p).collect()
}

/// Scans both static image and video directories into a [`CatalogBundle`].
pub fn build_bundle(
    image_dir: &Path,
    video_dir: &Path,
    cache_dir: &Path,
) -> Result<CatalogBundle, CatalogError> {
    Ok(CatalogBundle {
        images: build_image_catalog(image_dir, cache_dir)?,
        videos: build_video_catalog(video_dir, cache_dir)?,
    })
}
