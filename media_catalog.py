#!/usr/bin/env python3
"""List local image/video wallpaper cards with cached preview thumbnails as JSON."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import subprocess
import sys

from video_wallpaper import ROOT, prepare_image_thumbnail, prepare_thumbnail

IMAGE_SUFFIXES = frozenset({'.jpg', '.jpeg', '.png', '.webp', '.gif',
                            '.bmp', '.jxl', '.avif', '.heif', '.exr'})
VIDEO_SUFFIXES = frozenset({'.mp4', '.mkv', '.webm', '.mov'})


def _build_media_catalog(directory, suffixes, thumbnail_fn, kind):
    if not directory.exists():
        return []
    if not directory.is_dir():
        raise ValueError(f'{kind} directory is not a directory: {directory}')
    sources = sorted((p for p in directory.iterdir()
                      if p.is_file() and p.suffix.lower() in suffixes),
                     key=lambda p: (p.name.casefold(), p.name))

    def make_entry(source):
        # as_uri quotes spaces, Unicode, # and % correctly for QML Image/Video.
        original = source.resolve()
        entry = {'fileName': source.name, 'fileUrl': original.as_uri(),
                 'thumbnailUrl': '', 'originalPath': str(original)}
        try:
            entry['thumbnailUrl'] = Path(thumbnail_fn(original)).resolve().as_uri()
        except (OSError, RuntimeError, subprocess.SubprocessError) as error:
            entry['thumbnailError'] = str(error)
        return entry

    if not sources:
        return []
    if len(sources) == 1:
        return [make_entry(sources[0])]
    with ThreadPoolExecutor(max_workers=4) as pool:
        return list(pool.map(make_entry, sources))


def build_image_catalog(directory=None):
    """Keep selectable static images in the catalog even if thumbnailing fails."""
    target = Path(directory) if directory is not None else ROOT / 'wallpapers'
    return _build_media_catalog(target, IMAGE_SUFFIXES, prepare_image_thumbnail, 'image')


def build_catalog(directory=None):
    """Keep selectable videos in the catalog even if thumbnail decoding fails."""
    target = Path(directory) if directory is not None else ROOT / 'dynamic'
    return _build_media_catalog(target, VIDEO_SUFFIXES, prepare_thumbnail, 'video')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, default=ROOT / 'dynamic',
                        help='video folder (default: project dynamic/)')
    parser.add_argument('--bundle', nargs=2, type=Path, metavar=('IMAGE_DIR', 'VIDEO_DIR'),
                        help='emit {"images": ..., "videos": ...} for both folders')
    args = parser.parse_args(argv)
    try:
        if args.bundle:
            images = build_image_catalog(args.bundle[0])
            videos = build_catalog(args.bundle[1])
            payload = {'images': images, 'videos': videos}
            all_entries = images + videos
        else:
            all_entries = build_catalog(args.directory)
            payload = all_entries
    except (OSError, ValueError) as error:
        print(f'BgPicker: cannot read media catalog: {error}', file=sys.stderr)
        return 1
    # Stdout is exclusively the JSON document consumed by the picker.
    print(json.dumps(payload, ensure_ascii=False))
    for entry in all_entries:
        if entry.get('thumbnailError'):
            print(f"BgPicker: thumbnail unavailable for {entry['fileName']}: "
                  f"{entry['thumbnailError']}", file=sys.stderr)
    return 0


if __name__ == '__main__':
    sys.exit(main())
