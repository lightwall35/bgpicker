"""Exercise blur cache generation, dual-swaybg simple mode, and bgpickerctl shims."""
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest


PROJECT = Path(__file__).resolve().parents[1]


class WallpaperTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bgpicker-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.original = self.root / "original with spaces.png"
        self.env = dict(
            os.environ,
            HOME=str(self.root / "home"),
            XDG_CACHE_HOME=str(self.root / "cache"),
            XDG_CONFIG_HOME=str(self.root / "config"),
            XDG_STATE_HOME=str(self.root / "state"),
            BGPICKER_BLUR_SIZE="64x64",
            BGPICKER_BLUR_SIGMA="3",
        )
        self.env.pop("BGPICKERCTL", None)
        self.run_command(["magick", "-size", "128x96", "pattern:checkerboard", str(self.original)])

    def run_command(self, command, *, check=True, env=None):
        return subprocess.run(
            command,
            env=env or self.env,
            capture_output=True,
            text=True,
            timeout=10,
            check=check,
        )

    def prepare(self, env=None):
        return Path(
            self.run_command([str(PROJECT / "prepare_wallpaper.sh"), str(self.original)], env=env)
            .stdout.rstrip("\n")
        )

    def mock_ctl_and_qs(self):
        bin_dir = self.root / "bin"
        bin_dir.mkdir(exist_ok=True)
        self.env["PATH"] = str(bin_dir) + os.pathsep + os.environ["PATH"]
        self.env["MOCK_LOG"] = str(self.root / "calls.jsonl")
        self.env["MOCK_QS_ALIVE"] = str(self.root / "qs.alive")

        ctl_stub = """#!/usr/bin/python3
import json, os, sys
with open(os.environ["MOCK_LOG"], "a") as f:
    f.write(json.dumps(["bgpickerctl", sys.argv[1:]]) + "\\n")
if os.environ.get("MOCK_CTL_FAIL"):
    sys.stderr.write("mock bgpickerctl error\\n")
    sys.exit(int(os.environ.get("MOCK_CTL_EXIT", "1")))
args = sys.argv[1:]
if args == ["engine", "status"]:
    print(os.environ.get("MOCK_ENGINE_STATUS", "full"))
elif args == ["mode"]:
    print(os.environ.get("MOCK_MODE_STATUS", "awww"))
elif args == ["backdrop-parallax"]:
    print(os.environ.get("MOCK_BACKDROP_STATUS", "off"))
elif args == ["transition"]:
    print(json.dumps({"type": "fade"}))
elif args == ["video", "status"]:
    print("engine: video\\nmode: normal\\nvideo: /tmp/demo.mp4")
"""
        qs_stub = """#!/usr/bin/python3
import json, os, pathlib, sys
with open(os.environ["MOCK_LOG"], "a") as f:
    f.write(json.dumps(["qs", sys.argv[1:]]) + "\\n")
alive = pathlib.Path(os.environ["MOCK_QS_ALIVE"])
args = sys.argv[1:]
if args[:1] == ["ipc"] and "show" in args:
    sys.exit(0 if alive.exists() else 1)
if args[:1] == ["kill"]:
    alive.unlink(missing_ok=True)
    sys.exit(0)
if "-d" in args:
    alive.write_text("1")
    sys.exit(0)
"""
        for name, body in [("bgpickerctl", ctl_stub), ("qs", qs_stub)]:
            path = bin_dir / name
            path.write_text(body)
            path.chmod(0o755)

    def mock_swaybg(self):
        bin_dir = self.root / "bin"
        bin_dir.mkdir(exist_ok=True)
        if str(bin_dir) not in self.env.get("PATH", "").split(os.pathsep):
            self.env["PATH"] = str(bin_dir) + os.pathsep + self.env.get("PATH", os.environ["PATH"])
        self.env.setdefault("MOCK_LOG", str(self.root / "calls.jsonl"))
        self.env["MOCK_SWAYBG_PID"] = str(self.root / "swaybg.pids")
        self.addCleanup(self.stop_mock_swaybg)

        stub_src = self.root / "swaybg_stub.c"
        stub_src.write_text(
            r"""
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

static volatile sig_atomic_t running = 1;
static void on_term(int sig) { (void)sig; running = 0; }
static const char namespace_tag[] = "wallpaper";

static void write_json_str(FILE *f, const char *s) {
    fputc('"', f);
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        if (*p == '"' || *p == '\\') { fputc('\\', f); fputc(*p, f); }
        else if (*p == '\n') { fputs("\\n", f); }
        else { fputc(*p, f); }
    }
    fputc('"', f);
}

int main(int argc, char **argv) {
    signal(SIGTERM, on_term);
    volatile const char *ns = namespace_tag;
    if (getenv("MOCK_SWAYBG_FAIL")) return 1;
    if (getenv("MOCK_SWAYBG_FAIL_FRONT") && ns[0] == 'b') return 1;
    const char *log = getenv("MOCK_LOG");
    if (log) {
        FILE *f = fopen(log, "a");
        if (f) {
            fprintf(f, "[");
            write_json_str(f, namespace_tag);
            fprintf(f, ", [");
            for (int i = 1; i < argc; i++) {
                if (i > 1) fprintf(f, ", ");
                write_json_str(f, argv[i]);
            }
            fprintf(f, "]]\n");
            fclose(f);
        }
    }
    const char *pidf = getenv("MOCK_SWAYBG_PID");
    if (pidf) {
        FILE *f = fopen(pidf, "a");
        if (f) { fprintf(f, "%d\n", getpid()); fclose(f); }
    }
    while (running) pause();
    return 0;
}
"""
        )
        self.run_command(["cc", str(stub_src), "-o", str(bin_dir / "swaybg")])

    def stop_mock_swaybg(self):
        pid_file = self.root / "swaybg.pids"
        if not pid_file.exists():
            return
        for line in pid_file.read_text().splitlines():
            if not line.strip():
                continue
            try:
                os.kill(int(line), signal.SIGTERM)
            except (ProcessLookupError, ValueError):
                pass

    def pid_alive(self, pid):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return False
        stat_path = Path(f"/proc/{pid}/stat")
        if stat_path.exists():
            fields = stat_path.read_text().split(")")[-1].split()
            if fields and fields[0] == "Z":
                return False
        return True

    def wait_pid_dead(self, pid, timeout=2.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.pid_alive(pid):
                return True
            time.sleep(0.05)
        return not self.pid_alive(pid)

    def read_calls(self):
        log = self.root / "calls.jsonl"
        if not log.exists():
            return []
        return [json.loads(line) for line in log.read_text().splitlines()]

    def test_blur_is_downsized_cached_and_does_not_modify_original(self):
        original_bytes = self.original.read_bytes()
        blurred = self.prepare()
        self.assertEqual(blurred.parent, self.root / "cache/bgpicker/blur")
        self.assertEqual(
            self.run_command(["magick", "identify", "-format", "%wx%h", str(blurred)]).stdout,
            "64x48",
        )
        modified = blurred.stat().st_mtime_ns
        self.assertEqual(self.prepare(), blurred)
        self.assertEqual(blurred.stat().st_mtime_ns, modified)
        self.assertEqual(self.original.read_bytes(), original_bytes)

        def contrast(path):
            return float(
                self.run_command(["magick", str(path), "-format", "%[fx:standard_deviation]", "info:"]).stdout
            )

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
        with subprocess.Popen(
            command,
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        ) as first:
            second = self.run_command(command)
            stdout, stderr = first.communicate(timeout=10)
            self.assertEqual(first.returncode, 0, stderr)
        self.assertEqual(stdout, second.stdout)
        self.assertEqual(len(list((self.root / "cache/bgpicker/blur").glob("*.png"))), 1)

    def test_bad_input_is_not_published_as_a_cached_image(self):
        self.original.write_text("not an image")
        result = self.run_command([str(PROJECT / "prepare_wallpaper.sh"), str(self.original)], check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list((self.root / "cache/bgpicker/blur").glob("*.png")), [])
        missing = self.run_command([str(PROJECT / "prepare_wallpaper.sh")], check=False)
        self.assertNotEqual(missing.returncode, 0)

    def test_switch_wallpaper_forwards_to_bgpickerctl(self):
        self.mock_ctl_and_qs()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "mode").write_text("parallax\n")

        self.run_command([str(PROJECT / "switch_wallpaper.sh"), str(self.original)])
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "awww", str(self.original)])
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "toggle", str(self.original)])
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "toggle"])

        resolved = str(self.original.resolve())
        self.assertEqual(
            self.read_calls(),
            [
                ["bgpickerctl", ["apply-static", resolved]],
                ["bgpickerctl", ["apply-static", resolved, "--mode", "awww"]],
                ["bgpickerctl", ["apply-static", resolved, "--mode", "awww"]],
                ["bgpickerctl", ["mode", "toggle"]],
            ],
        )

    def test_switch_wallpaper_restore_and_engine_guards(self):
        self.mock_ctl_and_qs()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)

        # No current_wallpaper -> restore is a no-op
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--restore"])
        self.assertEqual(self.read_calls(), [])

        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--restore"])
        self.assertEqual(self.read_calls(), [["bgpickerctl", ["restore"]]])

        # Video engine refuses static switch
        (state / "engine").write_text("video\n")
        res = self.run_command([str(PROJECT / "switch_wallpaper.sh"), str(self.original)], check=False)
        self.assertNotEqual(res.returncode, 0)

        # Simple engine rejects --mode and routes image to pure dual-swaybg simple_wallpaper.sh
        self.mock_swaybg()
        (state / "engine").write_text("simple\n")
        res = self.run_command([str(PROJECT / "switch_wallpaper.sh"), "--mode", "parallax"], check=False)
        self.assertNotEqual(res.returncode, 0)
        calls_before = len(self.read_calls())
        self.run_command([str(PROJECT / "switch_wallpaper.sh"), str(self.original)])
        resolved = str(self.original.resolve())
        self.assertEqual((state / "current_wallpaper").read_text(), resolved + "\n")
        new_calls = self.read_calls()[calls_before:]
        self.assertFalse(any(c[0] == "bgpickerctl" for c in new_calls))
        self.assertTrue(any(c[0] == "bgpick-fg" and "-i" in c[1] and resolved in c[1] for c in new_calls))
        self.run_command([str(PROJECT / "simple_wallpaper.sh"), "--stop"])

    def test_switch_wallpaper_failure_preserves_previous_state(self):
        self.mock_ctl_and_qs()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "current_wallpaper").write_text("previous-original\n")
        (state / "mode").write_text("awww\n")
        self.env["MOCK_CTL_FAIL"] = "1"
        res = self.run_command(
            [str(PROJECT / "switch_wallpaper.sh"), "--mode", "parallax", str(self.original)],
            check=False,
        )
        self.assertNotEqual(res.returncode, 0)
        self.assertEqual((state / "current_wallpaper").read_text(), "previous-original\n")
        self.assertEqual((state / "mode").read_text(), "awww\n")

    def test_picker_action_routes_static_video_and_mode_switches(self):
        self.mock_ctl_and_qs()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)
        (state / "current_wallpaper").write_text(str(self.original) + "\n")
        (state / "mode").write_text("awww\n")

        self.run_command([str(PROJECT / "picker_action.sh"), "video", "parallax", "/tmp/demo.mp4"])
        self.run_command([str(PROJECT / "picker_action.sh"), "static", str(self.original)])
        self.run_command([str(PROJECT / "picker_action.sh"), "mode", "toggle"])

        (state / "engine").write_text("video\n")
        self.run_command([str(PROJECT / "picker_action.sh"), "mode", "toggle"])

        self.assertEqual(
            self.read_calls(),
            [
                ["bgpickerctl", ["apply-video", "parallax", "/tmp/demo.mp4"]],
                ["bgpickerctl", ["apply-static", str(self.original)]],
                ["bgpickerctl", ["mode", "toggle"]],
                ["bgpickerctl", ["apply-static", str(self.original), "--mode", "parallax"]],
            ],
        )

    def test_wallpaper_engine_and_qs_picker_lifecycle(self):
        self.mock_ctl_and_qs()
        self.env["MOCK_ENGINE_STATUS"] = "full"
        self.run_command([str(PROJECT / "wallpaper_engine.sh"), "startup"])
        self.assertTrue((self.root / "qs.alive").exists())

        self.env["MOCK_ENGINE_STATUS"] = "simple"
        self.run_command([str(PROJECT / "wallpaper_engine.sh"), "simple"])
        self.assertFalse((self.root / "qs.alive").exists())

        self.run_command([str(PROJECT / "wallpaper_engine.sh"), "full", str(self.original), "parallax"])
        self.assertTrue((self.root / "qs.alive").exists())

        ctl_calls = [c[1] for c in self.read_calls() if c[0] == "bgpickerctl"]
        self.assertIn(["startup"], ctl_calls)
        self.assertIn(["engine", "simple"], ctl_calls)
        self.assertIn(["apply-static", str(self.original), "--mode", "parallax"], ctl_calls)

    def test_wallpaper_engine_status_and_toggle_transitions(self):
        self.mock_ctl_and_qs()
        state = self.root / "state/bgpicker"
        state.mkdir(parents=True)

        # Persisted simple status is returned without invoking bgpickerctl / waking daemon
        (state / "engine").write_text("simple\n")
        fail_env = dict(self.env, MOCK_CTL_FAIL="1")
        status_res = self.run_command([str(PROJECT / "wallpaper_engine.sh"), "status"], env=fail_env)
        self.assertEqual(status_res.stdout.strip(), "simple")
        self.assertEqual(self.read_calls(), [])

        # startup when engine=simple delegates to bgpickerctl startup and keeps qs stopped
        (self.root / "qs.alive").write_text("1")
        self.env["MOCK_ENGINE_STATUS"] = "simple"
        self.run_command([str(PROJECT / "wallpaper_engine.sh"), "startup"])
        self.assertFalse((self.root / "qs.alive").exists())

        # Non-simple status delegates to bgpickerctl engine status
        (state / "engine").write_text("full\n")
        self.env["MOCK_ENGINE_STATUS"] = "full"
        self.assertEqual(
            self.run_command([str(PROJECT / "wallpaper_engine.sh"), "status"]).stdout.strip(),
            "full",
        )

        # toggle full -> simple stops qs; toggle simple -> full starts qs
        self.env["MOCK_ENGINE_STATUS"] = "simple"
        (self.root / "qs.alive").write_text("1")
        self.run_command([str(PROJECT / "wallpaper_engine.sh"), "toggle"])
        self.assertFalse((self.root / "qs.alive").exists())

        self.env["MOCK_ENGINE_STATUS"] = "full"
        self.run_command([str(PROJECT / "wallpaper_engine.sh"), "toggle"])
        self.assertTrue((self.root / "qs.alive").exists())

    def test_simple_wallpaper_dual_swaybg_lifecycle_and_stop(self):
        self.mock_swaybg()
        state = self.root / "state/bgpicker"
        cache = self.root / "cache/bgpicker"

        self.run_command([str(PROJECT / "simple_wallpaper.sh"), str(self.original)])

        patched = cache / "swaybg-front"
        self.assertTrue(patched.is_file())
        patched_bytes = patched.read_bytes()
        self.assertEqual(patched_bytes.count(b"bgpick-fg\0"), 1)
        self.assertEqual(patched_bytes.count(b"wallpaper\0"), 0)

        resolved_first = str(self.original.resolve())
        blur_first = Path((self.root / "home/.cache/current_swaybg_img").read_text().strip())
        self.assertTrue(blur_first.is_file())
        self.assertEqual((state / "current_wallpaper").read_text(), resolved_first + "\n")

        front_pid1 = int((state / "simple-front.pid").read_text().split()[0])
        back_pid1 = int((state / "simple-back.pid").read_text().split()[0])
        self.assertTrue(self.pid_alive(front_pid1))
        self.assertTrue(self.pid_alive(back_pid1))

        calls = self.read_calls()
        self.assertIn(["wallpaper", ["-i", str(blur_first), "-m", "fill"]], calls)
        self.assertIn(["bgpick-fg", ["-i", resolved_first, "-m", "fill"]], calls)

        # Switching to a second wallpaper retires the old pair and updates state atomically
        second = self.root / "second.png"
        self.run_command(["magick", "-size", "128x96", "xc:blue", str(second)])
        self.run_command([str(PROJECT / "simple_wallpaper.sh"), str(second)])

        self.assertTrue(self.wait_pid_dead(front_pid1))
        self.assertTrue(self.wait_pid_dead(back_pid1))
        front_pid2 = int((state / "simple-front.pid").read_text().split()[0])
        back_pid2 = int((state / "simple-back.pid").read_text().split()[0])
        self.assertTrue(self.pid_alive(front_pid2))
        self.assertTrue(self.pid_alive(back_pid2))
        self.assertEqual((state / "current_wallpaper").read_text(), str(second.resolve()) + "\n")

        # --stop stops both managed swaybg processes cleanly and is idempotent
        self.run_command([str(PROJECT / "simple_wallpaper.sh"), "--stop"])
        self.assertTrue(self.wait_pid_dead(front_pid2))
        self.assertTrue(self.wait_pid_dead(back_pid2))
        self.assertFalse((state / "simple-front.pid").exists())
        self.assertFalse((state / "simple-back.pid").exists())
        self.run_command([str(PROJECT / "simple_wallpaper.sh"), "--stop"])

    def test_simple_wallpaper_failure_preserves_previous_processes_and_state(self):
        self.mock_swaybg()
        state = self.root / "state/bgpicker"
        self.run_command([str(PROJECT / "simple_wallpaper.sh"), str(self.original)])

        front_pid1 = int((state / "simple-front.pid").read_text().split()[0])
        back_pid1 = int((state / "simple-back.pid").read_text().split()[0])
        prev_wp = (state / "current_wallpaper").read_text()
        prev_blur = (self.root / "home/.cache/current_swaybg_img").read_text()

        second = self.root / "second.png"
        self.run_command(["magick", "-size", "128x96", "xc:green", str(second)])
        fail_env = dict(self.env, MOCK_SWAYBG_FAIL_FRONT="1")
        res = self.run_command([str(PROJECT / "simple_wallpaper.sh"), str(second)], env=fail_env, check=False)
        self.assertNotEqual(res.returncode, 0)

        self.assertTrue(self.pid_alive(front_pid1))
        self.assertTrue(self.pid_alive(back_pid1))
        self.assertEqual((state / "current_wallpaper").read_text(), prev_wp)
        self.assertEqual((self.root / "home/.cache/current_swaybg_img").read_text(), prev_blur)
        self.run_command([str(PROJECT / "simple_wallpaper.sh"), "--stop"])

    def test_bgpicker_sh_and_backdrop_shims_and_env_override(self):
        self.mock_ctl_and_qs()
        custom_ctl = self.root / "custom-ctl"
        custom_log = self.root / "custom.log"
        custom_ctl.write_text(
            f"#!/bin/bash\nprintf '%s\\n' \"$*\" >> {json.dumps(str(custom_log))}\n"
        )
        custom_ctl.chmod(0o755)
        env = dict(self.env, BGPICKERCTL=str(custom_ctl))
        self.run_command([str(PROJECT / "backdrop_parallax.sh"), "on"], env=env)
        self.run_command([str(PROJECT / "backdrop_awww.sh")], env=env)
        self.assertEqual(custom_log.read_text().splitlines(), ["backdrop-parallax on", "start"])

        self.run_command([str(PROJECT / "bgpicker.sh"), "toggle"])
        self.run_command([str(PROJECT / "bgpicker.sh"), "next"])
        self.run_command([str(PROJECT / "bgpicker.sh"), "transition", "wave"])
        self.run_command([str(PROJECT / "bgpicker.sh"), "video", "next"])
        self.run_command([str(PROJECT / "bgpicker.sh"), "stop"])
        self.assertFalse((self.root / "qs.alive").exists())

        calls = self.read_calls()
        self.assertIn(["qs", ["ipc", "-p", str(PROJECT), "call", "wallpaperCarousel", "toggle"]], calls)
        self.assertIn(["qs", ["ipc", "-p", str(PROJECT), "call", "wallpaperCarousel", "cycleNext"]], calls)
        self.assertIn(["bgpickerctl", ["transition", "wave"]], calls)
        self.assertIn(["bgpickerctl", ["video", "next"]], calls)
        self.assertIn(["bgpickerctl", ["stop"]], calls)


if __name__ == "__main__":
    unittest.main()
