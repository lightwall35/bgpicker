#!/usr/bin/env python3
"""Synchronize awww 0.12.1's per-output startup records without drawing."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def sync(image):
    image = str(Path(image).resolve(strict=True))
    version = subprocess.run(['awww', '--version'], capture_output=True, text=True, timeout=3, check=True).stdout.strip()
    if version != 'awww 0.12.1':
        raise ValueError(f'unsupported awww cache format: {version}')
    cache = Path(os.environ.get('XDG_CACHE_HOME') or Path.home() / '.cache') / 'awww' / '0.12.1'
    cache.mkdir(mode=0o700, parents=True, exist_ok=True)
    outputs = set()
    try:
        result = subprocess.run(['niri', 'msg', '--json', 'outputs'], capture_output=True, text=True, timeout=3, check=True)
        outputs.update(name for name, output in json.loads(result.stdout).items() if output.get('logical') is not None)
    except (OSError, ValueError, subprocess.SubprocessError):
        pass
    # Cached entries also cover previously connected outputs. Ignore animation
    # files and do not modify files that do not parse as awww output records.
    existing = {}
    for file in cache.iterdir():
        if not file.is_file() or file.is_symlink():
            continue
        try:
            raw = file.read_bytes()
            parts = raw.decode('utf-8').split('\0')
            if not raw or len(parts) % 4 or any(not parts[i+1] or not parts[i+2] or not parts[i+3] for i in range(0, len(parts), 4)):
                raise ValueError('invalid cache entry')
            existing[file.name] = [parts[i:i+4] for i in range(0, len(parts), 4)]
        except (UnicodeError, ValueError, OSError):
            if file.name in outputs:
                raise ValueError(f'invalid cache for output {file.name}')
    outputs.update(existing)
    if not outputs:
        raise ValueError('no outputs available to update')
    for name in outputs:
        if name in ('.', '..') or '/' in name or '\0' in name:
            raise ValueError('invalid output name')
        entries = existing.get(name, [])
        for entry in entries:
            if entry[0] == '':
                entry[3] = image
                break
        else:
            entries.insert(0, ['', 'crop:center', 'Lanczos3', image])
        payload = '\0'.join(value for entry in entries for value in entry).encode('utf-8')
        fd, temp = tempfile.mkstemp(prefix='.bgpicker-', dir=cache)
        try:
            with os.fdopen(fd, 'wb') as stream:
                stream.write(payload)
            os.replace(temp, cache / name)
        finally:
            if os.path.exists(temp):
                os.unlink(temp)


if __name__ == '__main__':
    try:
        if len(sys.argv) != 2:
            raise ValueError('expected an image path')
        sync(sys.argv[1])
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f'BgPicker: cannot synchronize awww startup cache: {error}', file=sys.stderr)
        sys.exit(1)
