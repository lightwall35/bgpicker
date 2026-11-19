#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
QSB="${QSB:-/usr/lib/qt6/bin/qsb}"
"$QSB" --qt6 --qsbversion 64 \
    -o "$SCRIPT_DIR/shaders/wallpaper-transition.frag.qsb" \
    "$SCRIPT_DIR/shaders/wallpaper-transition.frag"
"$QSB" --qt6 --qsbversion 64 \
    -o "$SCRIPT_DIR/shaders/kawase-down.frag.qsb" \
    "$SCRIPT_DIR/shaders/kawase-down.frag"
"$QSB" --qt6 --qsbversion 64 \
    -o "$SCRIPT_DIR/shaders/kawase-up.frag.qsb" \
    "$SCRIPT_DIR/shaders/kawase-up.frag"
