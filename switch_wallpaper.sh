#!/bin/bash
# ==============================================================================
# Script Name: switch_wallpaper.sh
# Description: Switches wallpapers on both awww and swaybg backends
#              simultaneously with smooth transitions.
# Usage:       switch_wallpaper.sh <swaybg_image_path> <awww_image_path>
# ==============================================================================

SWAYBG_IMG="$1"
AWWW_IMG="$2"

readonly SWAYBG_CACHE="$HOME/.cache/current_swaybg_img"

# ------------------------------------------------------------------------------
# 1. Switch awww (workspace wallpaper) with fade transition
# ------------------------------------------------------------------------------
if [[ -n "$AWWW_IMG" ]] && [[ -f "$AWWW_IMG" ]]; then
    awww img "$AWWW_IMG" \
        --transition-fps 60 \
        --transition-type fade \
        --transition-duration 0.8 &
fi

# ------------------------------------------------------------------------------
# 2. Switch swaybg (background wallpaper) — seamless, no black flash
# ------------------------------------------------------------------------------
if [[ -n "$SWAYBG_IMG" ]] && [[ -f "$SWAYBG_IMG" ]]; then
    # Persist the new image path to the cache file
    echo "$SWAYBG_IMG" > "$SWAYBG_CACHE"

    # Start the new swaybg process in the background and detach it
    swaybg -i "$SWAYBG_IMG" -m fill &
    NEW_PID=$!
    disown "$NEW_PID"

    # Wait for the new image to render before killing old instances
    sleep 0.1

    # Gracefully kill all old swaybg processes
    for pid in $(pgrep -x swaybg); do
        if [ "$pid" != "$NEW_PID" ]; then
            kill "$pid" 2>/dev/null
        fi
    done
fi

exit 0

