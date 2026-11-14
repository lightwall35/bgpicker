"""Shared transition normalization, persistence and argv generation."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

PROJECT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("preferences", PROJECT / "wallpaper_settings.py")
PREFERENCES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREFERENCES)


class TransitionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bgpicker-transition-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = dict(os.environ, HOME=str(self.root / "home"),
                        XDG_CONFIG_HOME=str(self.root / "config"), XDG_CACHE_HOME=str(self.root / "cache"))
        self.config = self.root / "config/bgpicker/settings.json"

    def run_settings(self, *args, check=True):
        return subprocess.run(["python3", str(PROJECT / "wallpaper_settings.py"), *args],
                              env=self.env, check=check, capture_output=True, text=True, timeout=5)

    def write_config(self, value):
        self.config.parent.mkdir(parents=True, exist_ok=True)
        self.config.write_text(json.dumps(value))

    def test_all_choices_persist_without_overwriting_other_settings(self):
        self.write_config({"parallax": {"zoom": 1.25}, "custom": [1, 2],
                           "transition": {"durationMs": 900, "angle": 70}})
        for kind in PREFERENCES.TYPES:
            self.run_settings("set-transition", kind)
            config = json.loads(self.config.read_text())
            self.assertEqual(config["parallax"], {"zoom": 1.25})
            self.assertEqual(config["custom"], [1, 2])
            self.assertEqual(config["transition"], {"type": kind, "durationMs": 900, "angle": 70})
            self.assertEqual(json.loads(self.run_settings("show").stdout)["type"], kind)

    def test_resolved_random_and_shell_arguments_describe_one_effect(self):
        self.write_config({"transition": {"type": "random"}})
        for _ in range(12):
            lines = self.run_settings("resolve", "--shell").stdout.splitlines()
            snapshot, args = json.loads(lines[0]), lines[1:]
            self.assertIn(snapshot["type"], PREFERENCES.TYPES[:5])
            self.assertEqual(snapshot["selectedType"], "random")
            self.assertEqual(args[args.index("--transition-type") + 1], snapshot["type"])
            self.assertEqual(args[args.index("--transition-pos") + 1], "0.500000,0.500000")
        self.assertEqual(json.loads(self.config.read_text())["transition"]["type"], "random")

    def test_bad_json_is_reported_and_preserved(self):
        self.config.parent.mkdir(parents=True)
        self.config.write_text("{invalid")
        before = self.config.read_bytes()
        self.assertNotEqual(self.run_settings("set-transition", "wipe", check=False).returncode, 0)
        self.assertEqual(self.config.read_bytes(), before)
        self.assertNotEqual(self.run_settings("resolve", check=False).returncode, 0)

    def test_invalid_choice_cannot_create_configuration(self):
        self.assertNotEqual(self.run_settings("set-transition", "bad;command", check=False).returncode, 0)
        self.assertFalse(self.config.exists())

    def test_restore_and_zero_duration_are_instant_without_mutating_choice(self):
        self.write_config({"transition": {"type": "wave", "durationMs": 900}})
        snapshot = json.loads(self.run_settings("resolve", "--instant").stdout)
        self.assertEqual((snapshot["type"], snapshot["durationMs"], snapshot["selectedType"]), ("none", 0, "wave"))
        self.assertEqual(json.loads(self.run_settings("show").stdout)["type"], "wave")
        self.assertEqual(PREFERENCES.resolve({"transition": {"durationMs": 0}})["type"], "none")

    def test_numeric_bounds_defaults_and_legacy_duration(self):
        value = PREFERENCES.normalize({"transition": {"type": "bad", "durationMs": -20,
            "fps": 1000, "angle": "NaN", "position": [-2, 5], "wave": [0, 999]}})
        self.assertEqual(value, {"type": "fade", "durationMs": 0, "fps": 240, "angle": 45,
            "position": [0, 1], "wave": [1, 500]})
        self.assertEqual(PREFERENCES.normalize({"parallax": {"transitionMs": 350}})["durationMs"], 350)
        self.assertEqual(PREFERENCES.normalize({})["durationMs"], 800)

    def test_concurrent_preference_writes_leave_complete_json(self):
        self.write_config({"parallax": {"zoom": 1.3}, "untouched": True})
        processes = [subprocess.Popen(["python3", str(PROJECT / "wallpaper_settings.py"), "set-transition", kind],
                                     env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                     for kind in ("wave", "grow", "outer")]
        for process in processes:
            _, errors = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 0, errors)
        config = json.loads(self.config.read_text())
        self.assertTrue(config["untouched"])
        self.assertEqual(config["parallax"], {"zoom": 1.3})
        self.assertIn(config["transition"]["type"], ("wave", "grow", "outer"))
