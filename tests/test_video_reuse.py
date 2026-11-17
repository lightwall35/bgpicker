"""Transactions against a running QS renderer must preserve the visible choice."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

MODULE = Path(__file__).resolve().parents[1] / 'video_wallpaper.py'
spec = importlib.util.spec_from_file_location('video_wallpaper_reuse', MODULE)
video = importlib.util.module_from_spec(spec)
spec.loader.exec_module(video)


class VideoRendererReuseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.state = self.root / 'state'
        self.state.mkdir()
        self.old = str(self.root / 'old.mp4')
        self.new = str(self.root / 'new.mp4')
        self.blur = str(self.root / 'blur.mp4')
        self.record = {'pid': 4321, 'start': '1122', 'token': 'existing'}
        (self.state / 'video-manager.json').write_text(json.dumps(self.record))
        (self.state / 'video-wallpaper').write_text(self.old + '\n')
        (self.state / 'video-mode').write_text('normal\n')
        config = self.root / 'config/bgpicker/settings.json'
        config.parent.mkdir(parents=True)
        config.write_text(json.dumps({'transition': {'type': 'wave', 'durationMs': 600}}))
        for context in (patch.object(video, 'STATE', self.state),
                        patch.object(video, 'live_pid', return_value=True),
                        patch.object(video, 'prepare_blur', return_value=self.blur),
                        patch.object(video.time, 'sleep'),
                        patch.dict(os.environ, {'XDG_CONFIG_HOME': str(self.root / 'config')})):
            context.start()
            self.addCleanup(context.stop)

    def assert_old_selection(self):
        self.assertEqual((self.state / 'video-wallpaper').read_text(), self.old + '\n')
        self.assertEqual((self.state / 'video-mode').read_text(), 'normal\n')

    def test_first_frame_readiness_precedes_commit_and_selection_waits_for_transition(self):
        calls = []
        ready = False
        readiness_checks = 0
        completion_checks = 0
        def qs(*args):
            nonlocal ready, readiness_checks, completion_checks
            calls.append(args)
            self.assert_old_selection()
            if args[0] == 'status' and len(calls) == 1:
                return json.dumps({'apiVersion': 2, 'currentPath': self.old})
            if args[0] == 'stage':
                self.assertEqual(args, ('stage', self.new, self.blur, 'parallax'))
                return 'loading'
            if args[0] == 'ready':
                readiness_checks += 1
                ready = readiness_checks > 1
                return 'ready' if ready else 'loading'
            if args[0] == 'commit':
                self.assertTrue(ready, 'must decode the new video before committing')
                self.assertEqual(args[1], self.new)
                transition = json.loads(args[2])
                self.assertEqual(transition['type'], 'wave')
                self.assertEqual(transition['durationMs'], 600)
                return 'transitioning'
            if args[0] == 'status':
                completion_checks += 1
                return json.dumps({'apiVersion': 2, 'transitionActive': completion_checks == 1})
            self.fail(f'unexpected IPC: {args!r}')
        with patch.object(video, 'qs_video', side_effect=qs):
            self.assertTrue(video.reuse_renderer(self.new, 'parallax'))
        self.assertEqual([c[0] for c in calls], ['status', 'stage', 'ready', 'ready', 'commit', 'status', 'status'])
        self.assertEqual((self.state / 'video-wallpaper').read_text(), self.new + '\n')
        self.assertEqual((self.state / 'video-mode').read_text(), 'parallax\n')
        self.assertEqual(json.loads((self.state / 'video-manager.json').read_text()), self.record)

    def test_failed_readiness_cancels_staging_and_preserves_selection(self):
        calls = []
        def qs(*args):
            calls.append(args)
            if args[0] == 'status': return json.dumps({'apiVersion': 2, 'currentPath': self.old})
            if args[0] == 'stage': return 'loading'
            if args[0] == 'ready': return 'error: invalid video frame'
            if args[0] == 'cancel': return 'cancelled'
            self.fail(f'unexpected IPC: {args!r}')
        with patch.object(video, 'qs_video', side_effect=qs):
            with self.assertRaisesRegex(RuntimeError, 'invalid video frame'):
                video.reuse_renderer(self.new, 'parallax')
        self.assertEqual(calls[-1], ('cancel', self.new))
        self.assertNotIn('commit', [c[0] for c in calls])
        self.assert_old_selection()
        self.assertEqual(json.loads((self.state / 'video-manager.json').read_text()), self.record)

    def test_same_video_only_changes_mode_and_keeps_renderer(self):
        calls = []
        def qs(*args):
            calls.append(args)
            self.assert_old_selection()
            if args[0] == 'status':
                return json.dumps({'apiVersion': 2, 'currentPath': self.old, 'transitionActive': False})
            if args[0] == 'mode':
                self.assertEqual(args[1], 'parallax')
                return 'ready'
            self.fail(f'unexpected IPC: {args!r}')
        with patch.object(video, 'qs_video', side_effect=qs):
            self.assertTrue(video.reuse_renderer(self.old, 'parallax'))
        self.assertEqual([c[0] for c in calls], ['status', 'mode', 'status'])
        self.assertEqual((self.state / 'video-mode').read_text(), 'parallax\n')
        self.assertEqual(json.loads((self.state / 'video-manager.json').read_text()), self.record)

    def test_incompatible_renderer_returns_false_without_changing_selection(self):
        with patch.object(video, 'qs_video', return_value=json.dumps({'apiVersion': 1})) as qs:
            self.assertFalse(video.reuse_renderer(self.new, 'normal'))
        qs.assert_called_once_with('status')
        self.assert_old_selection()

    def test_transition_to_static_stages_commits_and_waits_for_completion(self):
        image = self.root / 'wall.png'
        image.write_bytes(b'png')
        blur = str(self.root / 'blur.png')
        (self.state / 'backdrop_parallax').write_text('on\n')
        calls = []
        ready = False
        readiness_checks = 0
        completion_checks = 0

        def qs(*args):
            nonlocal ready, readiness_checks, completion_checks
            calls.append(args)
            if args[0] == 'status' and len(calls) == 1:
                return json.dumps({'apiVersion': 2, 'ready': True})
            if args[0] == 'stageStatic':
                self.assertEqual(args, ('stageStatic', str(image.resolve()), blur, 'parallax', 'on'))
                return 'loading'
            if args[0] == 'readyStatic':
                readiness_checks += 1
                ready = readiness_checks > 1
                return 'ready' if ready else 'loading'
            if args[0] == 'commitStatic':
                self.assertTrue(ready)
                self.assertEqual(args[1], str(image.resolve()))
                transition = json.loads(args[2])
                self.assertEqual(transition['type'], 'wave')
                self.assertEqual(transition['durationMs'], 600)
                return 'ready'
            if args[0] == 'status':
                completion_checks += 1
                return json.dumps({'apiVersion': 2, 'transitionActive': completion_checks == 1})
            self.fail(f'unexpected IPC: {args!r}')

        with patch.object(video, 'prepare_static_blur', return_value=blur), \
             patch.object(video, 'qs_video', side_effect=qs):
            self.assertTrue(video.transition_to_static(str(image), 'parallax'))
        self.assertEqual([c[0] for c in calls],
                         ['status', 'stageStatic', 'readyStatic', 'readyStatic', 'commitStatic', 'status', 'status'])
        self.assert_old_selection()

    def test_transition_to_static_failure_cancels_staging(self):
        image = self.root / 'wall.png'
        image.write_bytes(b'png')
        blur = str(self.root / 'blur.png')
        calls = []

        def qs(*args):
            calls.append(args)
            if args[0] == 'status':
                return json.dumps({'apiVersion': 2, 'ready': True})
            if args[0] == 'stageStatic':
                return 'loading'
            if args[0] == 'readyStatic':
                return 'error: Qt could not load the static image'
            if args[0] == 'cancelStatic':
                return 'cancelled'
            self.fail(f'unexpected IPC: {args!r}')

        with patch.object(video, 'prepare_static_blur', return_value=blur), \
             patch.object(video, 'qs_video', side_effect=qs):
            with self.assertRaisesRegex(RuntimeError, 'Qt could not load'):
                video.transition_to_static(str(image), 'awww')
        self.assertEqual(calls[-1], ('cancelStatic', str(image.resolve())))
        self.assertNotIn('commitStatic', [c[0] for c in calls])

    def test_transition_to_static_returns_false_when_renderer_not_ready(self):
        image = self.root / 'wall.png'
        image.write_bytes(b'png')
        with patch.object(video, 'qs_video', return_value=json.dumps({'apiVersion': 2, 'ready': False})) as qs:
            self.assertFalse(video.transition_to_static(str(image), 'awww'))
        qs.assert_called_once_with('status')


if __name__ == '__main__':
    unittest.main()
