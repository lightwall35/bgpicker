#!/bin/bash
# ==============================================================================
# Script Name: deploy.sh
# Description: From-scratch deployment script for BgPicker on Linux + niri.
#              Checks dependencies, prepares media/config dirs, builds release
#              binaries, links executables into ~/.local/bin, updates
#              niri/config.kdl safely (with backup + `niri validate`), and
#              optionally starts the wallpaper daemon.
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

NIRI_CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/niri/config.kdl"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
LINK_BINS=true
EDIT_NIRI=true
RUN_BUILD=true
START_DAEMON=true
CHECK_ONLY=false
DRY_RUN=false
ADD_BINDS=true

BGPICKER_BINS=(bgpickerctl bgpicker-daemon bgpicker-picker bgpicker-splash)

usage() {
    cat <<'EOF'
Usage: ./deploy.sh [OPTIONS]

Options:
  --niri-config PATH   Override niri config path (default: ~/.config/niri/config.kdl)
  --bin-dir DIR        Directory for CLI symlinks (default: ~/.local/bin)
  --no-bin-link        Do not create symlinks in --bin-dir
  --no-binds           Do not insert default Mod+N / Mod+Shift+B/G binds
  --skip-build         Skip `cargo build --release`
  --skip-niri-config   Skip modifying niri/config.kdl
  --no-start           Skip launching bgpickerctl startup after deploy
  --check-only         Only check build & runtime dependencies, make no changes
  --dry-run            Print planned steps without modifying any files
  -h, --help           Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --niri-config)
            [[ $# -ge 2 ]] || { echo "deploy.sh: --niri-config requires a path" >&2; exit 2; }
            NIRI_CONFIG="$2"
            shift 2
            ;;
        --niri-config=*)
            NIRI_CONFIG="${1#--niri-config=}"
            shift
            ;;
        --bin-dir)
            [[ $# -ge 2 ]] || { echo "deploy.sh: --bin-dir requires a path" >&2; exit 2; }
            BIN_DIR="$2"
            shift 2
            ;;
        --bin-dir=*)
            BIN_DIR="${1#--bin-dir=}"
            shift
            ;;
        --no-bin-link)
            LINK_BINS=false
            shift
            ;;
        --no-binds)
            ADD_BINDS=false
            shift
            ;;
        --skip-build)
            RUN_BUILD=false
            shift
            ;;
        --skip-niri-config)
            EDIT_NIRI=false
            shift
            ;;
        --no-start)
            START_DAEMON=false
            shift
            ;;
        --check-only)
            CHECK_ONLY=true
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
            echo "deploy.sh: unknown option '$1'" >&2
            usage >&2
            exit 2
            ;;
    esac
done

log() {
    printf '[deploy] %s\n' "$*"
}

warn() {
    printf '[deploy] WARN: %s\n' "$*" >&2
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

print_install_hints() {
    cat <<'EOF'

Suggested package installation commands by distribution:
  Arch Linux:
    sudo pacman -S --needed rust pkgconf gcc ffmpeg libdrm wayland libxkbcommon imagemagick swaybg python util-linux
  Debian / Ubuntu:
    sudo apt install -y cargo rustc pkg-config gcc libavformat-dev libavcodec-dev libswscale-dev libavutil-dev libdrm-dev libwayland-dev libxkbcommon-dev libegl-dev imagemagick ffmpeg swaybg python3 util-linux
  Fedora:
    sudo dnf install -y cargo rust pkgconf-pkg-config gcc ffmpeg-free-devel libdrm-devel wayland-devel libxkbcommon-devel mesa-libEGL-devel ImageMagick ffmpeg swaybg python3 util-linux
EOF
}

check_dependencies() {
    local missing_req=()
    local missing_opt=()

    if $RUN_BUILD; then
        for cmd in cargo rustc pkg-config; do
            command -v "$cmd" >/dev/null 2>&1 || missing_req+=("cmd:$cmd")
        done
        if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
            missing_req+=("cmd:cc/gcc/clang")
        fi
        if command -v pkg-config >/dev/null 2>&1; then
            for lib in libavformat libavcodec libswscale libavutil libdrm wayland-client wayland-egl xkbcommon; do
                pkg-config --exists "$lib" 2>/dev/null || missing_req+=("pkg:$lib")
            done
        fi
    fi

    for cmd in python3 magick ffmpeg; do
        command -v "$cmd" >/dev/null 2>&1 || missing_opt+=("$cmd")
    done
    command -v swaybg >/dev/null 2>&1 || missing_opt+=("swaybg (optional, needed for simple mode)")
    command -v niri >/dev/null 2>&1 || missing_opt+=("niri (Wayland compositor)")

    if [[ ${#missing_opt[@]} -gt 0 ]]; then
        warn "optional/runtime tools not found: ${missing_opt[*]}"
    fi

    if [[ ${#missing_req[@]} -gt 0 ]]; then
        echo "[deploy] ERROR: missing required build dependencies: ${missing_req[*]}" >&2
        print_install_hints >&2
        return 1
    fi

    log "all required dependencies satisfied"
    return 0
}

update_niri_config() {
    local cfg="$1"
    local repo="$2"
    local add_binds="$3"
    local cfg_dir
    cfg_dir="$(dirname -- "$cfg")"

    if [[ ! -f "$cfg" ]]; then
        if $DRY_RUN; then
            log "[dry-run] would create $cfg with bgpicker startup & layer-rule"
            return 0
        fi
        mkdir -p "$cfg_dir"
        touch "$cfg"
    fi

    local stamp backup
    stamp="$(date +%Y%m%d-%H%M%S)"
    backup="${cfg}.bak.${stamp}"

    if $DRY_RUN; then
        log "[dry-run] would back up $cfg -> $backup and update managed bgpicker entries"
        return 0
    fi

    cp -a "$cfg" "$backup"

    python3 - "$cfg" "$repo" "$add_binds" <<'PY'
import re
import sys
from pathlib import Path

cfg_path = Path(sys.argv[1])
repo = sys.argv[2]
add_binds = sys.argv[3] == "true"

text = cfg_path.read_text(encoding="utf-8")

START_MARK = "// >>> bgpicker managed start >>>"
END_MARK = "// <<< bgpicker managed end <<<"

# Strip existing managed block(s).
text = re.sub(
    r"(?m)^[ \t]*" + re.escape(START_MARK) + r"[\s\S]*?^[ \t]*" + re.escape(END_MARK) + r"[ \t]*\n?",
    "",
    text,
)

# Remove legacy unmanaged bgpicker startup lines so upgrades stay clean.
legacy_patterns = [
    r'^[ \t]*//壁纸开机独立垫图守护进程[^\n]*\n?',
    r'^[ \t]*//壁纸守护进程\s*\n(?=[ \t]*spawn-at-startup\s+"[^"]*(?:bgpicker|wallpaper_engine\.sh))',
    r'^[ \t]*spawn-at-startup\s+"[^"]*(?:bgpicker-splash|bgpickerctl|wallpaper_engine\.sh|bgpicker/shell\.qml)[^\n]*\n?',
    r'^[ \t]*spawn-at-startup\s+"qs"\s+"-p"\s+"[^"]*bgpicker[^"]*"[^\n]*\n?',
]
for pat in legacy_patterns:
    text = re.sub(pat, "", text, flags=re.MULTILINE)

managed_startup = (
    f"{START_MARK}\n"
    "//壁纸守护进程（内置进程内 wl_shm 开机垫图，首帧 EGL 上屏后自动销毁）\n"
    f'spawn-at-startup "{repo}/target/release/bgpickerctl" "startup"\n'
    f"{END_MARK}\n"
)

text = managed_startup + text.lstrip("\n")

# Ensure overview backdrop rule exists without touching user's other rules.
has_backdrop_rule = (
    'namespace="^bgpicker:backdrop$"' in text
    and "place-within-backdrop" in text
)
if not has_backdrop_rule:
    rule_block = (
        "\n// >>> bgpicker layer-rule start >>>\n"
        "layer-rule {\n"
        '    match namespace="^bgpicker:backdrop$"\n'
        '    match namespace="^wallpaper$"\n'
        "    place-within-backdrop true\n"
        "}\n"
        "// <<< bgpicker layer-rule end <<<\n"
    )
    text = text.rstrip() + "\n" + rule_block

# Optionally add keybinds if `binds {` exists and neither bgpicker bind nor Mod+N is present.
if add_binds and "bgpicker.sh" not in text and "bgpickerctl" not in text.split("binds", 1)[-1]:
    m = re.search(r'(?m)^binds\s*\{\s*\n', text)
    if m and "Mod+N" not in text:
        bind_lines = (
            "    // >>> bgpicker binds start >>>\n"
            f'    Mod+N {{ spawn "{repo}/bgpicker.sh" "toggle"; }}\n'
            f'    Mod+Shift+B {{ spawn "{repo}/bgpicker.sh" "next"; }}\n'
            f'    Mod+Shift+G {{ spawn "{repo}/bgpicker.sh" "prev"; }}\n'
            "    // <<< bgpicker binds end <<<\n"
        )
        pos = m.end()
        text = text[:pos] + bind_lines + text[pos:]

cfg_path.write_text(text, encoding="utf-8")
PY

    if command -v niri >/dev/null 2>&1; then
        if ! niri validate -c "$cfg" >/dev/null 2>&1; then
            warn "niri validate failed for $cfg; restoring backup $backup"
            cp -a "$backup" "$cfg"
            return 1
        fi
    fi

    log "updated $cfg (backup saved to $backup)"
}

check_dependencies
if $CHECK_ONLY; then
    exit 0
fi

# 1. Prepare local directories and default settings.
run_or_dry mkdir -p "$SCRIPT_DIR/wallpapers" "$SCRIPT_DIR/dynamic"
CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/bgpicker"
run_or_dry mkdir -p "$CFG_DIR"
if [[ -f "$SCRIPT_DIR/settings.example.json" && ! -f "$CFG_DIR/settings.json" ]]; then
    run_or_dry cp -n "$SCRIPT_DIR/settings.example.json" "$CFG_DIR/settings.json"
    log "initialized default config at $CFG_DIR/settings.json"
fi

# 2. Build release binaries.
if $RUN_BUILD; then
    log "building release binaries (bgpicker-daemon, bgpickerctl, bgpicker-picker, bgpicker-splash)..."
    run_or_dry cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"
fi

# 3. Symlink binaries into user bin dir.
if $LINK_BINS; then
    run_or_dry mkdir -p "$BIN_DIR"
    for bin in "${BGPICKER_BINS[@]}"; do
        src="$SCRIPT_DIR/target/release/$bin"
        dst="$BIN_DIR/$bin"
        if [[ -e "$src" || $DRY_RUN ]]; then
            if [[ -e "$dst" && ! -L "$dst" ]]; then
                warn "skipping $dst (exists and is not a symlink)"
            else
                run_or_dry ln -sfn "$src" "$dst"
            fi
        fi
    done
    log "linked binaries into $BIN_DIR"
fi

# 4. Configure niri startup, overview layer-rule, and default binds if appropriate.
if $EDIT_NIRI; then
    update_niri_config "$NIRI_CONFIG" "$SCRIPT_DIR" "$ADD_BINDS"
fi

# 5. Start daemon + warm caches if Wayland session is active.
if $START_DAEMON; then
    CTL="$SCRIPT_DIR/target/release/bgpickerctl"
    if [[ -n "${WAYLAND_DISPLAY:-}" && -x "$CTL" ]]; then
        run_or_dry "$CTL" startup-placeholder on >/dev/null 2>&1 || true
        run_or_dry "$CTL" placeholder-cache on >/dev/null 2>&1 || true
        log "starting bgpicker-daemon..."
        run_or_dry "$CTL" startup
    else
        log "skipping live start (no active WAYLAND_DISPLAY or $CTL missing)"
    fi
fi

log "deployment complete!"
if [[ -z "$(find "$SCRIPT_DIR/wallpapers" "$SCRIPT_DIR/dynamic" -maxdepth 1 -type f ! -name '.gitkeep' 2>/dev/null | head -n 1)" ]]; then
    log "tip: add images into $SCRIPT_DIR/wallpapers/ or videos into $SCRIPT_DIR/dynamic/, then press Mod+N or run ./bgpicker.sh toggle"
fi
