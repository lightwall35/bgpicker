#!/bin/bash
# Apply one original in the saved mode, or change modes using the current image.
# Usage: switch_wallpaper.sh [--mode awww|parallax|toggle] [original_image_path]
#        switch_wallpaper.sh --restore
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bgpicker.sh
source "$SCRIPT_DIR/bgpicker.sh"
CTL="$(resolve_bgpickerctl "$SCRIPT_DIR")"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker"
ENGINE="$(cat -- "$STATE_DIR/engine" 2>/dev/null || printf full)"

if [[ "$ENGINE" == video ]]; then
    echo "BgPicker: switch to a static engine before choosing an image" >&2
    exit 1
fi

if [[ "$ENGINE" == simple ]]; then
    case "${1:-}" in
        --restore) exec "$SCRIPT_DIR/simple_wallpaper.sh" ;;
        --mode) echo "BgPicker: mode changes require the full engine" >&2; exit 1 ;;
        *) exec "$SCRIPT_DIR/simple_wallpaper.sh" "$@" ;;
    esac
fi

if [[ "${1:-}" == --restore ]]; then
    shift
    [[ $# -eq 0 ]] || { echo "Usage: $0 [--mode awww|parallax|toggle] [image] | --restore" >&2; exit 1; }
    [[ -f "$STATE_DIR/current_wallpaper" ]] || exit 0
    exec "$CTL" restore
fi

REQUESTED_MODE=""
if [[ "${1:-}" == --mode ]]; then
    REQUESTED_MODE="${2:-}"
    case "$REQUESTED_MODE" in
        awww|parallax|toggle) ;;
        *) echo "BgPicker: mode must be awww, parallax or toggle" >&2; exit 1 ;;
    esac
    shift 2
fi

if [[ $# -eq 0 ]]; then
    if [[ -n "$REQUESTED_MODE" ]]; then
        exec "$CTL" mode "$REQUESTED_MODE"
    fi
    ORIGINAL="$(cat -- "$STATE_DIR/current_wallpaper" 2>/dev/null || true)"
elif [[ $# -eq 1 ]]; then
    ORIGINAL="$1"
else
    echo "Usage: $0 [--mode awww|parallax|toggle] [image] | --restore" >&2
    exit 1
fi

if [[ ! -f "$ORIGINAL" ]]; then
    echo "BgPicker: select an existing wallpaper first: $ORIGINAL" >&2
    exit 1
fi
ORIGINAL="$(realpath -e -- "$ORIGINAL")"

if [[ -n "$REQUESTED_MODE" ]]; then
    if [[ "$REQUESTED_MODE" == toggle ]]; then
        CURRENT_MODE="$(cat -- "$STATE_DIR/mode" 2>/dev/null || printf awww)"
        if [[ "$CURRENT_MODE" == parallax ]]; then REQUESTED_MODE=awww; else REQUESTED_MODE=parallax; fi
    fi
    exec "$CTL" apply-static "$ORIGINAL" --mode "$REQUESTED_MODE"
fi

exec "$CTL" apply-static "$ORIGINAL"
