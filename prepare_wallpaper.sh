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
printf '%s\n' "$BLURRED"
