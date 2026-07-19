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
        start_daemon
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
    *)
        echo "Usage: $0 {start|stop|toggle|open|close|next|prev}"
        exit 1
        ;;
esac
