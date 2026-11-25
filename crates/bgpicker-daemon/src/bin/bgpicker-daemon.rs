use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use bgpicker_daemon::daemon::{resolve_project_dir, DaemonInstance, DaemonOptions};

static SIGNAL_EXIT: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: libc::c_int) {
    SIGNAL_EXIT.store(true, Ordering::Release);
}

fn install_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handle_signal as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = 0;
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
    }
}

fn parse_daemon_args() -> Result<(Option<PathBuf>, Option<PathBuf>, bool), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut socket = None;
    let mut project_dir = None;
    let mut restore = true;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" => {
                i += 1;
                let s = args.get(i).ok_or("--socket requires a path")?;
                socket = Some(PathBuf::from(s));
            }
            "--project-dir" => {
                i += 1;
                let s = args.get(i).ok_or("--project-dir requires a path")?;
                project_dir = Some(PathBuf::from(s));
            }
            "--no-restore" => {
                restore = false;
            }
            "-h" | "--help" => {
                return Err(
                    "Usage: bgpicker-daemon [--socket PATH] [--project-dir DIR] [--no-restore]"
                        .into(),
                );
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
        i += 1;
    }
    Ok((socket, project_dir, restore))
}

fn main() {
    install_signal_handlers();

    let (socket, explicit_dir, restore) = match parse_daemon_args() {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(if msg.starts_with("Usage:") { 0 } else { 2 });
        }
    };

    let project_dir = resolve_project_dir(explicit_dir);
    let opts = DaemonOptions::from_env(socket, project_dir, restore);

    let mut instance = match DaemonInstance::start_live(opts) {
        Ok(inst) => inst,
        Err(err) => {
            eprintln!("bgpicker-daemon: {err}");
            std::process::exit(1);
        }
    };

    let notifier = instance.stop_notifier();
    let sig_thread = thread::Builder::new()
        .name("bgpicker-sigwait".into())
        .spawn(move || {
            let (lock, cv) = &*notifier;
            loop {
                if SIGNAL_EXIT.load(Ordering::Acquire) {
                    let mut guard = lock.lock().expect("stop signal lock poisoned");
                    *guard = true;
                    cv.notify_all();
                    break;
                }
                {
                    let guard = lock.lock().expect("stop signal lock poisoned");
                    if *guard {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(50));
            }
        })
        .ok();

    instance.wait_for_stop();
    instance.shutdown();

    if let Some(t) = sig_thread {
        let _ = t.join();
    }
}
