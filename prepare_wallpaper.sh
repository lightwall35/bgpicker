#!/bin/bash
# Print the path to an original's blurred variant, generating it on cache miss.
# Usage: prepare_wallpaper.sh <original_image_path>
set -euo pipefail

if [[ $# -ne 1 || ! -f "$1" ]]; then
    echo "Usage: $0 <original_image_path> (must be an existing image)" >&2
    exit 1
fi

ORIGINAL="$(realpath -e -- "$1")"
BLUR_SIZE="${BGPICKER_BLUR_SIZE:-1920x1080}"
BLUR_SIGMA="${BGPICKER_BLUR_SIGMA:-20}"
if [[ ! "$BLUR_SIZE" =~ ^[1-9][0-9]*x[1-9][0-9]*$ || ! "$BLUR_SIGMA" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
    echo "BgPicker: invalid BGPICKER_BLUR_SIZE or BGPICKER_BLUR_SIGMA" >&2
    exit 1
fi

CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/bgpicker/blur"
mkdir -p -- "$CACHE_DIR"
# Fast metadata index avoids re-reading tens of MiB on every wallpaper switch.
META_INFO="$(stat -c '%n|%i|%s|%Y|%y' -- "$ORIGINAL")"
META_KEY="$(printf '%s\0%s\0%s\0%s' 'blur-meta-v1' "$META_INFO" "$BLUR_SIZE" "$BLUR_SIGMA" | sha256sum)"
META_KEY="${META_KEY%% *}"
META_REF="$CACHE_DIR/$META_KEY.ref"
if [[ -r "$META_REF" ]]; then
    CACHED_BLUR="$(cat -- "$META_REF" 2>/dev/null || true)"
    if [[ -n "$CACHED_BLUR" && -s "$CACHED_BLUR" ]]; then
        printf '%s\n' "$CACHED_BLUR"
        exit 0
    fi
fi

# Content and processing settings identify a variant, independently of filenames.
SOURCE_HASH="$(sha256sum < "$ORIGINAL")"
SOURCE_HASH="${SOURCE_HASH%% *}"
CACHE_KEY="$(printf '%s\0%s\0%s\0%s' 'blur-v1' "$SOURCE_HASH" "$BLUR_SIZE" "$BLUR_SIGMA" | sha256sum)"
CACHE_KEY="${CACHE_KEY%% *}"
BLURRED="$CACHE_DIR/$CACHE_KEY.png"

exec 8>"$CACHE_DIR/$CACHE_KEY.lock"
flock 8
if [[ ! -s "$BLURRED" ]]; then
    TEMP_IMAGE="$(mktemp "$CACHE_DIR/.blur-XXXXXXXX.png")"
    trap 'rm -f -- "$TEMP_IMAGE"' EXIT
    # Only the first frame is used for the static background. Downsize before blur.
    magick -limit thread 2 "${ORIGINAL}[0]" -auto-orient \
        -thumbnail "${BLUR_SIZE}>" -blur "0x${BLUR_SIGMA}" -strip "$TEMP_IMAGE"
    [[ -s "$TEMP_IMAGE" ]]
    mv -f -- "$TEMP_IMAGE" "$BLURRED"
fi
META_TMP="$(mktemp "$CACHE_DIR/.meta-XXXXXXXX")"
printf '%s\n' "$BLURRED" > "$META_TMP"
mv -f -- "$META_TMP" "$META_REF"
printf '%s\n' "$BLURRED"
