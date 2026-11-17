#!/usr/bin/env python3
"""List local video wallpaper cards with cached preview thumbnails as JSON."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

from video_wallpaper import ROOT, prepare_thumbnail

VIDEO_SUFFIXES = frozenset({'.mp4', '.mkv', '.webm', '.mov'})


def build_catalog(directory=None):
    """Keep selectable videos in the catalog even if thumbnail decoding fails."""
    directory = Path(directory) if directory is not None else ROOT / 'dynamic'
    if not directory.exists():
        return []
    if not directory.is_dir():
        raise ValueError(f'video directory is not a directory: {directory}')
    sources = sorted((p for p in directory.iterdir()
                      if p.is_file() and p.suffix.lower() in VIDEO_SUFFIXES),
                     key=lambda p: (p.name.casefold(), p.name))
    entries = []
    for source in sources:
        # as_uri quotes spaces, Unicode, # and % correctly for QML Image/Video.
        original = source.resolve()
        entry = {'fileName': source.name, 'fileUrl': original.as_uri(),
                 'thumbnailUrl': '', 'originalPath': str(original)}
        try:
            entry['thumbnailUrl'] = Path(prepare_thumbnail(original)).resolve().as_uri()
        except (OSError, RuntimeError, subprocess.SubprocessError) as error:
            entry['thumbnailError'] = str(error)
        entries.append(entry)
    return entries


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, default=ROOT / 'dynamic',
                        help='video folder (default: project dynamic/)')
    args = parser.parse_args(argv)
    try:
        entries = build_catalog(args.directory)
    except (OSError, ValueError) as error:
        print(f'BgPicker: cannot read video catalog: {error}', file=sys.stderr)
        return 1
    # Stdout is exclusively the JSON document consumed by the picker.
    print(json.dumps(entries, ensure_ascii=False))
    for entry in entries:
        if entry.get('thumbnailError'):
            print(f"BgPicker: thumbnail unavailable for {entry['fileName']}: "
                  f"{entry['thumbnailError']}", file=sys.stderr)
    return 0


if __name__ == '__main__':
    sys.exit(main())
