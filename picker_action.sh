#!/bin/bash
# Apply a card or a page's motion preference without stopping its picker.
set -euo pipefail
DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker"
engine="$(cat "$STATE/engine" 2>/dev/null || printf full)"
case "${1:-}" in
    video)
        exec "$DIR/wallpaper_engine.sh" video "$2" "$3"
        ;;
    static)
        if [[ "$engine" == full ]]; then
            exec "$DIR/switch_wallpaper.sh" "$2"
        fi
        exec "$DIR/wallpaper_engine.sh" full "$2"
        ;;
    mode)
        if [[ "$engine" != full ]]; then
            mode="$(cat "$STATE/mode" 2>/dev/null || printf awww)"
            if [[ "$2" == toggle ]]; then
                if [[ "$mode" == parallax ]]; then mode=awww; else mode=parallax; fi
            else mode="$2"; fi
            exec "$DIR/wallpaper_engine.sh" full "$(cat "$STATE/current_wallpaper")" "$mode"
        fi
        exec "$DIR/bgpicker.sh" mode "$2"
        ;;
    *) echo 'Usage: picker_action.sh video MODE FILE | static FILE | mode MODE' >&2; exit 1 ;;
esac
