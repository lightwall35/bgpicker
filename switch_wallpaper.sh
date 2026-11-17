#!/bin/bash
# Apply one original in the saved mode, or change modes using the current image.
# Usage: switch_wallpaper.sh [--mode awww|parallax|toggle] [original_image_path]
#        switch_wallpaper.sh --restore
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ "$(cat "${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker/engine" 2>/dev/null || printf full)" == video ]]; then
    echo "BgPicker: switch to a static engine before choosing an image" >&2
    exit 1
fi
# The lightweight engine bypasses awww and Quickshell entirely.
if [[ "$(cat "${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker/engine" 2>/dev/null || printf full)" == simple ]]; then
    case "${1:-}" in
        --restore) exec "$SCRIPT_DIR/simple_wallpaper.sh" ;;
        --mode) echo "BgPicker: mode changes require the full engine" >&2; exit 1 ;;
        *) exec "$SCRIPT_DIR/simple_wallpaper.sh" "$@" ;;
    esac
fi
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/bgpicker"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker"
# Existing compositor startup commands read this path; keep it up to date.
SWAYBG_CACHE="$HOME/.cache/current_swaybg_img"
mkdir -p -- "$CACHE_DIR" "$STATE_DIR" "$(dirname -- "$SWAYBG_CACHE")"

# Serialize switches, including calls made outside Quickshell.
exec 9>"$CACHE_DIR/switch.lock"
flock 9
MODE="$(cat -- "$STATE_DIR/mode" 2>/dev/null || printf awww)"
[[ "$MODE" == awww || "$MODE" == parallax ]] || MODE=awww
PREVIOUS_MODE="$MODE"
PREVIOUS_ORIGINAL="$(cat -- "$STATE_DIR/current_wallpaper" 2>/dev/null || true)"
RESTORE=false
if [[ "${1:-}" == --restore ]]; then
    RESTORE=true
    shift
    # A fresh install has no selection to restore.
    [[ -f "$STATE_DIR/current_wallpaper" ]] || exit 0
elif [[ "${1:-}" == --mode ]]; then
    REQUESTED_MODE="${2:-}"
    case "$REQUESTED_MODE" in
        awww|parallax) MODE="$REQUESTED_MODE" ;;
        toggle) if [[ "$MODE" == awww ]]; then MODE=parallax; else MODE=awww; fi ;;
        *) echo "BgPicker: mode must be awww, parallax or toggle" >&2; exit 1 ;;
    esac
    shift 2
fi
if [[ $# -eq 0 ]]; then
    ORIGINAL="$(cat -- "$STATE_DIR/current_wallpaper" 2>/dev/null || true)"
elif [[ $# -eq 1 ]]; then
    ORIGINAL="$1"
else
    echo "Usage: $0 [--mode awww|parallax|toggle] [image] | --restore" >&2
    exit 1
fi
if [[ ! -f "$ORIGINAL" ]]; then
    echo "BgPicker: select an existing wallpaper first: $ORIGINAL" >&2
    exit 1
fi
ORIGINAL="$(realpath -e -- "$ORIGINAL")"
SMOOTH_ENTER=false
SMOOTH_EXIT=false
HANDOFF=false
[[ "${BGPICKER_HANDOFF:-false}" == true ]] && HANDOFF=true
if ! $RESTORE && ! $HANDOFF && [[ "$MODE" != "$PREVIOUS_MODE" ]]; then
    if [[ "$MODE" == parallax ]]; then SMOOTH_ENTER=true; else SMOOTH_EXIT=true; fi
fi
RESOLVE_ARGS=(--shell)
if $RESTORE || $HANDOFF; then RESOLVE_ARGS+=(--instant); fi
# Resolve random once, then give the very same immutable settings to either
# backend. No shell eval: every validated argument is a separate array entry.
TRANSITION_DATA="$(python3 "$SCRIPT_DIR/wallpaper_settings.py" resolve "${RESOLVE_ARGS[@]}")"
mapfile -t TRANSITION_FIELDS <<< "$TRANSITION_DATA"
TRANSITION_JSON="${TRANSITION_FIELDS[0]}"
TRANSITION_ARGS=("${TRANSITION_FIELDS[@]:1}")
BLURRED="$("$SCRIPT_DIR/prepare_wallpaper.sh" "$ORIGINAL")"

# Only retire the legacy swaybg that uses this project's cached image.
PREVIOUS_BLUR="$(cat -- "$SWAYBG_CACHE" 2>/dev/null || true)"
BACKDROP_PARALLAX="$(cat -- "$STATE_DIR/backdrop_parallax" 2>/dev/null || printf off)"
BACKDROP_CACHE="$CACHE_DIR/backdrop-awww"
BACKDROP_NAMESPACE=bgpicker-backdrop
BACKDROP_TOUCHED=false
MAIN_CHANGED=false
BACKDROP_STARTED=false
BACKDROP_SEEDED=false
SWITCH_COMPLETED=false
STAGED=false
AWWW_CLEARED=false
MODE_EXIT_STARTED=false
COLD_RESTORE=false
COLD_RESTORE_COMMITTED=false
backend() {
    timeout 3 qs ipc -p "$SCRIPT_DIR" call wallpaperBackend "$@" 9>&-
}
cleanup() {
    if ! $SWITCH_COMPLETED && $COLD_RESTORE_COMMITTED; then
        if $AWWW_CLEARED; then awww img "$ORIGINAL" --transition-type none 9>&- >/dev/null 2>&1 || true; fi
        backend deactivate >/dev/null 2>&1 || true
        MAIN_CHANGED=false
    fi
    if ! $SWITCH_COMPLETED && $MODE_EXIT_STARTED; then backend cancelMode >/dev/null 2>&1 || true; fi
    if $STAGED; then backend cancel "$ORIGINAL" >/dev/null 2>&1 || true; fi
    if $STAGED && $AWWW_CLEARED && [[ "$PREVIOUS_MODE" == awww && -f "$PREVIOUS_ORIGINAL" ]]; then
        awww img "$PREVIOUS_ORIGINAL" --transition-type none 9>&- >/dev/null 2>&1 || true
    fi
    if $BACKDROP_TOUCHED && $MAIN_CHANGED && [[ -f "$PREVIOUS_ORIGINAL" ]]; then
        if [[ "$PREVIOUS_MODE" == awww ]]; then
            awww img "$PREVIOUS_ORIGINAL" --transition-type none 9>&- >/dev/null 2>&1 || true
            backend deactivate >/dev/null 2>&1 || true
        elif [[ "$(backend stage "$PREVIOUS_ORIGINAL" 2>/dev/null || true)" == loading ]]; then
            for ((retry=0; retry<50; retry++)); do
                if [[ "$(backend ready "$PREVIOUS_ORIGINAL" 2>/dev/null || true)" == ready ]]; then
                    backend commit "$PREVIOUS_ORIGINAL" '{"type":"none","durationMs":0}' >/dev/null 2>&1 || true
                    break
                fi
                sleep 0.1
            done
        fi
    fi
    if $BACKDROP_TOUCHED && [[ -f "$PREVIOUS_BLUR" ]]; then
        if [[ "$BACKDROP_PARALLAX" == on ]]; then
            BGPICKER_LOCK_HELD=1 bash "$SCRIPT_DIR/backdrop_parallax.sh" apply "$PREVIOUS_BLUR" '{"type":"none","durationMs":0}' >/dev/null 2>&1 || true
        else
            XDG_CACHE_HOME="$BACKDROP_CACHE" awww img --namespace "$BACKDROP_NAMESPACE" \
                "$PREVIOUS_BLUR" --transition-type none 9>&- >/dev/null 2>&1 || true
        fi
    fi
    if ! $SWITCH_COMPLETED && $BACKDROP_STARTED && ! $BACKDROP_SEEDED; then
        XDG_CACHE_HOME="$BACKDROP_CACHE" awww kill --namespace "$BACKDROP_NAMESPACE" 9>&- >/dev/null 2>&1 || true
    fi
    if ! $SWITCH_COMPLETED && [[ "$BACKDROP_PARALLAX" != on && -f "$PREVIOUS_BLUR" ]] \
        && ! XDG_CACHE_HOME="$BACKDROP_CACHE" awww query --namespace "$BACKDROP_NAMESPACE" 9>&- >/dev/null 2>&1 \
        && ! pgrep -u "$UID" -x swaybg >/dev/null 2>&1; then
        # Keep a static backdrop available if the new daemon is unavailable.
        setsid swaybg -i "$PREVIOUS_BLUR" -m fill 9>&- </dev/null >>"$CACHE_DIR/swaybg.log" 2>&1 &
        disown 2>/dev/null || true
    fi
    rm -f -- "$STATE_DIR/current_wallpaper.tmp.$$" "$STATE_DIR/mode.tmp.$$" "$SWAYBG_CACHE.tmp.$$"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

BACKDROP_STATE="$(bash "$SCRIPT_DIR/backdrop_awww.sh")"
if [[ "$BACKDROP_STATE" == started ]]; then
    BACKDROP_STARTED=true
    # Seed the old cached frame first, so the first transition has a source.
    SEED_BLUR="$BLURRED"
    if ! $HANDOFF && [[ -f "$PREVIOUS_BLUR" ]]; then SEED_BLUR="$PREVIOUS_BLUR"; fi
    if [[ "$BACKDROP_PARALLAX" == on ]]; then
        XDG_CACHE_HOME="$BACKDROP_CACHE" awww clear --namespace "$BACKDROP_NAMESPACE" 00000000 9>&-
    else
        XDG_CACHE_HOME="$BACKDROP_CACHE" awww img --namespace "$BACKDROP_NAMESPACE" \
            "$SEED_BLUR" --transition-type none 9>&-
    fi
    BACKDROP_SEEDED=true
fi

apply_backdrop() {
    if [[ "$BACKDROP_PARALLAX" == on ]]; then
        # The blurred renderer shares the exact transition snapshot. Restore
        # always reapplies it because the Quickshell process may have restarted.
        if ! $RESTORE && [[ "$BLURRED" == "$PREVIOUS_BLUR" && "$BACKDROP_STATE" == existing ]]; then return 0; fi
        local backdrop_transition
        backdrop_transition="$(python3 -c 'import json,sys; x=json.loads(sys.argv[1]); x.pop("modeSwitch",None); print(json.dumps(x,separators=(",",":")))' "$TRANSITION_JSON")"
        BGPICKER_LOCK_HELD=1 bash "$SCRIPT_DIR/backdrop_parallax.sh" apply "$BLURRED" "$backdrop_transition"
        return
    fi
    # A pure mode change keeps the same backdrop; no redundant animation.
    if [[ "$BLURRED" == "$PREVIOUS_BLUR" && "$BACKDROP_STATE" == existing ]]; then return 0; fi
    XDG_CACHE_HOME="$BACKDROP_CACHE" awww img --namespace "$BACKDROP_NAMESPACE" \
        "$BLURRED" "${TRANSITION_ARGS[@]}" 9>&-
}

await_mode_transition() {
    for ((attempt=0; attempt<120; attempt++)); do
        local result
        result="$(backend modeStatus)" || return 1
        [[ "$result" == ready ]] && return 0
        [[ "$result" == loading ]] || { echo "BgPicker: $result" >&2; return 1; }
        sleep 0.05
    done
    echo "BgPicker: mode transition timed out" >&2
    return 1
}

await_awww_image() {
    for ((attempt=0; attempt<60; attempt++)); do
        if awww query --json 9>&- 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin).get("",[]); sys.exit(not d or not all(x.get("displaying",{}).get("image")==sys.argv[1] for x in d))' "$ORIGINAL" 2>/dev/null; then
            return 0
        fi
        sleep 0.05
    done
    echo "BgPicker: awww did not report the prepared image" >&2
    return 1
}

await_awww_startup() {
    # The daemon and QS start independently. Give awww time to finish its own
    # cached restore, without issuing a redundant img request in the usual case.
    for ((attempt=0; attempt<30; attempt++)); do
        if timeout 2 awww query --json 9>&- 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin).get("",[]); sys.exit(not d or not all(x.get("displaying",{}).get("image")==sys.argv[1] for x in d))' "$ORIGINAL" 2>/dev/null; then
            return 0
        fi
        sleep 0.1
    done
    # A stale/unsupported cache or a new output needs a normal awww request.
    if ! awww query 9>&- >/dev/null 2>&1; then
        echo 'BgPicker: awww was not ready during wallpaper restore' >&2
        return 1
    fi
    awww img "$ORIGINAL" --transition-type none 9>&- || return 1
    await_awww_image
}

if [[ "$MODE" == parallax ]]; then
    # Decode off-screen on every output before exposing the new wallpaper.
    STAGED=true
    # During a QML reload the IPC endpoint can exist before accepting calls.
    # Retry that brief startup condition, but fail explicit renderer errors.
    RESULT=""
    for ((attempt = 0; attempt < 30; attempt++)); do
        if RESULT="$(backend stage "$ORIGINAL" 2>/dev/null)"; then
            [[ "$RESULT" == loading || "$RESULT" == error:* ]] && break
        fi
        sleep 0.1
    done
    [[ "$RESULT" == loading ]] || { echo "BgPicker: ${RESULT:-Quickshell wallpaper backend is unavailable}" >&2; exit 1; }
    READY=false
    for ((attempt = 0; attempt < 150; attempt++)); do
        RESULT="$(backend ready "$ORIGINAL")" || exit 1
        if [[ "$RESULT" == ready ]]; then READY=true; break; fi
        [[ "$RESULT" == loading ]] || { echo "BgPicker: $RESULT" >&2; exit 1; }
        sleep 0.1
    done
    $READY || { echo "BgPicker: parallax image/niri IPC timed out" >&2; exit 1; }
    if $RESTORE; then
        STATUS="$(backend status)" || { echo 'BgPicker: could not inspect the renderer state' >&2; exit 1; }
        ACTIVE="$(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); v=d.get("active"); assert isinstance(v,bool); print("true" if v else "false")' "$STATUS")" || { echo 'BgPicker: invalid renderer state' >&2; exit 1; }
        if [[ "$ACTIVE" == false ]]; then
            await_awww_startup
            COLD_RESTORE=true
            SMOOTH_ENTER=true
        fi
    fi
    if $SMOOTH_ENTER; then
        TRANSITION_JSON="$(python3 -c 'import json,sys; x=json.loads(sys.argv[1]); x["modeSwitch"]="enter"; print(json.dumps(x,separators=(",",":")))' "$TRANSITION_JSON")"
    fi
    BACKDROP_TOUCHED=true
    apply_backdrop &
    BACKDROP_PID=$!
    RESULT="$(backend commit "$ORIGINAL" "$TRANSITION_JSON")" || RESULT="error: renderer unavailable"
    [[ "$RESULT" == ready ]] && MAIN_CHANGED=true
    if $COLD_RESTORE && [[ "$RESULT" == ready ]]; then COLD_RESTORE_COMMITTED=true; fi
    wait "$BACKDROP_PID" || { echo "BgPicker: backdrop update failed" >&2; exit 1; }
    [[ "$RESULT" == ready ]] || { echo "BgPicker: $RESULT" >&2; exit 1; }
    if $SMOOTH_ENTER; then await_mode_transition; fi
    if awww query 9>&- >/dev/null 2>&1; then
        # IPC commit only schedules the first frame. Give Qt and the compositor
        # a brief opportunity to display it before removing the fallback.
        sleep 0.1
        awww clear 00000000 9>&-
        AWWW_CLEARED=true
    fi
    STAGED=false
else
    BACKDROP_TOUCHED=true
    apply_backdrop &
    BACKDROP_PID=$!
    MAIN_RESULT=0
    if $RESTORE; then
        await_awww_startup || MAIN_RESULT=$?
    elif $SMOOTH_EXIT; then
        awww img "$ORIGINAL" --transition-type none 9>&- || MAIN_RESULT=$?
    else
        awww img "$ORIGINAL" "${TRANSITION_ARGS[@]}" 9>&- || MAIN_RESULT=$?
        if $HANDOFF && [[ "$MAIN_RESULT" == 0 ]]; then
            await_awww_image || MAIN_RESULT=$?
        fi
    fi
    [[ "$MAIN_RESULT" == 0 ]] && MAIN_CHANGED=true
    wait "$BACKDROP_PID" || { echo "BgPicker: backdrop update failed" >&2; exit 1; }
    [[ "$MAIN_RESULT" == 0 ]] || exit "$MAIN_RESULT"
    if $SMOOTH_EXIT; then
        await_awww_image
        RESULT="$(backend exitMode)"
        [[ "$RESULT" == started ]] || { echo "BgPicker: $RESULT" >&2; exit 1; }
        MODE_EXIT_STARTED=true
        await_mode_transition
    fi
    # Keep parallax visible until awww has accepted the restored image.
    # Plain command-line use still works without a running picker.
    RESULT="$(backend deactivate 2>/dev/null)" || RESULT=""
    if [[ "$PREVIOUS_MODE" == parallax && "$RESULT" != awww ]]; then
        echo "BgPicker: could not disable the parallax renderer; retry after the picker has loaded" >&2
        exit 1
    fi
fi

# Persist the source separately from the disposable generated image.
printf '%s\n' "$ORIGINAL" > "$STATE_DIR/current_wallpaper.tmp.$$"
mv -f -- "$STATE_DIR/current_wallpaper.tmp.$$" "$STATE_DIR/current_wallpaper"
printf '%s\n' "$MODE" > "$STATE_DIR/mode.tmp.$$"
mv -f -- "$STATE_DIR/mode.tmp.$$" "$STATE_DIR/mode"
printf '%s\n' "$BLURRED" > "$SWAYBG_CACHE.tmp.$$"
mv -f -- "$SWAYBG_CACHE.tmp.$$" "$SWAYBG_CACHE"

BACKDROP_TOUCHED=false
SWITCH_COMPLETED=true
# This is only a startup optimization. An unknown awww version or inaccessible
# cache must not undo a successful wallpaper switch; startup has a fallback.
if [[ "$MODE" == parallax ]]; then
    python3 "$SCRIPT_DIR/sync_awww_cache.py" "$ORIGINAL" 9>&- || true
fi
if [[ -n "$PREVIOUS_BLUR" ]]; then
    while IFS= read -r pid; do
        cmdline="/proc/$pid/cmdline"
        [[ -r "$cmdline" ]] || continue
        mapfile -d '' -t args < "$cmdline" 2>/dev/null || continue
        command_name="${args[0]:-}"
        [[ "${command_name##*/}" == swaybg ]] || continue
        for arg in "${args[@]}"; do
            if [[ "$arg" == "$PREVIOUS_BLUR" ]]; then
                kill "$pid" 2>/dev/null || true
                break
            fi
        done
    done < <(pgrep -u "$UID" -x swaybg || true)
fi

