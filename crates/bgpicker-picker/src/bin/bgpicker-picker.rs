//! Native pure-Rust Wayland layer-shell wallpaper picker overlay (`bgpicker-picker`).

use std::io::Write;
use std::process::ExitCode;

use bgpicker_core::BgPickerPaths;
use bgpicker_picker::ipc::{
    CliParseError, PickerIpcListener, SingleInstanceOutcome, parse_cli_args,
};
use bgpicker_picker::wayland::run_overlay;
use log::{Level, LevelFilter, Log, Metadata, Record};

struct StderrLogger;

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ = writeln!(
                std::io::stderr(),
                "[bgpicker-picker {}] {}",
                record.level(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

fn init_logger() {
    let level = match std::env::var("BGPICKER_LOG").ok().as_deref() {
        Some("debug" | "DEBUG") => LevelFilter::Debug,
        Some("warn" | "WARN") => LevelFilter::Warn,
        Some("error" | "ERROR") => LevelFilter::Error,
        _ => LevelFilter::Info,
    };
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);
}

fn main() -> ExitCode {
    init_logger();

    let paths = BgPickerPaths::from_env();
    let opts = match parse_cli_args(std::env::args().skip(1), &paths) {
        Ok(opts) => opts,
        Err(CliParseError::Help(text)) => {
            println!("{text}");
            return ExitCode::SUCCESS;
        }
        Err(CliParseError::Invalid(msg)) => {
            eprintln!("bgpicker-picker: {msg}");
            return ExitCode::from(2);
        }
    };

    let listener = match PickerIpcListener::acquire_or_forward(
        opts.picker_socket.clone(),
        opts.action.to_command(),
    ) {
        Ok(SingleInstanceOutcome::Forwarded(resp)) => {
            if resp.ok {
                return ExitCode::SUCCESS;
            }
            eprintln!(
                "bgpicker-picker: forwarded command failed: {}",
                resp.error.unwrap_or_else(|| "unknown error".into())
            );
            return ExitCode::FAILURE;
        }
        Ok(SingleInstanceOutcome::AlreadyClosed) => {
            return ExitCode::SUCCESS;
        }
        Ok(SingleInstanceOutcome::Acquired(listener)) => listener,
        Err(err) => {
            eprintln!("bgpicker-picker: failed to acquire picker socket: {err}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(err) = run_overlay(opts, listener) {
        eprintln!("bgpicker-picker: {err}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
