#!/usr/bin/env python3
"""Atomic shared transition preferences and one resolved snapshot per switch."""
import argparse
from contextlib import contextmanager
import fcntl
import json
import math
import os
from pathlib import Path
import random
import sys
import tempfile

TYPES = ("fade", "wipe", "wave", "grow", "outer", "random", "none")


def settings_path():
    return Path(os.environ.get("XDG_CONFIG_HOME") or str(Path.home() / ".config")) / "bgpicker/settings.json"


def read_config():
    try:
        value = json.loads(settings_path().read_text())
    except FileNotFoundError:
        return {}
    if not isinstance(value, dict):
        raise ValueError("settings.json must contain a JSON object")
    return value


def number(value, fallback, low, high):
    try:
        if value is None:
            return fallback
        n = float(value)
        return min(high, max(low, n)) if math.isfinite(n) else fallback
    except (TypeError, ValueError):
        return fallback


def normalize(config):
    raw = config.get("transition")
    p = raw if isinstance(raw, dict) else {}
    legacy = config.get("parallax")
    legacy = legacy.get("transitionMs") if isinstance(legacy, dict) else None

    def pair(value, fallback, low, high):
        return [number(v, f, low, high) for v, f in zip(value, fallback)] if isinstance(value, list) and len(value) == 2 else fallback

    return {
        "type": p.get("type") if p.get("type") in TYPES else "fade",
        "durationMs": int(number(p.get("durationMs"), number(legacy, 800, 0, 5000), 0, 5000) + 0.5),
        "fps": int(number(p.get("fps"), 60, 1, 240) + 0.5),
        "angle": number(p.get("angle"), 45, 0, 360),
        "position": pair(p.get("position"), [0.5, 0.5], 0, 1),
        "wave": pair(p.get("wave"), [20, 20], 1, 500),
    }


@contextmanager
def preference_lock():
    cache = Path(os.environ.get("XDG_CACHE_HOME") or str(Path.home() / ".cache")) / "bgpicker"
    cache.mkdir(parents=True, exist_ok=True)
    with (cache / "switch.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def set_transition(kind):
    with preference_lock():
        config = read_config()
        previous = config.get("transition")
        transition = dict(previous) if isinstance(previous, dict) else {}
        transition["type"] = kind
        config["transition"] = transition
        path = settings_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        # Replace only after a complete write; preserve unrelated customization.
        fd, temporary = tempfile.mkstemp(prefix=".settings-", suffix=".json", dir=path.parent)
        try:
            with os.fdopen(fd, "w") as stream:
                json.dump(config, stream, ensure_ascii=False, indent=2)
                stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, path)
        finally:
            Path(temporary).unlink(missing_ok=True)


def resolve(config, instant=False):
    value = normalize(config)
    value["selectedType"] = value["type"]
    if value["type"] == "random":
        value["type"] = random.choice(TYPES[:5])
    if instant or value["durationMs"] == 0:
        value["type"] = "none"
        value["durationMs"] = 0
    return value


def awww_args(value):
    x, y = value["position"]
    wave_width, wave_height = value["wave"]
    return ["--transition-type", value["type"], "--transition-duration", str(value["durationMs"] / 1000),
            "--transition-fps", str(value["fps"]), "--transition-angle", str(value["angle"]),
            "--transition-pos", f"{x:.6f},{y:.6f}", "--invert-y",
            "--transition-wave", f"{wave_width:g},{wave_height:g}",
            "--transition-bezier", ".54,0,.34,.99"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("show")
    setter = sub.add_parser("set-transition")
    setter.add_argument("type", choices=TYPES)
    resolver = sub.add_parser("resolve")
    resolver.add_argument("--shell", action="store_true", help="Print the JSON snapshot then one argv element per line")
    resolver.add_argument("--instant", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "set-transition":
            set_transition(args.type)
        elif args.command == "show":
            print(json.dumps(normalize(read_config()), separators=(",", ":")))
        else:
            value = resolve(read_config(), args.instant)
            print(json.dumps(value, separators=(",", ":")))
            if args.shell:
                print("\n".join(awww_args(value)))
    except (OSError, ValueError) as error:
        print(f"BgPicker settings: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
