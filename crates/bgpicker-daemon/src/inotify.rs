use std::ffi::{CStr, CString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

/// Watches `<config_dir>` via Linux `inotify` for atomic updates to `<file_name>`
/// (such as `settings.json` replaced by `rename` or edited in place) and invokes `on_change`.
pub struct SettingsWatcher {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SettingsWatcher {
    pub fn spawn<F>(settings_path: &Path, mut on_change: F) -> std::io::Result<Self>
    where
        F: FnMut() + Send + 'static,
    {
        let parent = settings_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;

        let target_name = settings_path
            .file_name()
            .map(|n| n.as_bytes().to_vec())
            .unwrap_or_else(|| b"settings.json".to_vec());

        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let c_dir = CString::new(parent.as_os_str().as_bytes()).map_err(|_| {
            unsafe { libc::close(fd) };
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "interior nul in path")
        })?;

        let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_DELETE;
        let wd = unsafe { libc::inotify_add_watch(fd, c_dir.as_ptr(), mask) };
        if wd < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(err);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("bgpicker-settings-watch".into())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                while !worker_stop.load(Ordering::Acquire) {
                    let mut pfd = libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    let poll_rc = unsafe { libc::poll(&mut pfd, 1, 200) };
                    if poll_rc < 0 {
                        let err = std::io::Error::last_os_error();
                        if err.kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                    if poll_rc == 0 || (pfd.revents & libc::POLLIN) == 0 {
                        continue;
                    }

                    let mut triggered = false;
                    loop {
                        let n = unsafe {
                            libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                        };
                        if n <= 0 {
                            break;
                        }
                        let mut offset = 0usize;
                        let n = n as usize;
                        while offset + std::mem::size_of::<libc::inotify_event>() <= n {
                            let ev = unsafe {
                                &*(buf.as_ptr().add(offset) as *const libc::inotify_event)
                            };
                            let name_len = ev.len as usize;
                            let step = std::mem::size_of::<libc::inotify_event>() + name_len;
                            if name_len > 0 && offset + step <= n {
                                let name_ptr = unsafe {
                                    buf.as_ptr()
                                        .add(offset + std::mem::size_of::<libc::inotify_event>())
                                        as *const libc::c_char
                                };
                                let c_name = unsafe { CStr::from_ptr(name_ptr) };
                                if c_name.to_bytes() == target_name.as_slice() {
                                    triggered = true;
                                }
                            }
                            offset += step;
                        }
                    }

                    if triggered && !worker_stop.load(Ordering::Acquire) {
                        on_change();
                    }
                }
                unsafe {
                    libc::inotify_rm_watch(fd, wd);
                    libc::close(fd);
                }
            })?;

        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for SettingsWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}
