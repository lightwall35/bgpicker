use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bgpicker_video::{
    CpuPixelFormat, DecoderConfig, FrameData, FrameTarget, HwAccelPreference, PlaybackState,
    VideoPlayer,
};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "bgpicker-video-test-{prefix}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self { path: dir }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn generate_test_mp4(path: &Path, duration_secs: &str, fps: u32, color: &str) {
    let status = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size=320x180:rate={fps}:duration={duration_secs}"),
            "-f",
            "lavfi",
            "-i",
            &format!("color=c={color}:s=320x180:r={fps}:d={duration_secs}"),
            "-filter_complex",
            "[0:v][1:v]blend=all_mode=addition:all_opacity=0.25[v]",
            "-map",
            "[v]",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "10",
            path.to_str().unwrap(),
        ])
        .status()
        .expect("run ffmpeg to generate test mp4");
    assert!(status.success(), "ffmpeg failed generating {}", path.display());
}

fn wait_until<F: FnMut() -> bool>(timeout: Duration, mut pred: F) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn off_thread_open_switch_and_teardown() {
    let tmp = TempDir::new("off-thread");
    let mp4_a = tmp.file("a.mp4");
    let mp4_b = tmp.file("b.mp4");
    generate_test_mp4(&mp4_a, "0.6", 30, "red");
    generate_test_mp4(&mp4_b, "0.6", 30, "blue");

    let caller_tid = std::thread::current().id();
    let mut player = VideoPlayer::new(DecoderConfig {
        pace_frames: false,
        min_ready_frames: 2,
        ..Default::default()
    })
    .expect("spawn VideoPlayer");

    let t0 = Instant::now();
    let stream_a = player.open(&mp4_a);
    let open_elapsed = t0.elapsed();
    assert!(
        open_elapsed < Duration::from_millis(25),
        "open() should be non-blocking, took {open_elapsed:?}"
    );

    assert!(
        wait_until(Duration::from_secs(5), || player.is_ready()),
        "stream A never became ready: {:?}",
        player.status()
    );

    let st_a = player.status();
    assert_eq!(st_a.stream_id, stream_a);
    assert_ne!(st_a.worker_thread_id, Some(caller_tid));
    assert_ne!(st_a.teardown_thread_id, Some(caller_tid));
    assert_ne!(st_a.worker_thread_id, st_a.teardown_thread_id);
    assert_eq!(st_a.stream_info.as_ref().unwrap().width, 320);
    assert_eq!(st_a.stream_info.as_ref().unwrap().height, 180);

    let frame_a = wait_until(Duration::from_secs(2), || {
        player
            .poll_foreground()
            .is_some_and(|f| f.stream_id == stream_a)
    });
    assert!(frame_a, "expected frame for stream A");

    // Switching to stream B must also return immediately and retire A on teardown thread.
    let t1 = Instant::now();
    let stream_b = player.open(&mp4_b);
    assert!(t1.elapsed() < Duration::from_millis(25));
    assert!(stream_b > stream_a);

    assert!(
        wait_until(Duration::from_secs(5), || {
            player.is_ready()
                && player
                    .poll_foreground()
                    .is_some_and(|f| f.stream_id == stream_b)
        }),
        "expected stream B frames: {:?}",
        player.status()
    );

    player.stop();
    assert!(wait_until(Duration::from_secs(2), || {
        player.status().state == PlaybackState::Idle
    }));

    player.shutdown();
}

#[test]
fn hwaccel_auto_and_software_cpu_formats() {
    let tmp = TempDir::new("formats");
    let mp4 = tmp.file("test.mp4");
    generate_test_mp4(&mp4, "0.5", 30, "green");

    // 1. Auto hardware acceleration (VAAPI DMA-BUF zero-copy or upload/CUDA/software fallback).
    {
        let mut player = VideoPlayer::new(DecoderConfig {
            hw_accel: HwAccelPreference::Auto,
            prefer_dmabuf: true,
            pace_frames: false,
            ..Default::default()
        })
        .unwrap();
        let mut fg = player.take_foreground().unwrap();
        let mut bg = player.take_backdrop().unwrap();

        player.open(&mp4);
        assert!(wait_until(Duration::from_secs(5), || player.is_ready()));

        let fg_frame = loop {
            if let Some(f) = fg.poll() {
                break f;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let bg_frame = loop {
            if let Some(f) = bg.latest() {
                break f;
            }
            std::thread::sleep(Duration::from_millis(2));
        };

        eprintln!("Auto backend selected: {:?}", fg_frame.backend);
        assert_eq!(fg_frame.dimensions(), (320, 180));
        assert_eq!(bg_frame.dimensions(), (320, 180));

        match &fg_frame.data {
            FrameData::DmaBuf(dma) => {
                assert!(!dma.objects.is_empty());
                assert!(!dma.layers.is_empty());
                for obj in &dma.objects {
                    assert!(obj.fd.as_raw_fd() >= 0);
                }
                for layer in &dma.layers {
                    assert_ne!(layer.drm_fourcc, 0);
                    assert!(!layer.planes.is_empty());
                }
            }
            FrameData::Cpu(cpu) => {
                assert!(!cpu.as_bytes().is_empty());
            }
        }
        player.shutdown();
    }

    // 2. Explicit SoftwareOnly NV12 and RGBA8 buffer layout checks.
    for (fmt, expected_bytes) in [
        (CpuPixelFormat::Nv12, 320 * 180 * 3 / 2),
        (CpuPixelFormat::Rgba8, 320 * 180 * 4),
    ] {
        let mut player = VideoPlayer::new(DecoderConfig {
            hw_accel: HwAccelPreference::SoftwareOnly,
            cpu_format: fmt,
            pace_frames: false,
            ..Default::default()
        })
        .unwrap();
        player.open(&mp4);
        assert!(wait_until(Duration::from_secs(5), || player.is_ready()));
        assert!(wait_until(Duration::from_secs(2), || {
            player.poll_foreground().is_some()
        }));
        let f = player.current_foreground().unwrap();
        match &f.data {
            FrameData::Cpu(cpu) => {
                assert_eq!(cpu.format, fmt);
                assert_eq!(cpu.as_bytes().len(), expected_bytes);
                assert!(cpu.as_bytes().iter().any(|&b| b != 0));
            }
            FrameData::DmaBuf(_) => panic!("SoftwareOnly should yield Cpu frame"),
        }
        player.shutdown();
    }

    // 3. Hardware upload fallback (prefer_dmabuf: false) also produces valid Cpu frames.
    {
        let mut player = VideoPlayer::new(DecoderConfig {
            hw_accel: HwAccelPreference::Auto,
            prefer_dmabuf: false,
            cpu_format: CpuPixelFormat::Nv12,
            pace_frames: false,
            ..Default::default()
        })
        .unwrap();
        player.open(&mp4);
        assert!(wait_until(Duration::from_secs(5), || player.is_ready()));
        assert!(wait_until(Duration::from_secs(2), || {
            player.poll_foreground().is_some()
        }));
        let f = player.latest_foreground().unwrap();
        eprintln!("Upload fallback backend selected: {:?}", f.backend);
        match &f.data {
            FrameData::Cpu(cpu) => {
                assert_eq!(cpu.format, CpuPixelFormat::Nv12);
                assert_eq!(cpu.as_bytes().len(), 320 * 180 * 3 / 2);
                assert_eq!(cpu.planes.len(), 2);
                assert!(cpu.plane_bytes(0).is_some());
                assert!(cpu.plane_bytes(1).is_some());
            }
            FrameData::DmaBuf(_) => panic!("prefer_dmabuf=false must yield Cpu frame"),
        }
        player.shutdown();
    }
}

#[test]
fn single_stream_shared_and_overview_seeding_vs_live_streaming() {
    let tmp = TempDir::new("overview-seed");
    let mp4 = tmp.file("long.mp4");
    generate_test_mp4(&mp4, "1.0", 30, "yellow");

    let fg_callbacks = Arc::new(AtomicU64::new(0));
    let bg_callbacks = Arc::new(AtomicU64::new(0));
    let fg_cb = Arc::clone(&fg_callbacks);
    let bg_cb = Arc::clone(&bg_callbacks);

    let mut player = VideoPlayer::new(DecoderConfig {
        hw_accel: HwAccelPreference::SoftwareOnly,
        cpu_format: CpuPixelFormat::Nv12,
        overview_open: false,
        backdrop_seed_frames: 4,
        min_ready_frames: 2,
        pace_frames: false,
        looping: true,
        frame_callback: Some(Arc::new(move |target| match target {
            FrameTarget::Foreground => {
                fg_cb.fetch_add(1, Ordering::Relaxed);
            }
            FrameTarget::Backdrop => {
                bg_cb.fetch_add(1, Ordering::Relaxed);
            }
            FrameTarget::Both => {
                fg_cb.fetch_add(1, Ordering::Relaxed);
                bg_cb.fetch_add(1, Ordering::Relaxed);
            }
        })),
        ..Default::default()
    })
    .unwrap();

    let mut fg = player.take_foreground().unwrap();
    let mut bg = player.take_backdrop().unwrap();
    assert!(player.take_foreground().is_none());
    assert!(player.take_backdrop().is_none());

    player.open(&mp4);

    // Wait until foreground has decoded plenty of frames (> 20) while overview is closed.
    assert!(wait_until(Duration::from_secs(5), || {
        player.status().frames_decoded >= 25
    }));

    let st = player.status();
    assert!(st.foreground_frames_published >= 25);
    assert_eq!(
        st.backdrop_frames_published, 4,
        "overview-closed must publish exactly 4 seed frames, got {}",
        st.backdrop_frames_published
    );

    // Poll foreground and backdrop: backdrop must retain warm seed frame and stop reporting new frames.
    let fg_latest = fg.poll().expect("foreground must have latest frame");
    let bg_seeded = bg.poll().expect("backdrop must have seeded frame");
    assert!(bg_seeded.frame_seq <= 4);
    assert!(fg_latest.frame_seq >= 4);
    assert!(bg.poll().is_none(), "backdrop must not receive new frames while overview closed");
    assert!(
        Arc::ptr_eq(&bg.current().unwrap(), &bg_seeded),
        "backdrop current() must retain warm seeded frame"
    );

    // Pause briefly, open overview, and confirm backdrop immediately receives the current latest frame!
    player.set_paused(true);
    assert!(wait_until(Duration::from_secs(2), || {
        player.status().state == PlaybackState::Paused
    }));
    let paused_seq = player.status().frames_decoded;

    player.set_overview_open(true);
    assert!(
        wait_until(Duration::from_secs(2), || bg.has_pending()),
        "opening overview should immediately push latest frame to backdrop"
    );
    let bg_live_snap = bg.poll().unwrap();
    assert_eq!(bg_live_snap.frame_seq, paused_seq);

    // If we check the foreground's current frame at the same seq, it's the exact same Arc allocation!
    let _ = fg.poll();
    if let Some(fg_cur) = fg.current() {
        if fg_cur.frame_seq == bg_live_snap.frame_seq {
            assert!(
                Arc::ptr_eq(&fg_cur, &bg_live_snap),
                "foreground and backdrop must share the exact same Arc<DecodedFrame>"
            );
        }
    }

    // Resume while overview is open: backdrop now streams live frames!
    let before_resume_bg = player.status().backdrop_frames_published;
    player.set_paused(false);
    assert!(wait_until(Duration::from_secs(3), || {
        player.status().backdrop_frames_published >= before_resume_bg + 10
    }));

    // Close overview again: backdrop freezes on its current frame while foreground continues.
    player.set_overview_open(false);
    assert!(wait_until(Duration::from_secs(2), || {
        !player.status().overview_open
    }));
    let fg_at_close = player.status().foreground_frames_published;
    let bg_at_close = player.status().backdrop_frames_published;
    assert!(wait_until(Duration::from_secs(3), || {
        player.status().foreground_frames_published >= fg_at_close + 15
    }));
    assert!(
        player.status().backdrop_frames_published <= bg_at_close + 2,
        "backdrop should stop advancing after overview closes"
    );

    player.shutdown();
}

#[test]
fn looping_non_looping_seek_and_error_recovery() {
    let tmp = TempDir::new("seek-loop");
    let short_mp4 = tmp.file("short.mp4");
    generate_test_mp4(&short_mp4, "0.4", 25, "cyan"); // ~10 frames

    // 1. Looping=true decodes well past the 10-frame duration continuously.
    {
        let player = VideoPlayer::new(DecoderConfig {
            hw_accel: HwAccelPreference::SoftwareOnly,
            looping: true,
            pace_frames: false,
            ..Default::default()
        })
        .unwrap();
        player.open(&short_mp4);
        assert!(
            wait_until(Duration::from_secs(5), || player.status().frames_decoded >= 30),
            "expected seamless looping past 30 frames, got {:?}",
            player.status()
        );
        player.shutdown();
    }

    // 2. Looping=false ends cleanly, seek_ms rewinds and re-seeds backdrop.
    {
        let mut player = VideoPlayer::new(DecoderConfig {
            hw_accel: HwAccelPreference::SoftwareOnly,
            looping: false,
            overview_open: false,
            backdrop_seed_frames: 2,
            pace_frames: false,
            ..Default::default()
        })
        .unwrap();
        let mut bg = player.take_backdrop().unwrap();
        player.open(&short_mp4);
        assert!(
            wait_until(Duration::from_secs(5), || {
                player.status().state == PlaybackState::Ended
            }),
            "expected PlaybackState::Ended, got {:?}",
            player.status()
        );
        let _ = bg.poll();
        let bg_before_seek = player.status().backdrop_frames_published;

        player.seek_ms(100);
        assert!(wait_until(Duration::from_secs(3), || {
            player.status().backdrop_frames_published > bg_before_seek
                && player.status().state == PlaybackState::Ended
        }));
        assert!(bg.poll().is_some(), "seek should re-seed backdrop");
        player.shutdown();
    }

    // 3. Invalid path reports PlaybackState::Error without crashing, and recovers on next open.
    {
        let player = VideoPlayer::new(DecoderConfig {
            pace_frames: false,
            ..Default::default()
        })
        .unwrap();
        player.open(tmp.file("does-not-exist.mp4"));
        assert!(wait_until(Duration::from_secs(3), || {
            player.status().state == PlaybackState::Error
        }));
        assert!(player.status().last_error.is_some());

        player.open(&short_mp4);
        assert!(wait_until(Duration::from_secs(5), || player.is_ready()));
        player.shutdown();
    }
}

#[test]
fn cuda_first_preference_opens_and_decodes_frames() {
    let tmp = TempDir::new("cuda-first");
    let mp4 = tmp.file("clip.mp4");
    generate_test_mp4(&mp4, "0.6", 30, "orange");

    let mut player = VideoPlayer::new(DecoderConfig {
        hw_accel: HwAccelPreference::CudaFirst,
        cpu_format: CpuPixelFormat::Rgba8,
        pace_frames: false,
        ..Default::default()
    })
    .expect("create player with CudaFirst");
    let mut fg = player.take_foreground().unwrap();
    player.open(&mp4);

    assert!(
        wait_until(Duration::from_secs(10), || player.is_ready()),
        "expected player with CudaFirst to become ready, status={:?}",
        player.status()
    );

    let frame = loop {
        if let Some(f) = fg.poll() {
            break f;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(frame.dimensions(), (320, 180));
    match &frame.data {
        FrameData::Cpu(cpu) => assert!(!cpu.as_bytes().is_empty()),
        FrameData::DmaBuf(dma) => assert!(!dma.objects.is_empty()),
    }
    player.shutdown();
}
