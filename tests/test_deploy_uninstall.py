import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent


class DeployUninstallTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="bgpicker-deploy-test-"))
        self.repo = self.tmp / "repo"
        self.home = self.tmp / "home"
        self.xdg_cfg = self.home / ".config"
        self.xdg_state = self.home / ".local/state"
        self.xdg_cache = self.home / ".cache"
        self.xdg_run = self.tmp / "run"
        self.bin_dir = self.home / ".local/bin"
        self.niri_cfg = self.xdg_cfg / "niri/config.kdl"

        for d in [
            self.repo / "target/release",
            self.xdg_cfg / "niri",
            self.xdg_state,
            self.xdg_cache,
            self.xdg_run,
            self.bin_dir,
        ]:
            d.mkdir(parents=True, exist_ok=True)

        shutil.copy2(REPO_ROOT / "deploy.sh", self.repo / "deploy.sh")
        shutil.copy2(REPO_ROOT / "uninstall.sh", self.repo / "uninstall.sh")
        shutil.copy2(REPO_ROOT / "settings.example.json", self.repo / "settings.example.json")
        (self.repo / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")

        for b in ("bgpickerctl", "bgpicker-daemon", "bgpicker-picker", "bgpicker-splash"):
            p = self.repo / "target/release" / b
            p.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            p.chmod(p.stat().st_mode | stat.S_IXUSR)

        self.niri_cfg.write_text(
            '// old header\n'
            'spawn-at-startup "/old/path/wallpaper_engine.sh" "startup"\n'
            'spawn-at-startup "waybar"\n'
            'binds {\n'
            '    Mod+Return { spawn "kitty"; }\n'
            '}\n'
            'layer-rule {\n'
            '    match namespace="^waybar$"\n'
            '}\n',
            encoding="utf-8",
        )

        self.env = os.environ.copy()
        self.env.update(
            {
                "HOME": str(self.home),
                "XDG_CONFIG_HOME": str(self.xdg_cfg),
                "XDG_STATE_HOME": str(self.xdg_state),
                "XDG_CACHE_HOME": str(self.xdg_cache),
                "XDG_RUNTIME_DIR": str(self.xdg_run),
                "XDG_BIN_HOME": str(self.bin_dir),
            }
        )
        self.env.pop("WAYLAND_DISPLAY", None)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_deploy_check_only_and_dry_run_do_not_mutate(self):
        res_check = subprocess.run(
            ["bash", str(self.repo / "deploy.sh"), "--skip-build", "--check-only"],
            env=self.env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(res_check.returncode, 0, res_check.stderr)

        before = self.niri_cfg.read_text(encoding="utf-8")
        res_dry = subprocess.run(
            [
                "bash",
                str(self.repo / "deploy.sh"),
                "--skip-build",
                "--no-start",
                "--dry-run",
                "--niri-config",
                str(self.niri_cfg),
            ],
            env=self.env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(res_dry.returncode, 0, res_dry.stderr)
        self.assertEqual(self.niri_cfg.read_text(encoding="utf-8"), before)
        self.assertFalse((self.bin_dir / "bgpickerctl").exists())

    def test_deploy_and_uninstall_lifecycle_and_media_protection(self):
        # 1. Deploy into isolated repo & home (idempotent across two runs).
        for _ in range(2):
            res = subprocess.run(
                [
                    "bash",
                    str(self.repo / "deploy.sh"),
                    "--skip-build",
                    "--no-start",
                    "--niri-config",
                    str(self.niri_cfg),
                    "--bin-dir",
                    str(self.bin_dir),
                ],
                env=self.env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(res.returncode, 0, res.stderr + res.stdout)

        self.assertTrue((self.repo / "wallpapers").is_dir())
        self.assertTrue((self.repo / "dynamic").is_dir())
        self.assertTrue((self.xdg_cfg / "bgpicker/settings.json").is_file())
        for b in ("bgpickerctl", "bgpicker-daemon", "bgpicker-picker", "bgpicker-splash"):
            link = self.bin_dir / b
            self.assertTrue(link.is_symlink(), f"expected symlink at {link}")
            self.assertEqual(link.resolve(), (self.repo / "target/release" / b).resolve())

        kdl = self.niri_cfg.read_text(encoding="utf-8")
        self.assertEqual(kdl.count("bgpicker-splash"), 1)
        self.assertEqual(kdl.count("bgpickerctl"), 1)
        self.assertNotIn("wallpaper_engine.sh", kdl)
        self.assertIn('spawn-at-startup "waybar"', kdl)
        self.assertIn('namespace="^bgpicker:backdrop$"', kdl)
        self.assertIn("Mod+N", kdl)

        # Put personal media + state/cache/runtime sockets to test cleanup boundaries.
        wp = self.repo / "wallpapers/personal.jpg"
        vid = self.repo / "dynamic/personal.mp4"
        wp.write_bytes(b"jpg")
        vid.write_bytes(b"mp4")

        (self.xdg_state / "bgpicker").mkdir(parents=True, exist_ok=True)
        (self.xdg_state / "bgpicker/engine").write_text("full\n", encoding="utf-8")
        (self.xdg_cache / "bgpicker").mkdir(parents=True, exist_ok=True)
        (self.xdg_cache / "bgpicker/startup-splash-fg.bin").write_bytes(b"BGPX")
        (self.home / ".cache/current_swaybg_img").write_text(
            str(self.xdg_cache / "bgpicker/blur/a.png"), encoding="utf-8"
        )
        (self.xdg_run / "bgpicker-wayland-1.sock").write_text("", encoding="utf-8")
        (self.xdg_run / "bgpicker-picker-wayland-1.sock").write_text("", encoding="utf-8")

        # 2. Uninstall dry-run must leave everything intact.
        res_u_dry = subprocess.run(
            [
                "bash",
                str(self.repo / "uninstall.sh"),
                "--dry-run",
                "--niri-config",
                str(self.niri_cfg),
                "--bin-dir",
                str(self.bin_dir),
            ],
            env=self.env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(res_u_dry.returncode, 0, res_u_dry.stderr)
        self.assertTrue((self.xdg_cache / "bgpicker/startup-splash-fg.bin").exists())

        # 3. Default thorough uninstall preserves personal media & source repo.
        res_u = subprocess.run(
            [
                "bash",
                str(self.repo / "uninstall.sh"),
                "-y",
                "--niri-config",
                str(self.niri_cfg),
                "--bin-dir",
                str(self.bin_dir),
            ],
            env=self.env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(res_u.returncode, 0, res_u.stderr + res_u.stdout)

        for b in ("bgpickerctl", "bgpicker-daemon", "bgpicker-picker", "bgpicker-splash"):
            self.assertFalse((self.bin_dir / b).exists())
        self.assertFalse((self.xdg_cfg / "bgpicker").exists())
        self.assertFalse((self.xdg_state / "bgpicker").exists())
        self.assertFalse((self.xdg_cache / "bgpicker").exists())
        self.assertFalse((self.home / ".cache/current_swaybg_img").exists())
        self.assertFalse((self.xdg_run / "bgpicker-wayland-1.sock").exists())
        self.assertFalse((self.xdg_run / "bgpicker-picker-wayland-1.sock").exists())
        self.assertFalse((self.repo / "target").exists())

        cleaned_kdl = self.niri_cfg.read_text(encoding="utf-8")
        self.assertNotIn("bgpicker", cleaned_kdl)
        self.assertNotIn("place-within-backdrop", cleaned_kdl)
        self.assertIn('spawn-at-startup "waybar"', cleaned_kdl)
        self.assertIn('match namespace="^waybar$"', cleaned_kdl)

        self.assertTrue(wp.is_file(), "personal wallpapers must be preserved by default")
        self.assertTrue(vid.is_file(), "personal videos must be preserved by default")

        # 4. --purge-media removes personal media while keeping .gitkeep and repo.
        (self.repo / "wallpapers/.gitkeep").write_text("", encoding="utf-8")
        res_purge = subprocess.run(
            [
                "bash",
                str(self.repo / "uninstall.sh"),
                "-y",
                "--skip-niri-config",
                "--purge-media",
            ],
            env=self.env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(res_purge.returncode, 0, res_purge.stderr)
        self.assertFalse(wp.exists())
        self.assertFalse(vid.exists())
        self.assertTrue((self.repo / "wallpapers/.gitkeep").is_file())


if __name__ == "__main__":
    unittest.main()
