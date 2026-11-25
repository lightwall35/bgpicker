#!/bin/bash
# Manage the niri overview blurred backdrop parallax toggle via bgpickerctl.
set -euo pipefail

DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bgpicker.sh
source "$DIR/bgpicker.sh"
CTL="$(resolve_bgpickerctl "$DIR")"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker"

case "${1:-}" in
    on|off|toggle)
        exec "$CTL" backdrop-parallax "$1"
        ;;
    apply)
        current="$(cat "$STATE/backdrop_parallax" 2>/dev/null || printf off)"
        exec "$CTL" backdrop-parallax "$current"
        ;;
    *)
        echo "Usage: $0 {on|off|toggle|apply}" >&2
        exit 1
        ;;
esac
