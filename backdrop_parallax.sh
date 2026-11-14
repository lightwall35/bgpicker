#!/bin/bash
# Manage the optional niri overview backdrop renderer. The wallpaper switcher
# passes its already-held lock when applying a new cached image.
set -euo pipefail
DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/bgpicker"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker"
BACKDROP_CACHE="$CACHE/backdrop-awww"
NAMESPACE=bgpicker-backdrop
mkdir -p -- "$CACHE" "$STATE"
if [[ "${BGPICKER_LOCK_HELD:-0}" != 1 ]]; then
    exec 9>"$CACHE/switch.lock"
    flock 9
fi
backend() { timeout 3 qs ipc -p "$DIR" call backdropBackend "$@" 9>&-; }
awww_backdrop() { XDG_CACHE_HOME="$BACKDROP_CACHE" awww "$@" --namespace "$NAMESPACE" 9>&-; }
current="$(cat "$STATE/backdrop_parallax" 2>/dev/null || printf off)"
command="${1:-}"
[[ "$command" != toggle ]] || { if [[ "$current" == on ]]; then command=off; else command=on; fi; }
case "$command" in on|off|apply) ;; *) echo "Usage: $0 {on|off|toggle|apply}" >&2; exit 1 ;; esac
if [[ "$command" != apply && "$command" == "$current" ]]; then exit 0; fi
image="${2:-$(cat "$HOME/.cache/current_swaybg_img" 2>/dev/null || true)}"
if [[ ! -f "$image" ]]; then
    original="$(cat "$STATE/current_wallpaper" 2>/dev/null || true)"
    [[ -f "$original" ]] || { echo 'BgPicker: select a wallpaper first' >&2; exit 1; }
    image="$("$DIR/prepare_wallpaper.sh" "$original")"
fi
image="$(realpath -e -- "$image")"
transition='{"type":"none","durationMs":0}'
if [[ -n "${3:-}" ]]; then transition="$3"; fi
# An existing awww surface is retained as a fallback and as the base of
# enable/disable transitions. Never affect the normal wallpaper daemon.
bash "$DIR/backdrop_awww.sh" >/dev/null
wait_mode() {
    local result
    for ((i=0; i<120; i++)); do
        result="$(backend modeStatus)" || return 1
        [[ "$result" == ready ]] && return 0
        [[ "$result" == loading ]] || { echo "BgPicker: $result" >&2; return 1; }
        sleep 0.05
    done
    echo 'BgPicker: backdrop mode transition timed out' >&2; return 1
}
if [[ "$command" == off ]]; then
    awww_backdrop img "$image" --transition-type none
    result="$(backend exitMode)"
    if [[ "$result" == started ]]; then
        if ! wait_mode; then backend cancelMode >/dev/null 2>&1 || true; exit 1; fi
    elif [[ "$result" != 'error: renderer is not active' ]]; then
        echo "BgPicker: $result" >&2; exit 1
    fi
    [[ "$(backend deactivate)" == awww ]] || { echo 'BgPicker: could not disable backdrop renderer' >&2; exit 1; }
else
    staged=false
    cleanup() { if $staged; then backend cancel "$image" >/dev/null 2>&1 || true; fi; }
    trap cleanup EXIT
    # Seed the awww surface when enabling so the mode animation has a base.
    if [[ "$command" == on ]]; then
        awww_backdrop img "$image" --transition-type none
        transition='{"type":"none","durationMs":0,"modeSwitch":"enter"}'
    fi
    result="$(backend stage "$image")"
    [[ "$result" == loading ]] || { echo "BgPicker: $result" >&2; exit 1; }
    staged=true
    ready=false
    for ((i=0; i<150; i++)); do
        result="$(backend ready "$image")"
        if [[ "$result" == ready ]]; then ready=true; break; fi
        [[ "$result" == loading ]] || { echo "BgPicker: $result" >&2; exit 1; }
        sleep 0.1
    done
    $ready || { echo 'BgPicker: backdrop image/niri IPC timed out' >&2; exit 1; }
    [[ "$(backend commit "$image" "$transition")" == ready ]] || { echo 'BgPicker: backdrop commit failed' >&2; exit 1; }
    staged=false
    if [[ "$command" == on ]]; then
        if ! wait_mode; then
            backend deactivate >/dev/null 2>&1 || true
            exit 1
        fi
    fi
    if ! awww_backdrop clear 00000000; then
        awww_backdrop img "$image" --transition-type none >/dev/null 2>&1 || true
        backend deactivate >/dev/null 2>&1 || true
        exit 1
    fi
fi
if [[ "$command" != apply ]]; then
    printf '%s\n' "$command" > "$STATE/backdrop_parallax.tmp.$$"
    mv -f -- "$STATE/backdrop_parallax.tmp.$$" "$STATE/backdrop_parallax"
fi
