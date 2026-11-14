#!/bin/bash
# ==============================================================================
# Script Name: bgpicker.sh
# Description: Convenience script to launch and control the bgpicker daemon.
#
# Usage:
#   bgpicker.sh start    — Start the bgpicker daemon
#   bgpicker.sh toggle   — Toggle the carousel overlay (starts daemon if needed)
#   bgpicker.sh open     — Open the carousel overlay
#   bgpicker.sh close    — Close the carousel overlay
#   bgpicker.sh next     — Open (if closed) and highlight next wallpaper
#   bgpicker.sh prev     — Open (if closed) and highlight previous wallpaper
#   bgpicker.sh stop     — Stop the bgpicker daemon
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Check if the bgpicker daemon is running
is_running() {
    qs ipc -p "$SCRIPT_DIR" show &>/dev/null
    return $?
}

# Start the daemon in the background
start_daemon() {
    if is_running; then
        echo "bgpicker daemon is already running."
        return 0
    fi

    echo "Starting bgpicker daemon..."
    qs -p "$SCRIPT_DIR" -n -d 2>&1 &
    disown

    # Wait for daemon to be ready (up to 5 seconds)
    for i in $(seq 1 50); do
        if is_running; then
            echo "bgpicker daemon started successfully."
            return 0
        fi
        sleep 0.1
    done

    echo "Warning: daemon may not have started in time." >&2
    return 1
}

# Send an IPC command to the carousel
send_ipc() {
    local cmd="$1"

    if ! is_running; then
        start_daemon || return
    fi

    qs ipc -p "$SCRIPT_DIR" call wallpaperCarousel "$cmd"
}

# ── Main ───────────────────────────────────────────────────────────────────────
case "${1:-toggle}" in
    start)
        start_daemon
        ;;
    stop)
        if is_running; then
            qs kill -p "$SCRIPT_DIR"
            # qs kill returns before the process exits. Wait so a following
            # start cannot mistake the dying IPC endpoint for a live daemon.
            for ((attempt = 0; attempt < 50; attempt++)); do
                is_running || break
                sleep 0.1
            done
            if is_running; then
                echo "bgpicker daemon has not stopped yet." >&2
                exit 1
            fi
            echo "bgpicker daemon stopped."
        else
            echo "bgpicker daemon is not running."
        fi
        ;;
    toggle)
        send_ipc "toggle"
        ;;
    open)
        send_ipc "open"
        ;;
    close)
        send_ipc "close"
        ;;
    next)
        send_ipc "cycleNext"
        ;;
    prev)
        send_ipc "cyclePrevious"
        ;;
    mode)
        if [[ $# -eq 1 ]]; then
            cat -- "${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker/mode" 2>/dev/null || printf 'awww\n'
        else
            case "$2" in
                awww|parallax|toggle) ;;
                *) echo "Usage: $0 mode [awww|parallax|toggle]" >&2; exit 1 ;;
            esac
            is_running || start_daemon || exit 1
            exec "$SCRIPT_DIR/switch_wallpaper.sh" --mode "$2"
        fi
        ;;
    backdrop-parallax)
        if [[ $# -eq 1 ]]; then
            cat -- "${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker/backdrop_parallax" 2>/dev/null || printf 'off\n'
        elif [[ $# -eq 2 && ( "$2" == on || "$2" == off || "$2" == toggle ) ]]; then
            is_running || start_daemon || exit 1
            exec bash "$SCRIPT_DIR/backdrop_parallax.sh" "$2"
        else
            echo "Usage: $0 backdrop-parallax [on|off|toggle]" >&2; exit 1
        fi
        ;;
    transition)
        if [[ $# -eq 1 ]]; then
            exec python3 "$SCRIPT_DIR/wallpaper_settings.py" show
        elif [[ $# -eq 2 ]]; then
            python3 "$SCRIPT_DIR/wallpaper_settings.py" set-transition "$2" || exit 1
            qs ipc -p "$SCRIPT_DIR" call wallpaperBackend reloadSettings >/dev/null 2>&1 || true
        else
            echo "Usage: $0 transition [fade|wipe|wave|grow|outer|random|none]" >&2
            exit 1
        fi
        ;;
    *)
        echo "Usage: $0 {start|stop|toggle|open|close|next|prev|mode [awww|parallax|toggle]|backdrop-parallax [on|off|toggle]|transition [type]}"
        exit 1
        ;;
esac
