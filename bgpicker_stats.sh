#!/bin/bash
# One-shot or live-watch human-readable resource usage monitor for BgPicker processes.
set -euo pipefail

PROJECT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

exec python3 - "$PROJECT_DIR" "$@" <<'PY'
import argparse
import os
import re
import signal
import subprocess
import sys
import time
import unicodedata
from pathlib import Path

PROJECT_DIR = Path(sys.argv[1]).resolve()
ARGV = sys.argv[2:]
CLK_TCK = os.sysconf("SC_CLK_TCK")


def disp_width(s: str) -> int:
    return sum(2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1 for ch in s)


def ljust_disp(s: str, width: int) -> str:
    return s + " " * max(0, width - disp_width(s))


def rjust_disp(s: str, width: int) -> str:
    return " " * max(0, width - disp_width(s)) + s


def format_kib(kib: int) -> str:
    val = float(kib)
    for unit in ("KiB", "MiB", "GiB", "TiB"):
        if val < 1024.0 or unit == "TiB":
            if unit == "KiB":
                return f"{int(round(val))} {unit}"
            return f"{val:.1f} {unit}"
        val /= 1024.0
    return f"{kib} KiB"


def format_mib(mib: int | None) -> str:
    if mib is None:
        return "-"
    if mib >= 1024:
        return f"{mib / 1024.0:.2f} GiB"
    return f"{mib} MiB"


def find_bgpicker_pids() -> list[tuple[int, str]]:
    """Locate live bgpicker-daemon, project Quickshell, and simple-mode swaybg PIDs."""
    found: dict[int, str] = {}
    proj_str = str(PROJECT_DIR)

    # Check state PID files for simple-mode swaybg if active.
    state_dir = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state")) / "bgpicker"
    simple_pids: dict[int, str] = {}
    for label, name in (("swaybg-front", "simple-front.pid"), ("swaybg-back", "simple-back.pid")):
        pid_file = state_dir / name
        if pid_file.is_file():
            try:
                parts = pid_file.read_text(encoding="utf-8").split()
                if len(parts) >= 2:
                    simple_pids[int(parts[0])] = label
            except (OSError, ValueError):
                pass

    proc_root = Path("/proc")
    for entry in proc_root.iterdir():
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        try:
            raw_cmd = (entry / "cmdline").read_bytes()
        except OSError:
            continue
        if not raw_cmd:
            continue
        args = [a.decode("utf-8", errors="replace") for a in raw_cmd.split(b"\0") if a]
        if not args:
            continue
        exe_name = os.path.basename(args[0])

        if pid in simple_pids:
            found[pid] = simple_pids[pid]
            continue

        if exe_name == "bgpicker-daemon":
            found[pid] = "bgpicker-daemon"
        elif exe_name == "qs":
            # Match only the Quickshell instance rooted at this bgpicker project directory.
            for idx, arg in enumerate(args):
                if arg == "-p" and idx + 1 < len(args):
                    try:
                        if Path(args[idx + 1]).resolve() == PROJECT_DIR:
                            found[pid] = "qs (picker)"
                    except OSError:
                        if args[idx + 1] == proj_str:
                            found[pid] = "qs (picker)"
                    break

    order = {"bgpicker-daemon": 0, "qs (picker)": 1, "swaybg-front": 2, "swaybg-back": 3}
    return sorted(found.items(), key=lambda item: (order.get(item[1], 99), item[0]))


def read_proc_cpu_ticks(pid: int) -> tuple[int, int] | None:
    """Return (utime + stime ticks, thread_count) for pid."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8")
        rparen = stat.rfind(")")
        if rparen < 0:
            return None
        fields = stat[rparen + 2 :].split()
        utime = int(fields[11])
        stime = int(fields[12])
        threads = int(fields[17])
        return (utime + stime, threads)
    except (OSError, ValueError, IndexError):
        return None


def read_smaps_kib(pid: int) -> dict[str, int] | None:
    try:
        text = Path(f"/proc/{pid}/smaps_rollup").read_text(encoding="utf-8")
    except OSError:
        return None
    metrics: dict[str, int] = {}
    for line in text.splitlines():
        m = re.match(r"^(Rss|Pss|Private_Dirty|Swap):\s+(\d+)\s+kB", line)
        if m:
            metrics[m.group(1)] = int(m.group(2))
    return metrics


def read_nvidia_gpu_stats(target_pids: set[int]) -> dict[int, dict[str, str | int | None]]:
    """Query per-PID NVIDIA VRAM and utilization via nvidia-smi pmon."""
    if not target_pids:
        return {}
    try:
        res = subprocess.run(
            ["nvidia-smi", "pmon", "-c", "1", "-s", "um"],
            capture_output=True,
            text=True,
            timeout=3,
        )
        if res.returncode != 0:
            return {}
    except (OSError, subprocess.TimeoutExpired):
        return {}

    out: dict[int, dict[str, str | int | None]] = {}
    for line in res.stdout.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        cols = stripped.split()
        # Expected columns for -s um: gpu pid type sm mem enc dec jpg ofa fb ccpm command
        if len(cols) < 12:
            continue
        try:
            pid = int(cols[1])
        except ValueError:
            continue
        if pid not in target_pids:
            continue
        fb_raw = cols[9]
        fb_mib = int(fb_raw) if fb_raw.isdigit() else None
        out[pid] = {
            "fb_mib": fb_mib,
            "sm": cols[3],
            "mem": cols[4],
            "dec": cols[6],
        }
    return out


def read_daemon_summary() -> str | None:
    candidates = [
        os.environ.get("BGPICKERCTL", ""),
        str(PROJECT_DIR / "target/release/bgpickerctl"),
        str(PROJECT_DIR / "target/debug/bgpickerctl"),
    ]
    ctl = next((c for c in candidates if c and os.access(c, os.X_OK)), None)
    if not ctl:
        return None
    try:
        res = subprocess.run([ctl, "status", "--no-start"], capture_output=True, text=True, timeout=2)
        if res.returncode != 0:
            return None
        kv = {}
        for line in res.stdout.splitlines():
            if ":" in line:
                k, v = line.split(":", 1)
                kv[k.strip()] = v.strip()
        engine = kv.get("engine", "?")
        mode = kv.get("video_mode" if engine == "video" else "mode", "?")
        media = kv.get("video" if engine == "video" else "wallpaper", "-")
        if media not in ("-", ""):
            media = os.path.basename(media)
        bd = kv.get("backdrop_parallax", "?")
        return f"engine={engine}  mode={mode}  backdrop_parallax={bd}  media={media}"
    except (OSError, subprocess.TimeoutExpired):
        return None


def collect_and_print(sample_sec: float) -> None:
    procs = find_bgpicker_pids()
    now_str = time.strftime("%H:%M:%S")
    summary = read_daemon_summary()

    print(f"BgPicker 资源占用快照 ({now_str}, CPU采样 {sample_sec:.2f}s)")
    if summary:
        print(f"状态: {summary}")

    if not procs:
        print("未检测到正在运行的 bgpicker-daemon、项目 qs 或 simple swaybg 进程。")
        return

    t0 = time.monotonic()
    cpu_before = {pid: read_proc_cpu_ticks(pid) for pid, _ in procs}
    time.sleep(sample_sec)
    dt = max(0.001, time.monotonic() - t0)
    cpu_after = {pid: read_proc_cpu_ticks(pid) for pid, _ in procs}

    gpu_map = read_nvidia_gpu_stats({pid for pid, _ in procs})

    header = " ".join([
        ljust_disp("进程", 18),
        rjust_disp("PID", 7),
        rjust_disp("CPU%", 6),
        rjust_disp("线程", 4),
        rjust_disp("PSS(真实)", 10),
        rjust_disp("Private", 10),
        rjust_disp("RSS", 10),
        rjust_disp("VRAM", 9),
        "SM/DEC",
    ])
    sep = "-" * disp_width(header)
    print(sep)
    print(header)
    print(sep)

    tot_cpu = 0.0
    tot_threads = 0
    tot_pss = 0
    tot_priv = 0
    tot_rss = 0
    tot_vram = 0
    has_vram = False

    for pid, label in procs:
        b = cpu_before.get(pid)
        a = cpu_after.get(pid)
        smaps = read_smaps_kib(pid)
        if b is None or a is None or smaps is None:
            continue

        cpu_pct = ((a[0] - b[0]) / CLK_TCK) / dt * 100.0
        threads = a[1]
        pss = smaps.get("Pss", 0)
        priv = smaps.get("Private_Dirty", 0)
        rss = smaps.get("Rss", 0)

        g = gpu_map.get(pid, {})
        fb_mib = g.get("fb_mib")
        sm = g.get("sm", "-")
        dec = g.get("dec", "-")
        sm_str = f"{sm}%" if sm != "-" else "-"
        dec_str = f"{dec}%" if dec != "-" else "-"
        gpu_act = "-" if (sm == "-" and dec == "-") else f"{sm_str}/{dec_str}"

        tot_cpu += cpu_pct
        tot_threads += threads
        tot_pss += pss
        tot_priv += priv
        tot_rss += rss
        if isinstance(fb_mib, int):
            tot_vram += fb_mib
            has_vram = True

        print(" ".join([
            ljust_disp(label, 18),
            rjust_disp(str(pid), 7),
            rjust_disp(f"{cpu_pct:.1f}%", 6),
            rjust_disp(str(threads), 4),
            rjust_disp(format_kib(pss), 10),
            rjust_disp(format_kib(priv), 10),
            rjust_disp(format_kib(rss), 10),
            rjust_disp(format_mib(fb_mib if isinstance(fb_mib, int) else None), 9),
            gpu_act,
        ]))

    print(sep)
    print(" ".join([
        ljust_disp("合计", 18),
        rjust_disp("-", 7),
        rjust_disp(f"{tot_cpu:.1f}%", 6),
        rjust_disp(str(tot_threads), 4),
        rjust_disp(format_kib(tot_pss), 10),
        rjust_disp(format_kib(tot_priv), 10),
        rjust_disp(format_kib(tot_rss), 10),
        rjust_disp(format_mib(tot_vram if has_vram else None), 9),
    ]))


def main() -> int:
    parser = argparse.ArgumentParser(
        prog="bgpicker_stats.sh",
        description="Human-readable CPU, PSS/RSS memory, and GPU VRAM usage for BgPicker processes.",
    )
    parser.add_argument(
        "-i",
        "--interval",
        type=float,
        default=0.4,
        help="CPU sampling window in seconds (default: 0.4)",
    )
    parser.add_argument(
        "-w",
        "--watch",
        nargs="?",
        const=1.0,
        type=float,
        default=None,
        metavar="SEC",
        help="Refresh continuously every SEC seconds (default: 1.0 when -w is passed)",
    )
    args = parser.parse_args(ARGV)
    sample = max(0.05, args.interval)

    signal.signal(signal.SIGINT, lambda *_: sys.exit(0))

    if args.watch is None:
        collect_and_print(sample)
        return 0

    period = max(0.2, args.watch)
    while True:
        # Clear screen and move cursor home in watch mode.
        sys.stdout.write("\033[2J\033[H")
        sys.stdout.flush()
        collect_and_print(sample)
        sys.stdout.flush()
        time.sleep(max(0.05, period - sample))


if __name__ == "__main__":
    raise SystemExit(main())
PY
