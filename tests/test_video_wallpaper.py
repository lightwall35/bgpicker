import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch, Mock

MODULE = Path(__file__).resolve().parents[1] / 'video_wallpaper.py'
spec = importlib.util.spec_from_file_location('video_wallpaper', MODULE)
video = importlib.util.module_from_spec(spec)
spec.loader.exec_module(video)



class VideoWallpaperTests(unittest.TestCase):
    def test_apply_parallax_commits_only_when_renderer_is_ready(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / 'example.mp4'
            source.write_bytes(b'video')
            runtime, state = root / 'runtime', root / 'state'
            def spawn(args, **kwargs):
                status = runtime / args[-1] / 'status'
                status.parent.mkdir(parents=True, exist_ok=True)
                status.write_text('ready\n')
                return Mock(pid=1234)
            with patch.object(video, 'CACHE', root / 'cache'), patch.object(video, 'STATE', state), \
                 patch.object(video, 'RUNTIME', runtime), patch.object(video.subprocess, 'Popen', spawn), \
                 patch.object(video, 'stop_record'), patch.object(video, 'process_start', return_value='42'):
                video.apply(str(source), 'parallax')
                self.assertEqual((state / 'video-mode').read_text().strip(), 'parallax')
                self.assertEqual((state / 'video-wallpaper').read_text().strip(), str(source))
                self.assertEqual(json.loads((state / 'video-manager.json').read_text())['pid'], 1234)

    def test_failed_renderer_keeps_previous_video_selection(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / 'example.mp4'; source.write_bytes(b'video')
            runtime, state = root / 'runtime', root / 'state'
            state.mkdir(); (state / 'video-wallpaper').write_text('previous\n')
            def spawn(args, **kwargs):
                status = runtime / args[-1] / 'status'
                status.parent.mkdir(parents=True, exist_ok=True)
                status.write_text('error: decode failed\n')
                return Mock(pid=1234)
            with patch.object(video, 'CACHE', root / 'cache'), patch.object(video, 'STATE', state), \
                 patch.object(video, 'RUNTIME', runtime), patch.object(video.subprocess, 'Popen', spawn), \
                 patch.object(video, 'stop_record') as stop:
                with self.assertRaisesRegex(RuntimeError, 'decode failed'):
                    video.apply(str(source), 'parallax')
                self.assertEqual((state / 'video-wallpaper').read_text(), 'previous\n')
                stop.assert_not_called()

    def test_pid_identity_rejects_reused_start_time(self):
        start = video.process_start(os.getpid())
        self.assertTrue(video.live_pid(os.getpid(), start))
        self.assertFalse(video.live_pid(os.getpid(), str(int(start) + 1)))

    def test_video_thumbnail_is_cached_and_invalidated_when_source_changes(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / 'video.mp4'
            source.write_bytes(b'one')
            calls = []
            def fake_ffmpeg(args, **kwargs):
                calls.append(args)
                Path(args[-1]).write_bytes(b'jpeg')
                return type('Result', (), {'returncode': 0, 'stderr': ''})()
            with patch.object(video, 'CACHE', root / 'cache'), patch.object(video.subprocess, 'run', fake_ffmpeg):
                first = video.prepare_thumbnail(source)
                self.assertEqual(video.prepare_thumbnail(source), first)
                self.assertEqual(len(calls), 1)
                source.write_bytes(b'changed-content')
                self.assertNotEqual(video.prepare_thumbnail(source), first)
                self.assertEqual(len(calls), 2)

    def test_blur_cache_reuses_same_content_and_changes_with_source(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / 'example.mp4'
            source.write_bytes(b'first')
            calls = []
            def ffmpeg(args, **kwargs):
                calls.append(args)
                Path(args[-1]).write_bytes(b'blurred video')
                return type('Result', (), {'returncode': 0, 'stderr': ''})()
            with patch.object(video, 'CACHE', root / 'cache'), patch.object(video.subprocess, 'run', ffmpeg):
                first = video.prepare_blur(str(source))
                self.assertEqual(video.prepare_blur(str(source)), first)
                self.assertEqual(len(calls), 1)
                source.write_bytes(b'changed')
                self.assertNotEqual(video.prepare_blur(str(source)), first)
                self.assertEqual(len(calls), 2)
                self.assertIn('fps=30,scale=1920:-2,gblur=sigma=30', calls[0])

    def test_blur_failure_does_not_leave_partial_cache(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / 'example.mp4'; source.write_bytes(b'bad')
            def fail(args, **kwargs):
                Path(args[-1]).write_bytes(b'incomplete')
                return type('Result', (), {'returncode': 1, 'stderr': 'decode error'})()
            with patch.object(video, 'CACHE', root / 'cache'), patch.object(video.subprocess, 'run', fail):
                with self.assertRaisesRegex(RuntimeError, 'decode error'):
                    video.prepare_blur(str(source))
                self.assertEqual(list((root / 'cache/video-blur').iterdir()), [])


if __name__ == '__main__': unittest.main()
