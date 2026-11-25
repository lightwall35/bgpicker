use std::fs;
use std::io::{BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bgpicker_core::protocol::{read_request, write_response, Request, Response};

use crate::actor::DaemonActorHandle;

/// Unix domain socket IPC server accepting newline-delimited JSON [`Request`]s.
pub struct IpcServer {
    socket_path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl IpcServer {
    pub fn bind(
        socket_path: PathBuf,
        actor: DaemonActorHandle,
        on_stop: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Self> {
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent)?;
        }
        remove_stale_socket(&socket_path)?;

        let listener = UnixListener::bind(&socket_path)?;
        listener.set_nonblocking(true)?;

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("bgpicker-ipc".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let conn_actor = actor.clone();
                            let conn_stop = Arc::clone(&worker_stop);
                            let conn_on_stop = Arc::clone(&on_stop);
                            let _ = thread::Builder::new()
                                .name("bgpicker-ipc-conn".into())
                                .spawn(move || {
                                    handle_connection(stream, conn_actor, conn_stop, conn_on_stop);
                                });
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => break,
                    }
                }
            })?;

        Ok(Self {
            socket_path,
            stop,
            thread: Some(thread),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn remove_stale_socket(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("bgpicker-daemon socket already active at {}", path.display()),
        )),
        Err(_) => {
            let _ = fs::remove_file(path);
            Ok(())
        }
    }
}

fn handle_connection(
    stream: UnixStream,
    actor: DaemonActorHandle,
    server_stop: Arc<AtomicBool>,
    on_stop: Arc<dyn Fn() + Send + Sync>,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let Ok(write_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut writer = BufWriter::new(write_stream);

    loop {
        match read_request(&mut reader) {
            Ok(Some(req)) => {
                let is_stop = matches!(req, Request::Stop);
                let resp = actor.dispatch(req);
                if write_response(&mut writer, &resp).is_err() {
                    break;
                }
                if is_stop {
                    server_stop.store(true, Ordering::Release);
                    on_stop();
                    break;
                }
            }
            Ok(None) => break,
            Err(err) => {
                let _ = write_response(&mut writer, &Response::err(err.to_string()));
                break;
            }
        }
    }
}
