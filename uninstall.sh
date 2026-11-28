#!/bin/bash
# ==============================================================================
# Script Name: uninstall.sh
# Description: Thorough cleanup / uninstall script for BgPicker.
#              Stops all running processes cleanly, removes niri config entries
#              (with backup + `niri validate`), removes ~/.local/bin symlinks,
#              cleans XDG config/state/cache/runtime files and build artifacts.
#              Personal media (`wallpapers/`, `dynamic/`) and git source dir are
#              preserved unless explicitly requested with --purge-media/--purge-repo.
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

NIRI_CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/niri/config.kdl"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
KEEP_TARGET=false
PURGE_MEDIA=false
PURGE_REPO=false
SKIP_NIRI_CONFIG=false
DRY_RUN=false
YES=false

BGPICKER_BINS=(bgpickerctl bgpicker-daemon bgpicker-picker bgpicker-splash)

usage() {
    cat <<'EOF'
Usage: ./uninstall.sh [OPTIONS]

Options:
  -y, --yes            Skip interactive confirmation prompt
  --niri-config PATH   Override niri config path (default: ~/.config/niri/config.kdl)
  --bin-dir DIR        Directory of CLI symlinks (default: ~/.local/bin)
  --skip-niri-config   Do not modify niri/config.kdl
  --keep-target        Keep Cargo build directory (`target/`)
  --purge-media        Also delete personal files in `wallpapers/` and `dynamic/`
  --purge-repo         Also remove the entire repository directory itself (implies --purge-media)
  --dry-run            Show what would be removed without deleting anything
  -h, --help           Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -y|--yes)
            YES=true
            shift
            ;;
        --niri-config)
            [[ $# -ge 2 ]] || { echo "uninstall.sh: --niri-config requires a path" >&2; exit 2; }
            NIRI_CONFIG="$2"
            shift 2
            ;;
        --niri-config=*)
            NIRI_CONFIG="${1#--niri-config=}"
            shift
            ;;
        --bin-dir)
            [[ $# -ge 2 ]] || { echo "uninstall.sh: --bin-dir requires a path" >&2; exit 2; }
            BIN_DIR="$2"
            shift 2
            ;;
        --bin-dir=*)
            BIN_DIR="${1#--bin-dir=}"
            shift
            ;;
        --skip-niri-config)
            SKIP_NIRI_CONFIG=true
            shift
            ;;
        --keep-target)
            KEEP_TARGET=true
            shift
            ;;
        --purge-media)
            PURGE_MEDIA=true
            shift
            ;;
        --purge-repo)
            PURGE_REPO=true
            PURGE_MEDIA=true
            shift
            ;;
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "uninstall.sh: unknown option '$1'" >&2
            usage >&2
            exit 2
            ;;
    esac
done

log() {
    printf '[uninstall] %s\n' "$*"
}

warn() {
    printf '[uninstall] WARN: %s\n' "$*" >&2
}

run_or_dry() {
    if $DRY_RUN; then
        printf '[dry-run]'
        printf ' %q' "$@"
        printf '\n'
    else
        "$@"
    fi
}

CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/bgpicker"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/bgpicker"
RUNTIME_BASE="${XDG_RUNTIME_DIR:-/tmp}"
LEGACY_SWAYBG_CACHE="${HOME}/.cache/current_swaybg_img"

if ! $YES && ! $DRY_RUN; then
    echo "This will stop BgPicker, remove its XDG config/state/cache/runtime files,"
    echo "unlink binaries in $BIN_DIR, and clean BgPicker entries from $NIRI_CONFIG."
    if ! $KEEP_TARGET; then
        echo "It will also remove build artifacts under $SCRIPT_DIR/target."
    fi
    if $PURGE_REPO; then
        echo "WARNING: --purge-repo will delete the entire directory $SCRIPT_DIR!"
    elif $PURGE_MEDIA; then
        echo "WARNING: --purge-media will delete files inside wallpapers/ and dynamic/!"
    fi
    read -r -p "Continue? [y/N] " reply
    case "${reply,,}" in
        y|yes) ;;
        *)
            log "aborted."
            exit 0
            ;;
    esac
fi

# 1. Stop running processes safely.
stop_processes() {
    local ctl="$SCRIPT_DIR/target/release/bgpickerctl"
    if [[ -x "$ctl" && -n "${WAYLAND_DISPLAY:-}" ]]; then
        run_or_dry "$ctl" stop >/dev/null 2>&1 || true
    fi
    if command -v qs >/dev/null 2>&1; then
        run_or_dry qs kill -p "$SCRIPT_DIR" >/dev/null 2>&1 || true
    fi
    if [[ -x "$SCRIPT_DIR/simple_wallpaper.sh" ]]; then
        run_or_dry "$SCRIPT_DIR/simple_wallpaper.sh" --stop >/dev/null 2>&1 || true
    fi
    if command -v awww >/dev/null 2>&1 && [[ -d "$CACHE_DIR/backdrop-awww" ]]; then
        if ! $DRY_RUN; then
            XDG_CACHE_HOME="$CACHE_DIR/backdrop-awww" awww kill --namespace bgpicker-backdrop >/dev/null 2>&1 || true
        else
            log "[dry-run] would kill bgpicker-backdrop awww-daemon if running"
        fi
    fi

    # Terminate any orphaned splash/daemon/picker binaries belonging to this checkout.
    if ! $DRY_RUN; then
        python3 - "$SCRIPT_DIR" <<'PY'
import os, signal, sys, time
from pathlib import Path

repo = os.path.realpath(sys.argv[1]) + os.sep
my_pid = os.getpid()
uid = os.getuid()
names = {"bgpicker-daemon", "bgpicker-picker", "bgpicker-splash"}
victims = []

for entry in os.scandir("/proc"):
    if not entry.name.isdigit():
        continue
    pid = int(entry.name)
    if pid == my_pid:
        continue
    try:
        if os.stat(entry.path).st_uid != uid:
            continue
        comm = Path(entry.path, "comm").read_text().strip()
        if comm not in names:
            continue
        exe = ""
        try:
            exe = os.path.realpath(os.readlink(os.path.join(entry.path, "exe")))
        except OSError:
            pass
        cmdline = Path(entry.path, "cmdline").read_bytes().replace(b"\x00", b" ").decode(errors="ignore")
        if exe.startswith(repo) or repo in cmdline:
            victims.append(pid)
    except OSError:
        continue

for pid in victims:
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass

if victims:
    deadline = time.monotonic() + 0.3
    while time.monotonic() < deadline:
        alive = []
        for pid in victims:
            try:
                os.kill(pid, 0)
                alive.append(pid)
            except ProcessLookupError:
                pass
        if not alive:
            break
        time.sleep(0.02)
    else:
        for pid in victims:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
PY
    else
        log "[dry-run] would stop orphaned bgpicker-daemon/picker/splash processes owned by $SCRIPT_DIR"
    fi

    # Clean up recorded placeholder/simple PID files safely.
    local pid_file pid
    for pid_file in \
        "$STATE_DIR/startup-placeholder-front.pid" \
        "$STATE_DIR/startup-placeholder-back.pid" \
        "$STATE_DIR/simple-front.pid" \
        "$STATE_DIR/simple-back.pid"
    do
        if [[ -f "$pid_file" ]]; then
            pid="$(awk 'NR==1{print $1}' "$pid_file" 2>/dev/null || true)"
            if [[ "$pid" =~ ^[0-9]+$ ]] && kill -0 "$pid" 2>/dev/null; then
                local comm
                comm="$(ps -p "$pid" -o comm= 2>/dev/null || true)"
                if [[ "$comm" == *swaybg* || "$comm" == *bgpicker* ]]; then
                    run_or_dry kill -TERM "$pid" 2>/dev/null || true
                fi
            fi
        fi
    done
}

# 2. Clean niri/config.kdl entries with backup and syntax validation.
clean_niri_config() {
    local cfg="$1"
    local repo="$2"
    [[ -f "$cfg" ]] || return 0

    local stamp backup
    stamp="$(date +%Y%m%d-%H%M%S)"
    backup="${cfg}.pre-bgpicker-uninstall.${stamp}"

    if $DRY_RUN; then
        log "[dry-run] would back up $cfg -> $backup and strip bgpicker entries"
        return 0
    fi

    cp -a "$cfg" "$backup"

    python3 - "$cfg" "$repo" <<'PY'
import re
import sys
from pathlib import Path

cfg_path = Path(sys.argv[1])
repo = sys.argv[2]
text = cfg_path.read_text(encoding="utf-8")
original = text

managed_blocks = [
    (r"// >>> bgpicker managed start >>>", r"// <<< bgpicker managed end <<<"),
    (r"// >>> bgpicker layer-rule start >>>", r"// <<< bgpicker layer-rule end <<<"),
    (r"// >>> bgpicker binds start >>>", r"// <<< bgpicker binds end <<<"),
]
for start_m, end_m in managed_blocks:
    text = re.sub(
        r"(?m)^[ \t]*" + re.escape(start_m) + r"[\s\S]*?^[ \t]*" + re.escape(end_m) + r"[ \t]*\n?",
        "",
        text,
    )

line_patterns = [
    r'^[ \t]*//壁纸开机独立垫图守护进程[^\n]*\n?',
    r'^[ \t]*//壁纸守护进程\s*\n(?=[ \t]*spawn-at-startup\s+"[^"]*(?:bgpicker|wallpaper_engine\.sh))',
    r'^[ \t]*spawn-at-startup[^\n]*(?:' + re.escape(repo) + r'|bgpicker-splash|bgpickerctl|wallpaper_engine\.sh|awww-daemonbgpicker)[^\n]*\n?',
    r'^[ \t]*match\s+namespace="\^bgpicker:backdrop\$"[^\n]*\n?',
    r'^[ \t]*match\s+namespace="\^awww-daemonbgpicker-backdrop\$"[^\n]*\n?',
    r'^[ \t]*[^\n]*\bbgpicker\.sh\b[^\n]*\n?',
    r'^[ \t]*[^\n]*\bbgpickerctl\b[^\n]*\n?',
]
for pat in line_patterns:
    text = re.sub(pat, "", text, flags=re.MULTILINE)

# Drop layer-rule blocks that became empty or contain only place-within-backdrop after stripping.
def prune_empty_backdrop_rule(match: re.Match[str]) -> str:
    body = match.group(1)
    if re.search(r'\bmatch\b', body):
        return match.group(0)
    if "place-within-backdrop" in body:
        return ""
    return match.group(0)

text = re.sub(
    r'(?m)^[ \t]*(?://[^\n]*\n)?[ \t]*layer-rule\s*\{([^}]*)\}\s*\n?',
    prune_empty_backdrop_rule,
    text,
)

# Collapse excessive blank lines left behind.
text = re.sub(r'\n{3,}', "\n\n", text)

if text != original:
    cfg_path.write_text(text, encoding="utf-8")
    import shutil, subprocess
    if shutil.which("niri"):
        res = subprocess.run(["niri", "validate", "-c", str(cfg_path)], capture_output=True)
        if res.returncode != 0:
            cfg_path.write_text(original, encoding="utf-8")
            raise SystemExit(1)
    print(f"[uninstall] cleaned {cfg_path} (backup saved to {backup})")
else:
    Path(backup).unlink(missing_ok=True)
PY
}

stop_processes

if ! $SKIP_NIRI_CONFIG; then
    clean_niri_config "$NIRI_CONFIG" "$SCRIPT_DIR" || {
        warn "failed to cleanly validate updated $NIRI_CONFIG; original config restored"
    }
fi

# 3. Remove symlinks in BIN_DIR that point into SCRIPT_DIR (or match BgPicker names pointing into bgpicker).
for bin in "${BGPICKER_BINS[@]}"; do
    dst="$BIN_DIR/$bin"
    if [[ -L "$dst" ]]; then
        target="$(readlink -f "$dst" 2>/dev/null || true)"
        if [[ -z "$target" || "$target" == "$SCRIPT_DIR/"* ]]; then
            run_or_dry rm -f "$dst"
        fi
    fi
done

# 4. Remove XDG config, state, cache, legacy pointer, and runtime sockets.
for dir in "$CFG_DIR" "$STATE_DIR" "$CACHE_DIR"; do
    if [[ -d "$dir" ]]; then
        run_or_dry rm -rf "$dir"
    fi
done

if [[ -f "$LEGACY_SWAYBG_CACHE" ]]; then
    legacy_target="$(cat "$LEGACY_SWAYBG_CACHE" 2>/dev/null || true)"
    if [[ "$legacy_target" == "$CACHE_DIR/"* || -z "$legacy_target" || ! -e "$legacy_target" ]]; then
        run_or_dry rm -f "$LEGACY_SWAYBG_CACHE"
    fi
fi

shopt -s nullglob
for sock in \
    "$RUNTIME_BASE"/bgpicker-*.sock \
    "$RUNTIME_BASE"/bgpicker-picker-*.sock
do
    run_or_dry rm -f "$sock"
done
shopt -u nullglob

# 5. Clean local build & bytecode artifacts inside repository.
for rel in native/build BgPickerVideo __pycache__ crates/__pycache__ tests/__pycache__; do
    if [[ -e "$SCRIPT_DIR/$rel" ]]; then
        run_or_dry rm -rf "$SCRIPT_DIR/$rel"
    fi
done

if ! $KEEP_TARGET && [[ -d "$SCRIPT_DIR/target" ]]; then
    run_or_dry rm -rf "$SCRIPT_DIR/target"
fi

if $PURGE_MEDIA; then
    for media_dir in "$SCRIPT_DIR/wallpapers" "$SCRIPT_DIR/dynamic"; do
        if [[ -d "$media_dir" ]]; then
            while IFS= read -r -d '' f; do
                run_or_dry rm -rf "$f"
            done < <(find "$media_dir" -mindepth 1 -maxdepth 1 ! -name '.gitkeep' -print0)
        fi
    done
fi

if $PURGE_REPO; then
    log "removing repository directory $SCRIPT_DIR..."
    if $DRY_RUN; then
        log "[dry-run] rm -rf $SCRIPT_DIR"
    else
        cd /
        rm -rf "$SCRIPT_DIR"
    fi
fi

log "uninstall complete!"
