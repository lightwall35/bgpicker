#!/bin/bash
# Select the full desktop or the two-swaybg lightweight desktop.
set -euo pipefail
DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/bgpicker"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker"
mkdir -p "$CACHE" "$STATE"
exec 8>"$CACHE/engine.lock"
flock 8
current="$(cat "$STATE/engine" 2>/dev/null || printf full)"
[[ "$current" == simple || "$current" == full || "$current" == video ]] || current=full
request="${1:-status}"
is_startup=false
[[ "$request" == startup ]] && is_startup=true
case "$request" in
    status) echo "$current"; exit ;;
    toggle) if [[ "$current" == full ]]; then request=simple; else request=full; fi ;;
    startup) request="$current" ;;
    full|simple|video) ;;
    *) echo "Usage: $0 [status|simple|full|video|toggle|startup]" >&2; exit 1 ;;
esac
save_engine() {
    printf '%s\n' "$1" > "$STATE/engine.tmp.$$"
    mv -f "$STATE/engine.tmp.$$" "$STATE/engine"
}
awww_live() {
    local ns="${1:-}"
    if [[ -n "${XDG_RUNTIME_DIR:-}" && -n "${WAYLAND_DISPLAY:-}" && "$(command -v awww 2>/dev/null)" == /usr/* ]]; then
        [[ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY-awww-daemon${ns:+.$ns}.sock" ]] || return 1
    fi
    if [[ -n "$ns" ]]; then
        XDG_CACHE_HOME="$CACHE/backdrop-awww" awww query --namespace "$ns" >/dev/null 2>&1
    else
        awww query >/dev/null 2>&1
    fi
}
if [[ "$request" == video ]]; then
    video_mode="${2:-$(cat "$STATE/video-mode" 2>/dev/null || printf normal)}"
    video_path="${3:-$(cat "$STATE/video-wallpaper" 2>/dev/null || true)}"
    if [[ -z "$video_path" ]]; then
        for candidate in "$DIR"/dynamic/*; do
            [[ -f "$candidate" ]] || continue
            case "${candidate,,}" in *.mp4|*.mkv|*.webm|*.mov) video_path="$candidate"; break ;; esac
        done
    fi
    if [[ -z "$video_path" ]] || ! python3 "$DIR/video_wallpaper.py" apply "$video_path" "$video_mode" 8>&-; then
        [[ -n "$video_path" ]] || echo 'BgPicker: no video found in dynamic/' >&2
        if $is_startup; then
            echo 'BgPicker: video restore failed; falling back to the static wallpaper' >&2
            exec 8>&-
            exec "$DIR/wallpaper_engine.sh" simple
        fi
        exit 1
    fi
    for target in wallpaperBackend backdropBackend; do
        qs ipc -p "$DIR" call "$target" deactivate >/dev/null 2>&1 || true
    done
    if awww_live bgpicker-backdrop; then XDG_CACHE_HOME="$CACHE/backdrop-awww" awww kill --namespace bgpicker-backdrop >/dev/null 2>&1 || true; fi
    if awww_live; then awww kill >/dev/null 2>&1 || true; fi
    "$DIR/simple_wallpaper.sh" --stop
    save_engine video
    "$DIR/bgpicker.sh" start 8>&-
elif [[ "$request" == simple ]]; then
    "$DIR/simple_wallpaper.sh" 8>&-
    # Stop only this project's shell and its dedicated backdrop instance.
    "$DIR/bgpicker.sh" stop
    if awww_live bgpicker-backdrop; then XDG_CACHE_HOME="$CACHE/backdrop-awww" awww kill --namespace bgpicker-backdrop >/dev/null 2>&1 || true; fi
    if awww_live; then awww kill >/dev/null 2>&1 || true; fi
    save_engine simple
    python3 "$DIR/video_wallpaper.py" stop 8>&-
else
    target_image="${2:-$(cat "$STATE/current_wallpaper" 2>/dev/null || true)}"
    target_mode="${3:-$(cat "$STATE/mode" 2>/dev/null || printf awww)}"
    [[ "$target_mode" == awww || "$target_mode" == parallax ]] || target_mode=awww
    handoff=false
    if [[ "$current" == video && -n "$target_image" ]]; then
        rc=0
        python3 "$DIR/video_wallpaper.py" transition-to-static "$target_image" "$target_mode" 8>&- || rc=$?
        if [[ "$rc" -eq 0 ]]; then
            handoff=true
        elif [[ "$rc" -ne 2 ]]; then
            exit "$rc"
        fi
    fi
    # Save first so the shell's startup restore uses the complete backend.
    save_engine full
    if $handoff; then
        python3 "$DIR/sync_awww_cache.py" "$target_image" 8>&- 2>/dev/null || true
    fi
    need_awww=false
    if [[ "$target_mode" == awww ]] || { ! $handoff && $is_startup; }; then
        need_awww=true
    fi
    if $need_awww && ! awww_live; then
        daemon_args=()
        if $handoff && [[ "$target_mode" == parallax ]]; then
            daemon_args=(--no-cache)
        fi
        setsid awww-daemon "${daemon_args[@]}" 8>&- </dev/null >>"$CACHE/awww.log" 2>&1 &
        for ((i=0;i<50;i++)); do
            awww query >/dev/null 2>&1 && break
            sleep 0.1
        done
    fi
    if $handoff && [[ "$target_mode" == parallax ]] && awww_live; then
        awww clear 00000000 8>&- >/dev/null 2>&1 || true
    fi
    apply_args=(--restore)
    [[ -z "${2:-}" ]] || apply_args=("$2")
    [[ -z "${3:-}" ]] || apply_args=(--mode "$3" "$2")
    if { [[ "$target_mode" == awww ]] && ! awww_live; } || ! "$DIR/bgpicker.sh" start 8>&- || ! BGPICKER_HANDOFF="$handoff" "$DIR/switch_wallpaper.sh" "${apply_args[@]}" 8>&-; then
        save_engine "$current"
        if [[ "$current" != full ]]; then
            "$DIR/bgpicker.sh" stop || true
            XDG_CACHE_HOME="$CACHE/backdrop-awww" awww kill --namespace bgpicker-backdrop >/dev/null 2>&1 || true
            awww kill >/dev/null 2>&1 || true
        fi
        echo 'BgPicker: full mode could not start; previous mode remains selected' >&2
        exit 1
    fi
    "$DIR/simple_wallpaper.sh" --stop
    python3 "$DIR/video_wallpaper.py" stop 8>&-
fi
printf 'BgPicker: %s mode\n' "$request"
