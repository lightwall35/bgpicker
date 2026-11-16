"""Exercise generated resources and backend integration without changing the desktop."""
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import unittest


PROJECT = Path(__file__).resolve().parents[1]


class WallpaperTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bgpicker-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.original = self.root / "original with spaces.png"
        self.env = dict(os.environ, HOME=str(self.root / "home"),
                        XDG_CACHE_HOME=str(self.root / "cache"),
                        XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_STATE_HOME=str(self.root / "state"),
                        BGPICKER_BLUR_SIZE="64x64", BGPICKER_BLUR_SIGMA="3")
        self.run_command(["magick", "-size", "128x96", "pattern:checkerboard", str(self.original)])

    def run_command(self, command, *, check=True, env=None):
        return subprocess.run(command, env=env or self.env, capture_output=True,
                              text=True, timeout=10, check=check)

    def prepare(self, env=None):
        return Path(self.run_command([str(PROJECT / "prepare_wallpaper.sh"),
                                      str(self.original)], env=env).stdout.rstrip("\n"))

    def mock_backends(self):
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        self.env["PATH"] = str(bin_dir) + os.pathsep + os.environ["PATH"]
        self.env["MOCK_LOG"] = str(self.root / "backends.jsonl")
        self.env["MOCK_PID"] = str(self.root / "swaybg.pid")
        self.env["MOCK_AWWW_DISPLAY"] = str(self.original)
        stub = '''#!/usr/bin/python3
import json, os, pathlib, sys, time
name = pathlib.Path(sys.argv[0]).name
if name == "pgrep":
    sys.exit(1)
with open(os.environ["MOCK_LOG"], "a") as f:
    f.write(json.dumps([name, sys.argv[1:]]) + "\\n")
if name == "qs":
    if "call" not in sys.argv:
        sys.exit(0)
    target = "backdropBackend" if "backdropBackend" in sys.argv else "wallpaperBackend"
    method = sys.argv[sys.argv.index(target) + 1]
    if os.environ.get("MOCK_QS_FAIL"):
        sys.exit(1)
    if method == "stage": print("loading")
    elif method == "ready": print(os.environ.get("MOCK_QS_READY", "ready"))
    elif method == "commit": print(os.environ.get("MOCK_QS_COMMIT", "ready"))
    elif method == "status": print(json.dumps({"active": os.environ.get("MOCK_RENDERER_ACTIVE") == "1"}))
    elif method == "exitMode": print("started")
    elif method == "modeStatus": print(os.environ.get("MOCK_MODE_STATUS", "ready"))
    elif method == "cancelMode": print("cancelled")
    else: print(os.environ.get("MOCK_QS_DEACTIVATE", "awww"))
    sys.exit(0)
if name == "niri":
    print(json.dumps({"eDP-2": {"logical": {"x": 0}}}))
    sys.exit(0)
if name == "awww":
    if "--version" in sys.argv:
        print(os.environ.get("MOCK_AWWW_VERSION", "unknown"))
        sys.exit(0)
    if "query" in sys.argv and "--json" not in sys.argv and os.environ.get("MOCK_AWWW_ABSENT"):
        sys.exit(1)
    if "query" in sys.argv and "--json" in sys.argv:
        calls = [json.loads(line) for line in pathlib.Path(os.environ["MOCK_LOG"]).read_text().splitlines()]
        images = [c[1][1] for c in calls if c[0] == "awww" and c[1][0] == "img" and "--namespace" not in c[1]]
        queries = sum(c[0] == "awww" and "query" in c[1] and "--json" in c[1] for c in calls)
        display = images[-1] if images else os.environ.get("MOCK_AWWW_DISPLAY", "")
        if not images and queries <= int(os.environ.get("MOCK_AWWW_DELAY_QUERIES", "0")): display = ""
        print(json.dumps({"": [{"displaying": {"image": display}}]}))
        sys.exit(0)
    if "--namespace" in sys.argv and "img" in sys.argv and os.environ.get("MOCK_BACKDROP_FAIL"):
        sys.exit(1)
    if "img" not in sys.argv or "--namespace" in sys.argv:
        sys.exit(0)
if name == "swaybg":
    if os.environ.get("MOCK_SWAYBG_FAIL"):
        sys.exit(1)
    with open(os.environ["MOCK_PID"], "a") as f:
        f.write(str(os.getpid()) + "\\n")
    time.sleep(30)
sys.exit(int(os.environ.get("MOCK_AWWW_EXIT", "0")))
'''
        for name in ["awww", "awww-daemon", "niri", "swaybg", "pgrep", "qs"]:
            command = bin_dir / name
            command.write_text(stub)
            command.chmod(0o755)
        self.addCleanup(self.stop_mock_swaybg)

    def stop_mock_swaybg(self):
        path = self.root / "swaybg.pid"
        if path.exists():
            for pid in path.read_text().splitlines():
                try:
                    os.kill(int(pid), signal.SIGTERM)
                except ProcessLookupError:
                    pass

    def test_blur_is_downsized_cached_and_does_not_modify_original(self):
        original_bytes = self.original.read_bytes()
        blurred = self.prepare()
        self.assertEqual(blurred.parent, self.root / "cache/bgpicker/blur")
        self.assertEqual(self.run_command(["magick", "identify", "-format", "%wx%h",
                                          str(blurred)]).stdout, "64x48")
        modified = blurred.stat().st_mtime_ns
        self.assertEqual(self.prepare(), blurred)
        self.assertEqual(blurred.stat().st_mtime_ns, modified)
        self.assertEqual(self.original.read_bytes(), original_bytes)
        def contrast(path):
            return float(self.run_command(["magick", str(path), "-format",
                                            "%[fx:standard_deviation]", "info:"]).stdout)
        self.assertLess(contrast(blurred), contrast(self.original))

    def test_source_contents_and_processing_settings_invalidate_cache(self):
        first = self.prepare()
        self.run_command(["magick", "-size", "128x96", "xc:red", str(self.original)])
        changed_source = self.prepare()
        self.assertNotEqual(first, changed_source)
        changed_sigma = self.prepare(dict(self.env, BGPICKER_BLUR_SIGMA="5"))
        self.assertNotEqual(changed_source, changed_sigma)
        changed_size = self.prepare(dict(self.env, BGPICKER_BLUR_SIZE="32x32"))
        self.assertNotEqual(changed_source, changed_size)

    def test_concurrent_generation_publishes_one_complete_image(self):
        command = [str(PROJECT / "prepare_wallpaper.sh"), str(self.original)]
        with subprocess.Popen(command, env=self.env, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True) as first:
            second = self.run_command(command)
            stdout, stderr = first.communicate(timeout=10)
            self.assertEqual(first.returncode, 0, stderr)
        self.assertEqual(stdout, second.stdout)
        self.assertEqual(len(list((self.root / "cache/bgpicker/blur").glob("*.png"))), 1)

    def test_bad_input_is_not_published_as_a_cached_image(self):
        self.original.write_text("not an image")
        result = self.run_command([str(PROJECT / "prepare_wallpaper.sh"),
                                   str(self.original)], check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list((self.root / "cache/bgpicker/blur").glob("*.png")), [])
        missing = self.run_command([str(PROJECT / "prepare_wallpaper.sh")], check=False)
        self.assertNotEqual(missing.returncode, 0)

    def test_switch_applies_generated_image_and_records_original(self):
        self.mock_backends()
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), str(self.original)])
        source_state = self.root / "state/bgpicker/current_wallpaper"
        background_state = self.root / "home/.cache/current_swaybg_img"
        self.assertEqual(source_state.read_text(), str(self.original) + "\n")
        blurred = Path(background_state.read_text().rstrip("\n"))
        self.assertTrue(blurred.is_file())
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        self.assertTrue(any(c[0] == "awww" and c[1][:2] == ["img", str(self.original)] for c in calls))
        self.assertTrue(any(c[0] == "awww" and "--namespace" in c[1] and str(blurred) in c[1] for c in calls))
        # A surviving background process must not keep the switch lock held.
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), str(self.original)])

    def test_backend_failure_preserves_previous_selection(self):
        self.mock_backends()
        self.env["MOCK_AWWW_EXIT"] = "1"
        state = self.root / "state/bgpicker/current_wallpaper"
        state.parent.mkdir(parents=True)
        state.write_text("previous-original\n")
        background = self.root / "home/.cache/current_swaybg_img"
        background.parent.mkdir(parents=True)
        background.write_text("previous-background\n")
        result = self.run_command([str(PROJECT / "switch_wallpaper.sh"),
                                   str(self.original)], check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(state.read_text(), "previous-original\n")
        self.assertEqual(background.read_text(), "previous-background\n")

    def test_backdrop_failure_does_not_publish_selection(self):
        self.mock_backends()
        self.env["MOCK_BACKDROP_FAIL"] = "1"
        result = self.run_command([str(PROJECT / "switch_wallpaper.sh"),
                                   str(self.original)], check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "state/bgpicker/current_wallpaper").exists())

    def test_backdrop_failure_restores_previous_clear_wallpaper(self):
        self.mock_backends()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("awww\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        new_image = self.root / "new.png"
        self.run_command(["magick", "-size", "64x64", "xc:blue", str(new_image)])
        self.env["MOCK_BACKDROP_FAIL"] = "1"
        result = self.run_command([str(PROJECT / "switch_wallpaper.sh"), str(new_image)], check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((state / "current_wallpaper").read_text(), str(self.original) + "\n")
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        self.assertIn(["awww", ["img", str(self.original), "--transition-type", "none"]], calls)

    def test_parallax_stages_and_commits_without_calling_awww(self):
        self.mock_backends()
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "parallax", str(self.original)])
        self.assertEqual((self.root / "state/bgpicker/mode").read_text(), "parallax\n")
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        self.assertFalse(any(c[0] == "awww" and c[1][0] == "img" and "--namespace" not in c[1] for c in calls))
        self.assertIn(["awww", ["clear", "00000000"]], calls)
        methods = [c[1][c[1].index("wallpaperBackend") + 1] for c in calls if c[0] == "qs"]
        self.assertEqual(methods, ["stage", "ready", "commit", "modeStatus"])

    def test_mode_switch_waits_for_enter_before_clearing_awww(self):
        self.mock_backends()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("awww\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "parallax"])
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        commit = next(i for i, c in enumerate(calls) if c[0] == "qs" and "commit" in c[1])
        status = next(i for i, c in enumerate(calls) if c[0] == "qs" and "modeStatus" in c[1])
        clear = next(i for i, c in enumerate(calls) if c[0] == "awww" and c[1][0] == "clear")
        self.assertLess(commit, status)
        self.assertLess(status, clear)
        self.assertEqual(json.loads(calls[commit][1][-1])["modeSwitch"], "enter")

    def test_mode_switch_waits_for_exit_before_deactivation(self):
        self.mock_backends()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("parallax\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "awww"])
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        methods = [c[1][c[1].index("wallpaperBackend") + 1] for c in calls if c[0] == "qs"]
        self.assertEqual(methods, ["exitMode", "modeStatus", "deactivate"])
        main = next(c[1] for c in calls if c[0] == "awww" and c[1][:2] == ["img", str(self.original)])
        self.assertEqual(main[main.index("--transition-type") + 1], "none")

    def test_mode_exit_failure_keeps_previous_mode(self):
        self.mock_backends()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("parallax\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.env["MOCK_MODE_STATUS"] = "error: display changed"
        result = self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "awww"], check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((state / "mode").read_text(), "parallax\n")
        self.assertIn('"cancelMode"', (self.root / "backends.jsonl").read_text())

    def test_both_modes_receive_the_same_transition_snapshot(self):
        self.mock_backends()
        config = self.root / "config/bgpicker/settings.json"
        config.parent.mkdir(parents=True)
        config.write_text(json.dumps({"transition": {"type": "wave", "durationMs": 650,
            "fps": 90, "angle": 30, "position": [0.2, 0.3], "wave": [30, 15]}}))
        for mode in ("awww", "parallax"):
            self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", mode, str(self.original)])
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        awww = next(c[1] for c in calls if c[0] == "awww" and c[1][0] == "img" and "--namespace" not in c[1])
        backdrop = next(c[1] for c in calls if c[0] == "awww" and "--namespace" in c[1] and c[1][0] == "img")
        self.assertEqual(backdrop[backdrop.index("--transition-type") + 1], "wave")
        self.assertEqual(backdrop[backdrop.index("--transition-duration") + 1], "0.65")
        commit = next(c[1] for c in calls if c[0] == "qs" and "commit" in c[1])
        snapshot = json.loads(commit[-1])
        self.assertEqual(awww[awww.index("--transition-type") + 1], snapshot["type"])
        self.assertEqual(float(awww[awww.index("--transition-duration") + 1]) * 1000, snapshot["durationMs"])
        self.assertEqual(snapshot["type"], "wave")
        self.assertEqual(snapshot["position"], [0.2, 0.3])
        self.assertEqual(snapshot["wave"], [30, 15])
        self.assertIn("--invert-y", awww)

    def test_parallax_load_failure_preserves_mode_and_selection(self):
        self.mock_backends()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("awww\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.env["MOCK_QS_READY"] = "error: image failed"
        result = self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "parallax"], check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((state / "mode").read_text(), "awww\n")
        self.assertEqual((state / "current_wallpaper").read_text(), str(self.original) + "\n")
        self.assertIn('"cancel"', (self.root / "backends.jsonl").read_text())

    def test_toggle_returns_to_original_backend(self):
        self.mock_backends()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("parallax\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "toggle"])
        self.assertEqual((state / "mode").read_text(), "awww\n")
        calls = (self.root / "backends.jsonl").read_text()
        self.assertIn('"awww"', calls)
        self.assertIn('"deactivate"', calls)

    def test_commit_failure_keeps_awww_visible(self):
        self.mock_backends()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("awww\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.env["MOCK_QS_COMMIT"] = "error: screen changed"
        result = self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "parallax"], check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((state / "mode").read_text(), "awww\n")
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        self.assertFalse(any(c[0] == "awww" and c[1][0] == "clear" for c in calls))
        self.assertFalse(any(c[0] == "awww" and c[1][0] == "img" and "--namespace" not in c[1] for c in calls))

    def test_deactivate_failure_does_not_claim_original_mode(self):
        self.mock_backends()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("parallax\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.env["MOCK_QS_DEACTIVATE"] = "Not ready to accept queries yet."
        result = self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "awww"], check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((state / "mode").read_text(), "parallax\n")

    def test_restore_regenerates_missing_blur_and_reuses_saved_mode(self):
        self.mock_backends()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("parallax\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--restore"])
        blur = Path((self.root / "home/.cache/current_swaybg_img").read_text().strip())
        self.assertTrue(blur.is_file())
        self.assertIn('"commit"', (self.root / "backends.jsonl").read_text())

    def test_restore_commits_before_clearing_startup_wallpaper(self):
        self.mock_backends()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("parallax\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--restore"])
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        commit = next(i for i, c in enumerate(calls) if c[0] == "qs" and "wallpaperBackend" in c[1] and "commit" in c[1])
        mode_status = next(i for i, c in enumerate(calls) if c[0] == "qs" and "wallpaperBackend" in c[1] and "modeStatus" in c[1])
        clear = next(i for i, c in enumerate(calls) if c[0] == "awww" and c[1] == ["clear", "00000000"])
        self.assertEqual(json.loads(calls[commit][1][-1])["modeSwitch"], "enter")
        self.assertEqual(json.loads(calls[commit][1][-1])["durationMs"], 0)
        self.assertLess(commit, mode_status)
        self.assertLess(mode_status, clear)
        self.assertFalse(any(c[0] == "awww" and c[1][0] == "img" and "--namespace" not in c[1] for c in calls))

    def test_restore_active_renderer_does_not_restart_mode_animation(self):
        self.mock_backends()
        self.env["MOCK_RENDERER_ACTIVE"] = "1"
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("parallax\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--restore"])
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        commit = next(c[1] for c in calls if c[0] == "qs" and "wallpaperBackend" in c[1] and "commit" in c[1])
        self.assertNotIn("modeSwitch", json.loads(commit[-1]))
        self.assertFalse(any(c[0] == "qs" and "wallpaperBackend" in c[1] and "modeStatus" in c[1] for c in calls))
        self.assertFalse(any(c[0] == "awww" and "query" in c[1] and "--json" in c[1] for c in calls))

    def test_restore_animation_failure_keeps_awww_visible(self):
        self.mock_backends()
        self.env["MOCK_MODE_STATUS"] = "error: display changed"
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("parallax\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        result = self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--restore"], check=False)
        self.assertNotEqual(result.returncode, 0)
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        self.assertTrue(any(c[0] == "qs" and "wallpaperBackend" in c[1] and "deactivate" in c[1] for c in calls))
        self.assertFalse(any(c[0] == "awww" and c[1] == ["clear", "00000000"] for c in calls))
        self.assertEqual((state / "mode").read_text(), "parallax\n")

    def test_restore_waits_for_delayed_cached_image(self):
        self.mock_backends()
        self.env["MOCK_AWWW_DELAY_QUERIES"] = "2"
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("parallax\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--restore"])
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        queries = [i for i, c in enumerate(calls) if c[0] == "awww" and "query" in c[1] and "--json" in c[1]]
        commit = next(i for i, c in enumerate(calls) if c[0] == "qs" and "wallpaperBackend" in c[1] and "commit" in c[1])
        self.assertGreaterEqual(len(queries), 3)
        self.assertLess(queries[2], commit)
        self.assertFalse(any(c[0] == "awww" and c[1][0] == "img" and "--namespace" not in c[1] for c in calls))

    def test_restore_falls_back_when_cached_image_is_stale(self):
        self.mock_backends()
        self.env["MOCK_AWWW_DISPLAY"] = "/stale.png"
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("parallax\n")
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--restore"])
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        image = next(i for i, c in enumerate(calls) if c == ["awww", ["img", str(self.original), "--transition-type", "none"]])
        commit = next(i for i, c in enumerate(calls) if c[0] == "qs" and "wallpaperBackend" in c[1] and "commit" in c[1])
        self.assertLess(image, commit)

    def test_restore_without_selection_is_noop(self):
        self.mock_backends()
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--restore"])
        self.assertFalse((self.root / "backends.jsonl").exists())

    def test_backdrop_parallax_enable_switch_and_disable(self):
        self.mock_backends()
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), str(self.original)])
        script = str(PROJECT / "backdrop_parallax.sh")
        self.run_command([script, "on"])
        state = self.root / "state/bgpicker/backdrop_parallax"
        self.assertEqual(state.read_text(), "on\n")
        new_image = self.root / "second.png"
        self.run_command(["magick", "-size", "128x96", "xc:red", str(new_image)])
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), str(new_image)])
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        commits = [c[1] for c in calls if c[0] == "qs" and "backdropBackend" in c[1] and "commit" in c[1]]
        self.assertEqual(len(commits), 2)
        self.assertEqual(json.loads(commits[0][-1])["modeSwitch"], "enter")
        self.assertNotIn("modeSwitch", json.loads(commits[1][-1]))
        self.run_command([script, "off"])
        self.assertEqual(state.read_text(), "off\n")
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        methods = [c[1][c[1].index("backdropBackend") + 1] for c in calls if c[0] == "qs" and "backdropBackend" in c[1]]
        self.assertEqual(methods[-3:], ["exitMode", "modeStatus", "deactivate"])

    def test_backdrop_enable_failure_keeps_saved_state(self):
        self.mock_backends()
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), str(self.original)])
        self.env["MOCK_QS_READY"] = "error: image failed"
        result = self.run_command([str(PROJECT / "backdrop_parallax.sh"), "on"], check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "state/bgpicker/backdrop_parallax").exists())
        self.assertIn('"cancel"', (self.root / "backends.jsonl").read_text())

    def test_backdrop_parallax_restored_and_survives_main_mode_change(self):
        self.mock_backends()
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), str(self.original)])
        self.run_command([str(PROJECT / "backdrop_parallax.sh"), "on"])
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "parallax"])
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--restore"])
        calls = [json.loads(line) for line in (self.root / "backends.jsonl").read_text().splitlines()]
        commits = [c[1] for c in calls if c[0] == "qs" and "backdropBackend" in c[1] and "commit" in c[1]]
        self.assertEqual(len(commits), 2)  # enable and restore; no replay for the mode switch
        self.assertEqual(json.loads(commits[-1][-1])["durationMs"], 0)
        self.assertEqual((self.root / "state/bgpicker/backdrop_parallax").read_text(), "on\n")

    def test_missing_daemon_or_invalid_mode_does_not_publish_mode(self):
        self.mock_backends()
        self.env["MOCK_QS_FAIL"] = "1"
        for mode in ("parallax", "invalid"):
            result = self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", mode, str(self.original)], check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((self.root / "state/bgpicker/mode").exists())


if __name__ == "__main__":
    unittest.main()
