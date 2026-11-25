#!/bin/bash
# Apply a card or a page's motion preference via bgpickerctl.
set -euo pipefail

DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bgpicker.sh
source "$DIR/bgpicker.sh"
CTL="$(resolve_bgpickerctl "$DIR")"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker"
engine="$(cat "$STATE/engine" 2>/dev/null || printf full)"

case "${1:-}" in
    video)
        [[ $# -eq 3 ]] || { echo 'Usage: picker_action.sh video MODE FILE | static FILE | mode MODE' >&2; exit 1; }
        exec "$CTL" apply-video "$2" "$3"
        ;;
    static)
        [[ $# -eq 2 ]] || { echo 'Usage: picker_action.sh video MODE FILE | static FILE | mode MODE' >&2; exit 1; }
        exec "$CTL" apply-static "$2"
        ;;
    mode)
        [[ $# -eq 2 ]] || { echo 'Usage: picker_action.sh video MODE FILE | static FILE | mode MODE' >&2; exit 1; }
        if [[ "$engine" != full ]]; then
            mode="$(cat "$STATE/mode" 2>/dev/null || printf awww)"
            if [[ "$2" == toggle ]]; then
                if [[ "$mode" == parallax ]]; then mode=awww; else mode=parallax; fi
            else
                mode="$2"
            fi
            current="$(cat "$STATE/current_wallpaper" 2>/dev/null || true)"
            [[ -n "$current" ]] || { echo 'BgPicker: select an existing wallpaper first' >&2; exit 1; }
            exec "$CTL" apply-static "$current" --mode "$mode"
        fi
        exec "$CTL" mode "$2"
        ;;
    *)
        echo 'Usage: picker_action.sh video MODE FILE | static FILE | mode MODE' >&2
        exit 1
        ;;
esac
