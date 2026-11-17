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
    engine)
        exec "$SCRIPT_DIR/wallpaper_engine.sh" "${2:-status}"
        ;;
    video)
        case "${2:-status}" in
            status)
                printf 'engine: %s\nmode: %s\nvideo: %s\n' \
                    "$(cat "${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker/engine" 2>/dev/null || printf full)" \
                    "$(cat "${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker/video-mode" 2>/dev/null || printf normal)" \
                    "$(cat "${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker/video-wallpaper" 2>/dev/null || true)"
                ;;
            normal|parallax)
                [[ $# -le 3 ]] || { echo "Usage: $0 video [status|normal|parallax [file]|next]" >&2; exit 1; }
                exec "$SCRIPT_DIR/wallpaper_engine.sh" video "$2" "${3:-}"
                ;;
            next)
                shopt -s nullglob
                files=()
                for file in "$SCRIPT_DIR"/dynamic/*; do
                    [[ -f "$file" ]] || continue
                    case "${file,,}" in *.mp4|*.mkv|*.webm|*.mov) files+=("$file");; esac
                done
                ((${#files[@]})) || { echo 'BgPicker: no videos found in dynamic/' >&2; exit 1; }
                current="$(cat "${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker/video-wallpaper" 2>/dev/null || true)"
                index=0
                for i in "${!files[@]}"; do
                    if [[ "${files[$i]}" == "$current" ]]; then index=$(((i + 1) % ${#files[@]})); break; fi
                done
                video_mode="$(cat "${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker/video-mode" 2>/dev/null || printf normal)"
                exec "$SCRIPT_DIR/wallpaper_engine.sh" video "$video_mode" "${files[$index]}"
                ;;
            *) echo "Usage: $0 video [status|normal|parallax [file]|next]" >&2; exit 1 ;;
        esac
        ;;
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
