#!/bin/bash
# ==============================================================================
# Script Name: bgpicker.sh
# Description: Compatibility CLI wrapper for bgpicker-daemon / bgpickerctl and
#              the Quickshell Carousel picker overlay.
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

resolve_bgpickerctl() {
    local dir="${1:-$SCRIPT_DIR}"
    if [[ -n "${BGPICKERCTL:-}" ]]; then
        printf '%s\n' "$BGPICKERCTL"
        return 0
    fi
    local found
    if found="$(command -v bgpickerctl 2>/dev/null)"; then
        printf '%s\n' "$found"
        return 0
    fi
    if [[ -x "$dir/target/release/bgpickerctl" ]]; then
        printf '%s\n' "$dir/target/release/bgpickerctl"
        return 0
    fi
    if [[ -x "$dir/target/debug/bgpickerctl" ]]; then
        printf '%s\n' "$dir/target/debug/bgpickerctl"
        return 0
    fi
    if command -v cargo >/dev/null 2>&1 && [[ -f "$dir/Cargo.toml" ]]; then
        echo "BgPicker: building bgpickerctl (release)..." >&2
        cargo build --release -p bgpicker-daemon --manifest-path "$dir/Cargo.toml" >&2
        if [[ -x "$dir/target/release/bgpickerctl" ]]; then
            printf '%s\n' "$dir/target/release/bgpickerctl"
            return 0
        fi
    fi
    echo "BgPicker: bgpickerctl not found" >&2
    return 1
}

is_qs_running() {
    qs ipc -p "$SCRIPT_DIR" show &>/dev/null
}

start_qs() {
    if is_qs_running; then
        return 0
    fi
    qs -p "$SCRIPT_DIR" -n -d >/dev/null 2>&1 &
    disown 2>/dev/null || true
    for _ in $(seq 1 50); do
        if is_qs_running; then
            return 0
        fi
        sleep 0.1
    done
    echo "Warning: Quickshell picker may not have started in time." >&2
    return 1
}

stop_qs() {
    if is_qs_running; then
        qs kill -p "$SCRIPT_DIR" >/dev/null 2>&1 || true
        for _ in $(seq 1 50); do
            is_qs_running || return 0
            sleep 0.1
        done
        echo "bgpicker picker has not stopped yet." >&2
        return 1
    fi
    return 0
}

send_ipc() {
    local cmd="$1"
    start_qs || return 1
    qs ipc -p "$SCRIPT_DIR" call wallpaperCarousel "$cmd"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    CTL="$(resolve_bgpickerctl "$SCRIPT_DIR")"
    case "${1:-toggle}" in
        engine)
            shift
            exec "$SCRIPT_DIR/wallpaper_engine.sh" "${1:-status}" "${@:2}"
            ;;
        video)
            case "${2:-status}" in
                status)
                    exec "$CTL" video status
                    ;;
                normal|parallax)
                    [[ $# -le 3 ]] || { echo "Usage: $0 video [status|normal|parallax [file]|next]" >&2; exit 1; }
                    if [[ -n "${3:-}" ]]; then
                        "$CTL" apply-video "$2" "$3"
                    else
                        "$CTL" apply-video "$2"
                    fi
                    start_qs >/dev/null 2>&1 || true
                    ;;
                next)
                    "$CTL" video next
                    start_qs >/dev/null 2>&1 || true
                    ;;
                *)
                    echo "Usage: $0 video [status|normal|parallax [file]|next]" >&2
                    exit 1
                    ;;
            esac
            ;;
        start)
            "$CTL" start
            start_qs
            ;;
        stop)
            stop_qs
            exec "$CTL" stop
            ;;
        toggle)
            "$CTL" start >/dev/null 2>&1 || true
            send_ipc "toggle"
            ;;
        open)
            "$CTL" start >/dev/null 2>&1 || true
            send_ipc "open"
            ;;
        close)
            send_ipc "close"
            ;;
        next)
            "$CTL" start >/dev/null 2>&1 || true
            send_ipc "cycleNext"
            ;;
        prev)
            "$CTL" start >/dev/null 2>&1 || true
            send_ipc "cyclePrevious"
            ;;
        mode)
            if [[ $# -eq 1 ]]; then
                exec "$CTL" mode
            elif [[ $# -eq 2 && ( "$2" == awww || "$2" == parallax || "$2" == toggle ) ]]; then
                exec "$CTL" mode "$2"
            else
                echo "Usage: $0 mode [awww|parallax|toggle]" >&2
                exit 1
            fi
            ;;
        backdrop-parallax)
            if [[ $# -eq 1 ]]; then
                exec "$CTL" backdrop-parallax
            elif [[ $# -eq 2 && ( "$2" == on || "$2" == off || "$2" == toggle ) ]]; then
                exec "$CTL" backdrop-parallax "$2"
            else
                echo "Usage: $0 backdrop-parallax [on|off|toggle]" >&2
                exit 1
            fi
            ;;
        transition)
            if [[ $# -eq 1 ]]; then
                exec "$CTL" transition
            elif [[ $# -eq 2 ]]; then
                exec "$CTL" transition "$2"
            else
                echo "Usage: $0 transition [fade|wipe|wave|grow|outer|random|none]" >&2
                exit 1
            fi
            ;;
        clear-cache|trim)
            shift
            exec "$CTL" clear-cache "$@"
            ;;
        *)
            echo "Usage: $0 {start|stop|toggle|open|close|next|prev|mode [awww|parallax|toggle]|backdrop-parallax [on|off|toggle]|transition [type]|engine [status|simple|full|video|toggle|startup]|video [status|normal|parallax [file]|next]|clear-cache [--deep] [--picker] [--disk] [--all]}" >&2
            exit 1
            ;;
    esac
fi
