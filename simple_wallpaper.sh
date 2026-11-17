#!/bin/bash
# Two managed swaybg instances: overview backdrop and desktop foreground.
set -euo pipefail
DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/bgpicker"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/bgpicker"
mkdir -p "$CACHE" "$STATE" "$HOME/.cache"

stop_pid() {
    local file="$1" pid start actual exe
    [[ -f "$file" ]] || return 0
    read -r pid start < "$file" || return 0
    [[ "$pid" =~ ^[0-9]+$ && "${start:-}" =~ ^[0-9]+$ ]] || { rm -f "$file"; return 0; }
    actual="$(awk '{print $22}' "/proc/$pid/stat" 2>/dev/null || true)"
    [[ "$actual" == "$start" ]] || { rm -f "$file"; return 0; }
    exe="$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)"
    case "$exe" in
        "$CACHE/swaybg-front"|"$(command -v swaybg)") kill "$pid" 2>/dev/null || true ;;
    esac
    rm -f "$file"
}
if [[ "${1:-}" == --stop ]]; then
    stop_pid "$STATE/simple-front.pid"
    stop_pid "$STATE/simple-back.pid"
    exit
fi
exec 9>"$CACHE/switch.lock"
flock 9
image="${1:-$(cat "$STATE/current_wallpaper" 2>/dev/null || true)}"
[[ -f "$image" ]] || { echo "BgPicker: no current wallpaper: $image" >&2; exit 1; }
image="$(realpath -e -- "$image")"
blur="$("$DIR/prepare_wallpaper.sh" "$image")"
# swaybg has no namespace option. Generate a local, version-specific copy with
# a distinct Wayland namespace; the installed binary is never changed.
source_binary="$(command -v swaybg)"
python3 - "$source_binary" "$CACHE/swaybg-front" <<'PY'
import os, pathlib, sys, tempfile
source, target = map(pathlib.Path, sys.argv[1:])
data = source.read_bytes()
old, new = b'wallpaper\0', b'bgpick-fg\0'
if data.count(old) != 1 or len(old) != len(new):
    sys.exit('BgPicker: unsupported swaybg binary; cannot distinguish the two layers')
changed = data.replace(old, new)
if not target.exists() or target.read_bytes() != changed:
    fd, name = tempfile.mkstemp(dir=target.parent, prefix='.swaybg-front-')
    try:
        with os.fdopen(fd, 'wb') as f: f.write(changed)
        os.chmod(name, 0o755)
        os.replace(name, target)
    finally:
        if os.path.exists(name): os.unlink(name)
PY
# Start replacements before retiring previous surfaces.
setsid swaybg -i "$blur" -m fill 9>&- </dev/null >>"$CACHE/simple-swaybg.log" 2>&1 & back=$!
setsid "$CACHE/swaybg-front" -i "$image" -m fill 9>&- </dev/null >>"$CACHE/simple-swaybg.log" 2>&1 & front=$!
sleep 0.3
if ! kill -0 "$back" 2>/dev/null || ! kill -0 "$front" 2>/dev/null; then
    kill "$back" "$front" 2>/dev/null || true
    echo "BgPicker: swaybg failed to start; see $CACHE/simple-swaybg.log" >&2
    exit 1
fi
stop_pid "$STATE/simple-front.pid"
stop_pid "$STATE/simple-back.pid"
printf '%s %s\n' "$front" "$(awk '{print $22}' "/proc/$front/stat")" > "$STATE/simple-front.pid"
printf '%s %s\n' "$back" "$(awk '{print $22}' "/proc/$back/stat")" > "$STATE/simple-back.pid"
printf '%s\n' "$image" > "$STATE/current_wallpaper.tmp.$$"
mv -f "$STATE/current_wallpaper.tmp.$$" "$STATE/current_wallpaper"
printf '%s\n' "$blur" > "$HOME/.cache/current_swaybg_img.tmp.$$"
mv -f "$HOME/.cache/current_swaybg_img.tmp.$$" "$HOME/.cache/current_swaybg_img"
