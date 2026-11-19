#!/usr/bin/env python3
"""Manage persistent Quickshell video wallpapers and shared transitions."""
import json
import hashlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent
CACHE = Path(os.environ.get('XDG_CACHE_HOME', Path.home() / '.cache')) / 'bgpicker'
STATE = Path(os.environ.get('XDG_STATE_HOME', Path.home() / '.local/state')) / 'bgpicker'
RUNTIME = Path(os.environ.get('XDG_RUNTIME_DIR', '/tmp')) / 'bgpicker-video'


def atomic(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(path.name + f'.tmp.{os.getpid()}')
    temp.write_text(value, encoding='utf-8')
    os.replace(temp, path)


def live_pid(pid, start):
    try:
        stat = Path(f'/proc/{int(pid)}/stat').read_text()
        return stat[stat.rfind(')') + 2:].split()[19] == str(start)
    except (OSError, ValueError, IndexError):
        return False


def process_start(pid):
    stat = Path(f'/proc/{pid}/stat').read_text()
    return stat[stat.rfind(')') + 2:].split()[19]


def stop_record(record):
    try:
        data = json.loads(record.read_text())
        pid, start = data['pid'], data['start']
        if live_pid(pid, start):
            os.kill(int(pid), signal.SIGTERM)
            for _ in range(200):
                if not live_pid(pid, start): break
                time.sleep(.05)
            if live_pid(pid, start):
                if os.getpgid(int(pid)) == int(pid): os.killpg(int(pid), signal.SIGKILL)
                else: os.kill(int(pid), signal.SIGKILL)
    except (OSError, ValueError, KeyError, json.JSONDecodeError):
        pass
    record.unlink(missing_ok=True)


def prepare_thumbnail(video):
    """Create a reusable thumbnail for a video carousel card."""
    source = Path(video).resolve(strict=True)
    info = source.stat()
    key = hashlib.sha256(("bgpicker-video-thumb-v3-1280x672\0" + str(source) + "\0" + str(info.st_size) + "\0" + str(info.st_mtime_ns)).encode()).hexdigest()
    directory = CACHE / 'video-thumbnails'
    directory.mkdir(parents=True, exist_ok=True)
    target = directory / (key + '.jpg')
    if target.is_file() and target.stat().st_size:
        return str(target)
    temporary = directory / ('.' + key + f'.{os.getpid()}.jpg')
    try:
        result = subprocess.run(['ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
                                 '-i', str(source), '-frames:v', '1', '-vf',
                                 'scale=1280:672:force_original_aspect_ratio=decrease',
                                 '-q:v', '2', str(temporary)],
                                capture_output=True, text=True, timeout=60)
        if result.returncode or not temporary.is_file() or not temporary.stat().st_size:
            raise RuntimeError('video thumbnail failed: ' + result.stderr[-500:])
        os.replace(temporary, target)
        return str(target)
    finally:
        temporary.unlink(missing_ok=True)


def prepare_image_thumbnail(image):
    """Create a reusable card-sized JPG thumbnail for a static wallpaper."""
    source = Path(image).resolve(strict=True)
    info = source.stat()
    key = hashlib.sha256(("bgpicker-image-thumb-v1-1280x672\0" + str(source) + "\0" + str(info.st_size) + "\0" + str(info.st_mtime_ns)).encode()).hexdigest()
    directory = CACHE / 'image-thumbnails'
    directory.mkdir(parents=True, exist_ok=True)
    target = directory / (key + '.jpg')
    if target.is_file() and target.stat().st_size:
        return str(target)
    temporary = directory / ('.' + key + f'.{os.getpid()}.jpg')
    try:
        result = subprocess.run(['magick', '-limit', 'thread', '2', f'{source}[0]',
                                 '-auto-orient', '-thumbnail', '1280x672>',
                                 '-quality', '88', '-strip', str(temporary)],
                                capture_output=True, text=True, timeout=60)
        if result.returncode or not temporary.is_file() or not temporary.stat().st_size:
            raise RuntimeError('image thumbnail failed: ' + result.stderr[-500:])
        os.replace(temporary, target)
        return str(target)
    finally:
        temporary.unlink(missing_ok=True)


def prepare_blur(video):
    """Pre-render a small, blurred video once; avoid a 4K filter every frame."""
    source_path = Path(video).resolve(strict=True)
    info = source_path.stat()
    directory = CACHE / 'video-blur'
    directory.mkdir(parents=True, exist_ok=True)
    meta_key = hashlib.sha256(("bgpicker-video-blur-meta-v2\0" + str(source_path) + "\0" + str(info.st_ino) + "\0" + str(info.st_size) + "\0" + str(info.st_mtime_ns)).encode()).hexdigest()
    meta_file = directory / (meta_key + '.ref')
    if meta_file.is_file():
        cached_target = Path(meta_file.read_text(encoding='utf-8').strip())
        if cached_target.is_file() and cached_target.stat().st_size:
            return str(cached_target)
    digest = hashlib.sha256(b'bgpicker-video-blur-v2-1920-30fps-sigma30-crf18\0')
    with open(video, 'rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    target = directory / (digest.hexdigest() + '.mp4')
    if target.is_file() and target.stat().st_size:
        atomic(meta_file, str(target))
        return str(target)
    temporary = directory / ('.' + digest.hexdigest() + f'.{os.getpid()}.mp4')
    try:
        result = subprocess.run(['ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error', '-y',
                                 '-threads', '2', '-filter_threads', '2', '-i', video,
                                 '-vf', 'fps=30,scale=1920:-2,gblur=sigma=30', '-an',
                                 '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '18',
                                 '-pix_fmt', 'yuv420p', '-threads', '2', str(temporary)],
                                capture_output=True, text=True, timeout=1800)
        if result.returncode or not temporary.is_file() or not temporary.stat().st_size:
            raise RuntimeError('blur video preparation failed: ' + result.stderr[-500:])
        os.replace(temporary, target)
        atomic(meta_file, str(target))
        return str(target)
    finally:
        temporary.unlink(missing_ok=True)


def ensure_native_plugin():
    if (ROOT / 'BgPickerVideo/libbgpickervideo.so').exists():
        return
    result = subprocess.run([str(ROOT / 'build_native.sh')], capture_output=True, text=True, timeout=120)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or 'build_native.sh failed')


def run_daemon(video, mode, token):
    children = []
    directory = RUNTIME / token
    directory.mkdir(parents=True, mode=0o700)
    status = directory / 'status'
    log = open(CACHE / 'video.log', 'a', buffering=1)
    terminating = False
    def terminate(*_):
        nonlocal terminating
        terminating = True
    signal.signal(signal.SIGTERM, terminate)
    signal.signal(signal.SIGINT, terminate)
    try:
        atomic(status, 'preparing\n')
        ensure_native_plugin()
        if terminating: raise RuntimeError('cancelled')
        ready_marker = directory / 'qs-ready'
        env = os.environ.copy()
        qml_paths = [str(ROOT)] + ([env['QML_IMPORT_PATH']] if env.get('QML_IMPORT_PATH') else [])
        env.update({'BGPICKER_VIDEO': video, 'BGPICKER_BLUR': '',
                    'BGPICKER_MODE': mode, 'BGPICKER_QS_READY': str(ready_marker),
                    'QML_IMPORT_PATH': ':'.join(qml_paths)})
        from wallpaper_settings import resolve, read_config
        env['BGPICKER_INITIAL_TRANSITION'] = json.dumps(resolve(read_config()))
        for state_name, env_name in [('current_wallpaper', 'BGPICKER_STATIC_FRONT'),
                                     ('mode', 'BGPICKER_STATIC_MODE'),
                                     ('backdrop_parallax', 'BGPICKER_STATIC_BACKDROP_PARALLAX')]:
            path = STATE / state_name
            if path.exists(): env[env_name] = path.read_text().strip()
        blur_state = Path.home() / '.cache/current_swaybg_img'
        if blur_state.exists(): env['BGPICKER_STATIC_BLUR'] = blur_state.read_text().strip()
        proc = subprocess.Popen(['qs', '-p', str(ROOT / 'video_shell.qml')], env=env,
                                stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        children.append(proc)
        deadline = time.monotonic() + 40
        while not ready_marker.exists():
            if terminating: raise RuntimeError('cancelled')
            if proc.poll() is not None: raise RuntimeError('Quickshell video renderer exited; see video.log')
            if time.monotonic() > deadline: raise RuntimeError('Quickshell video renderer timed out; see video.log')
            time.sleep(.1)
        atomic(status, 'ready\n')
        while not terminating and proc.poll() is None:
            time.sleep(.1)
        if not terminating: raise RuntimeError('Quickshell video renderer exited unexpectedly')
    except Exception as error:
        print(f'BgPicker video: {error}', file=log)
        if not status.exists() or status.read_text().strip() != 'ready': atomic(status, 'error: ' + str(error) + '\n')
    finally:
        for p in children:
            if p.poll() is None: p.terminate()
        for p in children:
            try: p.wait(timeout=3)
            except subprocess.TimeoutExpired: p.kill(); p.wait()
        log.close()


def _qs_call(project_path, *args):
    result = subprocess.run(['qs', 'ipc', '-p', str(project_path),
                             'call', 'videoParallax', *args], capture_output=True, text=True, timeout=5)
    if result.returncode or result.stdout.strip().startswith('Not ready to accept queries'):
        raise RuntimeError(result.stderr.strip() or result.stdout.strip() or 'video renderer IPC failed')
    return result.stdout.strip()


def qs_video(*args):
    return _qs_call(ROOT / 'video_shell.qml', *args)


def _drive_video_apply(ipc_fn, status, video, mode):
    from wallpaper_settings import resolve, read_config
    if status.get('currentPath') == video:
        result = ipc_fn('mode', mode)
        if result.startswith('error:'): raise RuntimeError(result)
    else:
        try:
            result = ipc_fn('stage', video, '', mode)
            if result.startswith('error:'): raise RuntimeError(result)
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                result = ipc_fn('ready', video)
                if result == 'ready': break
                if result.startswith('error:'): raise RuntimeError(result)
                time.sleep(.05)
            else: raise RuntimeError('video first frame timed out')
            result = ipc_fn('commit', video, json.dumps(resolve(read_config())))
            if result.startswith('error:'): raise RuntimeError(result)
        except BaseException:
            try: ipc_fn('cancel', video)
            except (OSError, RuntimeError): pass
            raise
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        current = json.loads(ipc_fn('status'))
        if not current.get('transitionActive'): break
        time.sleep(.05)
    else: raise RuntimeError('video transition timed out')
    atomic(STATE / 'video-wallpaper', video + '\n')
    atomic(STATE / 'video-mode', mode + '\n')


def _drive_static_handoff(ipc_fn, image, mode):
    from wallpaper_settings import resolve, read_config
    blur = prepare_static_blur(image)
    backdrop_path = STATE / 'backdrop_parallax'
    backdrop_parallax = 'on' if backdrop_path.exists() and backdrop_path.read_text().strip() == 'on' else 'off'
    try:
        result = ipc_fn('stageStatic', image, blur, mode, backdrop_parallax)
        if result.startswith('error:'):
            raise RuntimeError(result)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            result = ipc_fn('readyStatic', image)
            if result == 'ready':
                break
            if result.startswith('error:'):
                raise RuntimeError(result)
            time.sleep(.05)
        else:
            raise RuntimeError('static image load timed out')
        result = ipc_fn('commitStatic', image, json.dumps(resolve(read_config())))
        if result.startswith('error:'):
            raise RuntimeError(result)
    except BaseException:
        try:
            ipc_fn('cancelStatic', image)
        except (OSError, RuntimeError):
            pass
        raise
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        status = json.loads(ipc_fn('status'))
        if not status.get('transitionActive'):
            break
        time.sleep(.05)
    else:
        raise RuntimeError('static transition timed out')


def reuse_renderer(video, mode):
    try:
        record = json.loads((STATE / 'video-manager.json').read_text())
        if not live_pid(record['pid'], record['start']): return False
        status = json.loads(qs_video('status'))
        if status.get('apiVersion') != 2: return False
    except (OSError, RuntimeError, ValueError, KeyError):
        return False
    _drive_video_apply(qs_video, status, video, mode)
    return True


def prepare_static_blur(image):
    result = subprocess.run([str(ROOT / 'prepare_wallpaper.sh'), image],
                            capture_output=True, text=True, timeout=120)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or 'prepare_wallpaper.sh failed')
    blur = result.stdout.strip()
    if not blur:
        raise RuntimeError('prepare_wallpaper.sh returned empty path')
    return blur


def transition_to_static(image, mode):
    image_path = Path(image).expanduser().resolve(strict=True)
    if not image_path.is_file():
        raise ValueError('image is not a file')
    image = str(image_path)
    if mode not in ('awww', 'parallax'):
        raise ValueError('mode must be awww or parallax')
    try:
        record = json.loads((STATE / 'video-manager.json').read_text())
        if not live_pid(record['pid'], record['start']):
            return False
        status = json.loads(qs_video('status'))
        if status.get('apiVersion') != 2 or status.get('ready') is not True:
            return False
    except (OSError, RuntimeError, ValueError, KeyError):
        return False
    _drive_static_handoff(qs_video, image, mode)
    return True


def apply(video, mode):
    video = str(Path(video).expanduser().resolve(strict=True))
    if not Path(video).is_file(): raise ValueError('video is not a file')
    if mode not in ('normal', 'parallax'): raise ValueError('mode must be normal or parallax')
    CACHE.mkdir(parents=True, exist_ok=True); STATE.mkdir(parents=True, exist_ok=True)
    RUNTIME.mkdir(parents=True, mode=0o700, exist_ok=True)
    if reuse_renderer(video, mode): return
    token = f'{os.getpid()}-{time.time_ns()}'
    log = open(CACHE / 'video.log', 'a')
    p = subprocess.Popen([sys.executable, __file__, '_run', video, mode, token],
                         stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True, close_fds=True)
    log.close()
    status_file = RUNTIME / token / 'status'
    deadline = time.monotonic() + 1840
    announced = False
    try:
        while time.monotonic() < deadline:
            if status_file.exists():
                result = status_file.read_text().strip()
                if result == 'ready': break
                if result.startswith('error:'): raise RuntimeError(result)
                if result == 'preparing' and not announced:
                    print('BgPicker: starting video renderer...', flush=True)
                    announced = True
            if p.poll() is not None: raise RuntimeError('video manager exited')
            time.sleep(.1)
        else: raise RuntimeError('video manager timed out')
        stop_record(STATE / 'video-manager.json')
        atomic(STATE / 'video-manager.json', json.dumps({'pid': p.pid, 'start': process_start(p.pid), 'token': token}))
        atomic(STATE / 'video-wallpaper', video + '\n')
        atomic(STATE / 'video-mode', mode + '\n')
    except BaseException:
        p.terminate()
        raise


def main():
    if len(sys.argv) >= 2 and sys.argv[1] == '_run': run_daemon(*sys.argv[2:]); return
    if len(sys.argv) == 2 and sys.argv[1] == 'stop': stop_record(STATE / 'video-manager.json'); return
    if len(sys.argv) == 4 and sys.argv[1] == 'apply': apply(sys.argv[2], sys.argv[3]); return
    if len(sys.argv) == 4 and sys.argv[1] == 'transition-to-static':
        if not transition_to_static(sys.argv[2], sys.argv[3]):
            sys.exit(2)
        return
    raise ValueError('usage: video_wallpaper.py apply VIDEO normal|parallax | transition-to-static IMAGE awww|parallax | stop')

if __name__ == '__main__':
    try: main()
    except (OSError, ValueError, RuntimeError, KeyError) as exc:
        print(f'BgPicker video: {exc}', file=sys.stderr)
        sys.exit(1)
