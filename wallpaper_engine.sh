#!/bin/bash
# Select the full, simple, or video wallpaper engine via bgpickerctl.
set -euo pipefail

DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bgpicker.sh
source "$DIR/bgpicker.sh"
CTL="$(resolve_bgpickerctl "$DIR")"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker"
request="${1:-status}"

sync_picker_for_engine() {
    local active_engine
    active_engine="$("$CTL" engine status 2>/dev/null || cat "$STATE/engine" 2>/dev/null || printf full)"
    if [[ "$active_engine" == simple ]]; then
        stop_qs >/dev/null 2>&1 || true
    else
        start_qs >/dev/null 2>&1 || true
    fi
}

case "$request" in
    status)
        if [[ "$(cat "$STATE/engine" 2>/dev/null || true)" == simple ]]; then
            printf 'simple\n'
        else
            exec "$CTL" engine status
        fi
        ;;
    startup)
        "$CTL" startup
        sync_picker_for_engine
        ;;
    simple)
        "$CTL" engine simple
        stop_qs >/dev/null 2>&1 || true
        ;;
    video)
        if [[ -n "${3:-}" ]]; then
            "$CTL" apply-video "${2:-normal}" "$3"
        elif [[ -n "${2:-}" ]]; then
            "$CTL" apply-video "$2"
        else
            "$CTL" engine video
        fi
        start_qs >/dev/null 2>&1 || true
        ;;
    full)
        if [[ -n "${2:-}" && -n "${3:-}" ]]; then
            "$CTL" apply-static "$2" --mode "$3"
        elif [[ -n "${2:-}" ]]; then
            "$CTL" apply-static "$2"
        else
            "$CTL" engine full
        fi
        start_qs >/dev/null 2>&1 || true
        ;;
    toggle)
        "$CTL" engine toggle
        sync_picker_for_engine
        ;;
    *)
        echo "Usage: $0 [status|simple|full|video|toggle|startup]" >&2
        exit 1
        ;;
esac
