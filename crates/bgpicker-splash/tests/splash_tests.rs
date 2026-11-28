use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use bgpicker_core::{
    compute_scaled_size_120, read_splash_header, write_splash_bin_from_packed,
};
use bgpicker_splash::{load_splash_into_memfd, parse_splash_args, try_load_bgpx_exact_into_memfd};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bgpicker-splash-test-{prefix}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn cli_parser_and_scaled_size_120_work() {
    assert_eq!(compute_scaled_size_120(1707, 1067, 180), (2561, 1601));
    assert_eq!(compute_scaled_size_120(1920, 1080, 120), (1920, 1080));

    let parsed = parse_splash_args([
        "--fg-bin",
        "/tmp/fg.bin",
        "--fg-image=/tmp/fg.jpg",
        "--bg-image",
        "/tmp/bg.png",
        "--ready-fd",
        "7",
    ])
    .unwrap();
    assert_eq!(parsed.fg_bin, Some(PathBuf::from("/tmp/fg.bin")));
    assert_eq!(parsed.fg_image, Some(PathBuf::from("/tmp/fg.jpg")));
    assert_eq!(parsed.bg_image, Some(PathBuf::from("/tmp/bg.png")));
    assert_eq!(parsed.ready_fd, Some(7));

    assert!(parse_splash_args(["--daemon", "--project-dir", "/tmp/proj"]).unwrap().daemon);
    assert_eq!(
        parse_splash_args(["--daemon", "--project-dir=/tmp/proj"])
            .unwrap()
            .project_dir,
        Some(PathBuf::from("/tmp/proj"))
    );

    assert!(parse_splash_args::<[&str; 0], &str>([]).is_err());
    assert!(parse_splash_args(["--ready-fd", "abc"]).is_err());
    assert!(parse_splash_args(["--unknown"]).is_err());
}

#[test]
fn splash_lock_enforces_single_instance_and_cleans_owned_pid_file() {
    use bgpicker_core::{is_splash_lock_held, try_acquire_splash_lock, BgPickerPaths};

    let tmp = TempDir::new("lock");
    let mut paths = BgPickerPaths::from_env();
    paths.state_dir = tmp.path.join("state");
    paths.cache_dir = tmp.path.join("cache");
    fs::create_dir_all(&paths.state_dir).unwrap();

    assert!(!is_splash_lock_held(&paths));
    let mut lock1 = try_acquire_splash_lock(&paths).expect("first lock succeeds");
    let pid_file = paths.startup_placeholder_front_pid_file();
    lock1.record_pid(&pid_file, std::process::id());
    assert!(pid_file.is_file());
    assert!(is_splash_lock_held(&paths));
    assert!(try_acquire_splash_lock(&paths).is_none());

    drop(lock1);
    assert!(!pid_file.exists());
    assert!(!is_splash_lock_held(&paths));
    assert!(try_acquire_splash_lock(&paths).is_some());
}

#[test]
fn bgpx_write_read_and_memfd_fast_path_plus_image_fallback() {
    let tmp = TempDir::new("bgpx");
    let bin_path = tmp.path.join("splash.bin");

    // 2x2 RGBA pattern: red top-left, green top-right, blue bottom-left, white bottom-right.
    let src_rgba = [
        255u8, 0, 0, 255, 0, 255, 0, 255, //
        0, 0, 255, 255, 255, 255, 255, 255,
    ];
    write_splash_bin_from_packed(&bin_path, 2, 2, 8, &src_rgba, false, 4, 4).unwrap();

    let mut f = fs::File::open(&bin_path).unwrap();
    assert_eq!(read_splash_header(&mut f).unwrap(), (4, 4));
    let mut payload = Vec::new();
    f.read_to_end(&mut payload).unwrap();
    assert_eq!(payload.len(), 4 * 4 * 4);
    // Top-left corner maps to red: Xrgb8888 little-endian [B=0, G=0, R=255, 0xFF].
    assert_eq!(&payload[0..4], &[0, 0, 255, 255]);

    // Exact dimension match loads straight into memfd with identical bytes.
    let memfd = try_load_bgpx_exact_into_memfd(&bin_path, 4, 4)
        .unwrap()
        .expect("exact match");
    assert_eq!((memfd.width(), memfd.height()), (4, 4));
    let mut memfd_file: fs::File = memfd.fd().try_clone().unwrap().into();
    memfd_file.seek(SeekFrom::Start(0)).unwrap();
    let mut shm_bytes = Vec::new();
    memfd_file.read_to_end(&mut shm_bytes).unwrap();
    assert_eq!(shm_bytes, payload);

    // Mismatched dimension falls back cleanly without failing.
    assert!(try_load_bgpx_exact_into_memfd(&bin_path, 8, 6).unwrap().is_none());
    let rescaled = load_splash_into_memfd(Some(&bin_path), None, 8, 6).unwrap();
    assert_eq!((rescaled.width(), rescaled.height(), rescaled.byte_len()), (8, 6, 8 * 6 * 4));
}
