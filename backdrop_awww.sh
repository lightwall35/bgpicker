#!/bin/bash
# Legacy backdrop awww helper now backed by bgpicker-daemon.
set -euo pipefail

DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bgpicker.sh
source "$DIR/bgpicker.sh"
CTL="$(resolve_bgpickerctl "$DIR")"
"$CTL" start >/dev/null
echo existing
