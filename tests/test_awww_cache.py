"""Test the versioned startup cache adapter with isolated synthetic records."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

PROJECT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('sync_awww_cache', PROJECT / 'sync_awww_cache.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CacheTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.image = self.root / 'image with spaces.png'
        self.image.write_bytes(b'image')
        self.cache = self.root / 'awww/0.12.1'
        self.cache.mkdir(parents=True)
        env = patch.dict(os.environ, {'XDG_CACHE_HOME': str(self.root)})
        env.start()
        self.addCleanup(env.stop)

    @staticmethod
    def run_stub(cmd, **kwargs):
        if cmd[:2] == ['awww', '--version']:
            return subprocess.CompletedProcess(cmd, 0, 'awww 0.12.1\n', '')
        return subprocess.CompletedProcess(cmd, 0, json.dumps({'eDP-2': {'logical': {'x': 0}}}), '')

    def test_updates_connected_and_old_outputs_preserves_other_namespaces(self):
        other = b'bgpicker-backdrop\0crop:center\0Lanczos3\0/blur.png'
        (self.cache / 'eDP-2').write_bytes(b'\0crop:center\0Lanczos3\0/old.png\0' + other)
        (self.cache / 'HDMI-A-1').write_bytes(b'\0fit\0Bilinear\0/old.png')
        (self.cache / '_animation').write_bytes(b'\xff\x00binary')
        with patch.object(module.subprocess, 'run', side_effect=self.run_stub):
            module.sync(self.image)
        image = str(self.image).encode()
        self.assertEqual((self.cache / 'eDP-2').read_bytes(), b'\0crop:center\0Lanczos3\0' + image + b'\0' + other)
        self.assertEqual((self.cache / 'HDMI-A-1').read_bytes(), b'\0fit\0Bilinear\0' + image)
        self.assertEqual((self.cache / '_animation').read_bytes(), b'\xff\x00binary')

    def test_adds_record_for_new_output(self):
        with patch.object(module.subprocess, 'run', side_effect=self.run_stub):
            module.sync(self.image)
        self.assertEqual((self.cache / 'eDP-2').read_bytes(), b'\0crop:center\0Lanczos3\0' + str(self.image).encode())

    def test_unknown_version_does_not_touch_cache(self):
        with patch.object(module.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, 'awww 1.0.0\n', '')):
            with self.assertRaises(ValueError):
                module.sync(self.image)
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_malformed_connected_output_is_not_overwritten(self):
        path = self.cache / 'eDP-2'
        path.write_bytes(b'broken')
        with patch.object(module.subprocess, 'run', side_effect=self.run_stub):
            with self.assertRaises(ValueError):
                module.sync(self.image)
        self.assertEqual(path.read_bytes(), b'broken')

if __name__ == '__main__':
    unittest.main()
