#!/bin/bash
# Ensure an isolated awww daemon for the niri overview backdrop.
set -euo pipefail
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/bgpicker"
BACKDROP_CACHE="$CACHE_DIR/backdrop-awww"
NAMESPACE=bgpicker-backdrop
mkdir -p -- "$BACKDROP_CACHE"
if XDG_CACHE_HOME="$BACKDROP_CACHE" awww query --namespace "$NAMESPACE" >/dev/null 2>&1; then
    echo existing
    exit 0
fi
setsid env XDG_CACHE_HOME="$BACKDROP_CACHE" awww-daemon --namespace "$NAMESPACE" --no-cache \
    9>&- </dev/null >>"$CACHE_DIR/backdrop-awww.log" 2>&1 &
daemon_pid=$!
for ((attempt=0; attempt<50; attempt++)); do
    if XDG_CACHE_HOME="$BACKDROP_CACHE" awww query --namespace "$NAMESPACE" >/dev/null 2>&1; then
        disown "$daemon_pid" 2>/dev/null || true
        echo started
        exit 0
    fi
    if ! kill -0 "$daemon_pid" 2>/dev/null; then break; fi
    sleep 0.1
done
kill "$daemon_pid" 2>/dev/null || true
echo "BgPicker: backdrop awww failed to start; see $CACHE_DIR/backdrop-awww.log" >&2
exit 1
