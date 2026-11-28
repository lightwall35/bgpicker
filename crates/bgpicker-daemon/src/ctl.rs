use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use bgpicker_core::{
    is_placeholder_pid_alive, is_splash_lock_held, list_videos_sorted, prepare_startup_placeholder,
    record_placeholder_pid, resolve_startup_placeholders, retire_startup_placeholders,
    write_splash_bin_from_packed,
};
use bgpicker_core::config::TransitionType;
use bgpicker_core::paths::{atomic_write_text, BgPickerPaths, EngineKind, PersistedState};
use bgpicker_core::protocol::{
    BackdropAction, BackdropOverride, ClearCacheReport, DaemonStatus, EngineAction, IpcClient,
    ModeAction, PickerCommand, PickerIpcClient, ProtocolError, Request, ResponseData,
    VideoModeAction,
};
use bgpicker_core::splash::write_startup_trace;
use bgpicker_gfx::{FrameInput, PixelFormat};

use crate::daemon::resolve_project_dir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtlGlobalOpts {
    pub socket: Option<PathBuf>,
    pub project_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    Toggle,
    Open,
    Close,
    Next,
    Prev,
}

impl PickerAction {
    fn qs_ipc_method(&self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Open => "open",
            Self::Close => "close",
            Self::Next => "cycleNext",
            Self::Prev => "cyclePrevious",
        }
    }

    fn picker_command(&self) -> PickerCommand {
        match self {
            Self::Toggle => PickerCommand::Toggle,
            Self::Open => PickerCommand::Open,
            Self::Close => PickerCommand::Close,
            Self::Next => PickerCommand::CycleNext,
            Self::Prev => PickerCommand::CyclePrevious,
        }
    }

    fn spawn_action_arg(&self) -> Option<&'static str> {
        match self {
            Self::Toggle | Self::Open => Some("open"),
            Self::Next => Some("cycle-next"),
            Self::Prev => Some("cycle-prev"),
            Self::Close => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtlCommand {
    Start,
    Startup,
    Stop,
    Status {
        json: bool,
        no_start: bool,
    },
    ApplyStatic {
        path: PathBuf,
        mode: Option<bgpicker_core::paths::WallpaperMode>,
        backdrop: BackdropOverride,
    },
    Restore,
    Mode {
        action: Option<ModeAction>,
        backdrop: BackdropOverride,
    },
    BackdropParallax {
        action: Option<BackdropAction>,
    },
    StartupPlaceholder {
        action: Option<BackdropAction>,
    },
    PlaceholderCache {
        action: Option<BackdropAction>,
    },
    ApplyVideo {
        mode: Option<VideoModeAction>,
        path: Option<PathBuf>,
        backdrop: BackdropOverride,
    },
    Video {
        sub: VideoSubcommand,
        backdrop: BackdropOverride,
    },
    Engine {
        action: Option<EngineAction>,
    },
    Transition {
        transition_type: Option<TransitionType>,
    },
    ReloadSettings,
    Catalog {
        images: Option<PathBuf>,
        videos: Option<PathBuf>,
    },
    Picker {
        action: PickerAction,
    },
    ClearCache {
        deep: bool,
        picker: bool,
        disk: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoSubcommand {
    Status,
    Normal { path: Option<PathBuf> },
    Parallax { path: Option<PathBuf> },
    Next,
}

/// Parses `bgpickerctl` CLI arguments into global options + typed command.
pub fn parse_args<I, S>(args: I) -> Result<(CtlGlobalOpts, CtlCommand), String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let raw: Vec<String> = args.into_iter().map(Into::into).collect();
    let mut idx = 0usize;
    let mut global = CtlGlobalOpts {
        socket: None,
        project_dir: None,
    };

    while idx < raw.len() {
        match raw[idx].as_str() {
            "--socket" => {
                idx += 1;
                let val = raw
                    .get(idx)
                    .ok_or_else(|| "--socket requires a path".to_string())?;
                global.socket = Some(PathBuf::from(val));
                idx += 1;
            }
            "--project-dir" => {
                idx += 1;
                let val = raw
                    .get(idx)
                    .ok_or_else(|| "--project-dir requires a path".to_string())?;
                global.project_dir = Some(PathBuf::from(val));
                idx += 1;
            }
            "-h" | "--help" | "help" => {
                return Err(usage_text());
            }
            _ => break,
        }
    }

    let Some(sub) = raw.get(idx).map(String::as_str) else {
        return Err(usage_text());
    };
    let rest = &raw[idx + 1..];

    let cmd = match sub {
        "start" => {
            ensure_no_extra(sub, rest)?;
            CtlCommand::Start
        }
        "startup" => {
            ensure_no_extra(sub, rest)?;
            CtlCommand::Startup
        }
        "stop" => {
            ensure_no_extra(sub, rest)?;
            CtlCommand::Stop
        }
        "status" => {
            let mut json = false;
            let mut no_start = false;
            for arg in rest {
                match arg.as_str() {
                    "--json" => json = true,
                    "--no-start" => no_start = true,
                    other => return Err(format!("unknown status flag: {other}")),
                }
            }
            CtlCommand::Status { json, no_start }
        }
        "apply-static" => parse_apply_static(rest)?,
        "restore" => {
            ensure_no_extra(sub, rest)?;
            CtlCommand::Restore
        }
        "mode" => parse_mode_cmd(rest)?,
        "backdrop-parallax" => parse_on_off_toggle(rest, "backdrop-parallax")
            .map(|action| CtlCommand::BackdropParallax { action })?,
        "startup-placeholder" | "placeholder" => parse_on_off_toggle(rest, "startup-placeholder")
            .map(|action| CtlCommand::StartupPlaceholder { action })?,
        "placeholder-cache" => parse_on_off_toggle(rest, "placeholder-cache")
            .map(|action| CtlCommand::PlaceholderCache { action })?,
        "apply-video" => parse_apply_video(rest)?,
        "video" => parse_video_cmd(rest)?,
        "engine" => match rest {
            [] => CtlCommand::Engine { action: None },
            [arg] => match arg.as_str() {
                "status" => CtlCommand::Engine { action: None },
                "full" => CtlCommand::Engine {
                    action: Some(EngineAction::Full),
                },
                "simple" => CtlCommand::Engine {
                    action: Some(EngineAction::Simple),
                },
                "video" => CtlCommand::Engine {
                    action: Some(EngineAction::Video),
                },
                "toggle" => CtlCommand::Engine {
                    action: Some(EngineAction::Toggle),
                },
                other => return Err(format!("invalid engine '{other}' (expected status|full|simple|video|toggle)")),
            },
            _ => return Err("usage: bgpickerctl engine [status|full|simple|video|toggle]".into()),
        },
        "transition" => match rest {
            [] => CtlCommand::Transition {
                transition_type: None,
            },
            [arg] => {
                let kind = TransitionType::from_str(arg).map_err(|e| e.to_string())?;
                CtlCommand::Transition {
                    transition_type: Some(kind),
                }
            }
            _ => return Err("usage: bgpickerctl transition [fade|wipe|wave|grow|outer|random|none]".into()),
        },
        "reload-settings" => {
            ensure_no_extra(sub, rest)?;
            CtlCommand::ReloadSettings
        }
        "catalog" => parse_catalog(rest)?,
        "picker" => match rest {
            [arg] => {
                let action = match arg.as_str() {
                    "toggle" => PickerAction::Toggle,
                    "open" => PickerAction::Open,
                    "close" => PickerAction::Close,
                    "next" => PickerAction::Next,
                    "prev" => PickerAction::Prev,
                    other => return Err(format!("invalid picker action '{other}' (expected toggle|open|close|next|prev)")),
                };
                CtlCommand::Picker { action }
            }
            _ => return Err("usage: bgpickerctl picker {toggle|open|close|next|prev}".into()),
        },
        "clear-cache" | "trim" => parse_clear_cache(rest)?,
        other => return Err(format!("unknown subcommand '{other}'\n\n{}", usage_text())),
    };

    Ok((global, cmd))
}

fn parse_on_off_toggle(rest: &[String], name: &str) -> Result<Option<BackdropAction>, String> {
    match rest {
        [] => Ok(None),
        [arg] => {
            let action = match arg.as_str() {
                "on" => BackdropAction::On,
                "off" => BackdropAction::Off,
                "toggle" => BackdropAction::Toggle,
                other => {
                    return Err(format!(
                        "invalid {name} action '{other}' (expected on|off|toggle)"
                    ))
                }
            };
            Ok(Some(action))
        }
        _ => Err(format!("usage: bgpickerctl {name} [on|off|toggle]")),
    }
}

fn ensure_no_extra(sub: &str, rest: &[String]) -> Result<(), String> {
    if rest.is_empty() {
        Ok(())
    } else {
        Err(format!("unexpected arguments for '{sub}': {}", rest.join(" ")))
    }
}

fn parse_cli_backdrop_value(val: &str) -> Result<BackdropOverride, String> {
    match val.trim() {
        "on" => Ok(BackdropOverride::On),
        "off" => Ok(BackdropOverride::Off),
        "keep" => Ok(BackdropOverride::Keep),
        "auto" => Ok(BackdropOverride::Auto),
        other => Err(format!(
            "invalid --backdrop value '{other}' (expected on|off|keep)"
        )),
    }
}

fn parse_backdrop_flag(
    rest: &[String],
    idx: &mut usize,
    backdrop: &mut BackdropOverride,
    backdrop_set: &mut bool,
) -> Result<bool, String> {
    match rest[*idx].as_str() {
        "--keep-backdrop" => {
            if *backdrop_set {
                return Err("conflicting or duplicate backdrop override flags".into());
            }
            *backdrop = BackdropOverride::Keep;
            *backdrop_set = true;
            Ok(true)
        }
        "--backdrop" => {
            if *backdrop_set {
                return Err("conflicting or duplicate backdrop override flags".into());
            }
            *idx += 1;
            let val = rest
                .get(*idx)
                .ok_or_else(|| "--backdrop requires on, off, or keep".to_string())?;
            *backdrop = parse_cli_backdrop_value(val)?;
            *backdrop_set = true;
            Ok(true)
        }
        arg if arg.starts_with("--backdrop=") => {
            if *backdrop_set {
                return Err("conflicting or duplicate backdrop override flags".into());
            }
            let val = &arg["--backdrop=".len()..];
            *backdrop = parse_cli_backdrop_value(val)?;
            *backdrop_set = true;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn parse_mode_cmd(rest: &[String]) -> Result<CtlCommand, String> {
    let mut positionals: Vec<&str> = Vec::new();
    let mut backdrop = BackdropOverride::Auto;
    let mut backdrop_set = false;
    let mut i = 0;
    while i < rest.len() {
        if parse_backdrop_flag(rest, &mut i, &mut backdrop, &mut backdrop_set)? {
            i += 1;
            continue;
        }
        if rest[i].starts_with("--") {
            return Err(format!("unknown flag for mode: {}", rest[i]));
        }
        positionals.push(rest[i].as_str());
        i += 1;
    }

    match positionals.as_slice() {
        [] => {
            if backdrop_set {
                return Err(
                    "--keep-backdrop/--backdrop requires a mode action (awww|parallax|toggle)"
                        .into(),
                );
            }
            Ok(CtlCommand::Mode {
                action: None,
                backdrop,
            })
        }
        [arg] => {
            let action = match *arg {
                "awww" => ModeAction::Awww,
                "parallax" => ModeAction::Parallax,
                "toggle" => ModeAction::Toggle,
                other => {
                    return Err(format!(
                        "invalid mode '{other}' (expected awww|parallax|toggle)"
                    ))
                }
            };
            Ok(CtlCommand::Mode {
                action: Some(action),
                backdrop,
            })
        }
        _ => Err(
            "usage: bgpickerctl mode [awww|parallax|toggle] [--keep-backdrop | --backdrop <on|off|keep>]"
                .into(),
        ),
    }
}

fn parse_apply_static(rest: &[String]) -> Result<CtlCommand, String> {
    let mut path: Option<PathBuf> = None;
    let mut mode = None;
    let mut backdrop = BackdropOverride::Auto;
    let mut backdrop_set = false;
    let mut i = 0;
    while i < rest.len() {
        if parse_backdrop_flag(rest, &mut i, &mut backdrop, &mut backdrop_set)? {
            i += 1;
            continue;
        }
        match rest[i].as_str() {
            "--mode" => {
                i += 1;
                let m = rest
                    .get(i)
                    .ok_or_else(|| "--mode requires awww or parallax".to_string())?;
                mode = Some(
                    bgpicker_core::paths::WallpaperMode::from_str(m).map_err(|e| e.to_string())?,
                );
            }
            arg if arg.starts_with("--") => {
                return Err(format!("unknown flag for apply-static: {arg}"));
            }
            arg => {
                if path.is_some() {
                    return Err(
                        "usage: bgpickerctl apply-static <path> [--mode awww|parallax] [--keep-backdrop | --backdrop <on|off|keep>]"
                            .into(),
                    );
                }
                path = Some(PathBuf::from(arg));
            }
        }
        i += 1;
    }
    let path = path.ok_or_else(|| {
        "usage: bgpickerctl apply-static <path> [--mode awww|parallax] [--keep-backdrop | --backdrop <on|off|keep>]"
            .to_string()
    })?;
    Ok(CtlCommand::ApplyStatic {
        path,
        mode,
        backdrop,
    })
}

fn parse_apply_video(rest: &[String]) -> Result<CtlCommand, String> {
    let mut positionals: Vec<&str> = Vec::new();
    let mut backdrop = BackdropOverride::Auto;
    let mut backdrop_set = false;
    let mut i = 0;
    while i < rest.len() {
        if parse_backdrop_flag(rest, &mut i, &mut backdrop, &mut backdrop_set)? {
            i += 1;
            continue;
        }
        if rest[i].starts_with("--") {
            return Err(format!("unknown flag for apply-video: {}", rest[i]));
        }
        positionals.push(rest[i].as_str());
        i += 1;
    }

    match positionals.as_slice() {
        [] => Ok(CtlCommand::ApplyVideo {
            mode: None,
            path: None,
            backdrop,
        }),
        [a] => match *a {
            "normal" => Ok(CtlCommand::ApplyVideo {
                mode: Some(VideoModeAction::Normal),
                path: None,
                backdrop,
            }),
            "parallax" => Ok(CtlCommand::ApplyVideo {
                mode: Some(VideoModeAction::Parallax),
                path: None,
                backdrop,
            }),
            "toggle" => Ok(CtlCommand::ApplyVideo {
                mode: Some(VideoModeAction::Toggle),
                path: None,
                backdrop,
            }),
            _ => Ok(CtlCommand::ApplyVideo {
                mode: None,
                path: Some(PathBuf::from(a)),
                backdrop,
            }),
        },
        [m, p] => {
            let mode = match *m {
                "normal" => VideoModeAction::Normal,
                "parallax" => VideoModeAction::Parallax,
                "toggle" => VideoModeAction::Toggle,
                other => {
                    return Err(format!(
                        "invalid video mode '{other}' (expected normal|parallax|toggle)"
                    ))
                }
            };
            Ok(CtlCommand::ApplyVideo {
                mode: Some(mode),
                path: Some(PathBuf::from(p)),
                backdrop,
            })
        }
        _ => Err(
            "usage: bgpickerctl apply-video [normal|parallax|toggle] [path] [--keep-backdrop | --backdrop <on|off|keep>]"
                .into(),
        ),
    }
}

fn parse_video_cmd(rest: &[String]) -> Result<CtlCommand, String> {
    let mut positionals: Vec<&str> = Vec::new();
    let mut backdrop = BackdropOverride::Auto;
    let mut backdrop_set = false;
    let mut i = 0;
    while i < rest.len() {
        if parse_backdrop_flag(rest, &mut i, &mut backdrop, &mut backdrop_set)? {
            i += 1;
            continue;
        }
        if rest[i].starts_with("--") {
            return Err(format!("unknown flag for video: {}", rest[i]));
        }
        positionals.push(rest[i].as_str());
        i += 1;
    }

    match positionals.as_slice() {
        [] => {
            if backdrop_set {
                return Err("--keep-backdrop/--backdrop cannot be used with 'video status'".into());
            }
            Ok(CtlCommand::Video {
                sub: VideoSubcommand::Status,
                backdrop,
            })
        }
        ["status"] => {
            if backdrop_set {
                return Err("--keep-backdrop/--backdrop cannot be used with 'video status'".into());
            }
            Ok(CtlCommand::Video {
                sub: VideoSubcommand::Status,
                backdrop,
            })
        }
        ["next"] => Ok(CtlCommand::Video {
            sub: VideoSubcommand::Next,
            backdrop,
        }),
        ["normal"] => Ok(CtlCommand::Video {
            sub: VideoSubcommand::Normal { path: None },
            backdrop,
        }),
        ["normal", p] => Ok(CtlCommand::Video {
            sub: VideoSubcommand::Normal {
                path: Some(PathBuf::from(p)),
            },
            backdrop,
        }),
        ["parallax"] => Ok(CtlCommand::Video {
            sub: VideoSubcommand::Parallax { path: None },
            backdrop,
        }),
        ["parallax", p] => Ok(CtlCommand::Video {
            sub: VideoSubcommand::Parallax {
                path: Some(PathBuf::from(p)),
            },
            backdrop,
        }),
        _ => Err(
            "usage: bgpickerctl video [status|normal|parallax [path]|next] [--keep-backdrop | --backdrop <on|off|keep>]"
                .into(),
        ),
    }
}

fn parse_catalog(rest: &[String]) -> Result<CtlCommand, String> {
    let mut images = None;
    let mut videos = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--images" => {
                i += 1;
                let dir = rest
                    .get(i)
                    .ok_or_else(|| "--images requires a directory path".to_string())?;
                images = Some(PathBuf::from(dir));
            }
            "--videos" => {
                i += 1;
                let dir = rest
                    .get(i)
                    .ok_or_else(|| "--videos requires a directory path".to_string())?;
                videos = Some(PathBuf::from(dir));
            }
            other => return Err(format!("unknown catalog flag: {other}")),
        }
        i += 1;
    }
    Ok(CtlCommand::Catalog { images, videos })
}

fn parse_clear_cache(rest: &[String]) -> Result<CtlCommand, String> {
    let mut deep = false;
    let mut picker = false;
    let mut disk = false;
    for arg in rest {
        match arg.as_str() {
            "--deep" | "--restart" => deep = true,
            "--picker" => picker = true,
            "--disk" => disk = true,
            "--all" => {
                deep = true;
                picker = true;
                disk = true;
            }
            other => {
                return Err(format!(
                    "unknown clear-cache flag '{other}' (expected --deep|--picker|--disk|--all)"
                ))
            }
        }
    }
    Ok(CtlCommand::ClearCache { deep, picker, disk })
}

pub fn usage_text() -> String {
    "\
Usage: bgpickerctl [--socket PATH] [--project-dir DIR] <command>

Commands:
  start | startup | stop
  status [--json] [--no-start]
  apply-static <path> [--mode awww|parallax] [--keep-backdrop | --backdrop <on|off|keep>]
  restore
  mode [awww|parallax|toggle] [--keep-backdrop | --backdrop <on|off|keep>]
  backdrop-parallax [on|off|toggle]
  startup-placeholder [on|off|toggle]
  placeholder-cache [on|off|toggle]
  apply-video [normal|parallax|toggle] [path] [--keep-backdrop | --backdrop <on|off|keep>]
  video [status|normal|parallax [path]|next] [--keep-backdrop | --backdrop <on|off|keep>]
  engine [status|full|simple|video|toggle]
  transition [fade|wipe|wave|grow|outer|random|none]
  reload-settings
  catalog [--images <dir>] [--videos <dir>]
  picker {toggle|open|close|next|prev}
  clear-cache [--deep] [--picker] [--disk] [--all]"
        .to_string()
}

/// Executes a parsed `bgpickerctl` command against `paths` and returns stdout output (if any).
pub fn execute(global: CtlGlobalOpts, cmd: CtlCommand) -> Result<Option<String>, String> {
    let mut paths = BgPickerPaths::from_env();
    if let Some(sock) = global.socket.clone() {
        paths.socket_override = Some(sock);
    }
    let project_dir = resolve_project_dir(global.project_dir.clone());
    let socket_path = paths.daemon_socket_path();
    let client = IpcClient::new(&socket_path).with_timeout(Duration::from_secs(30));

    match cmd {
        CtlCommand::Start => {
            if is_daemon_live(&client) {
                return Ok(Some("bgpicker daemon is already running.".into()));
            }
            ensure_daemon_live(&client, &paths, &project_dir, false)?;
            Ok(Some("bgpicker daemon started.".into()))
        }
        CtlCommand::Startup => {
            let persisted = PersistedState::load(&paths);
            if persisted.engine == EngineKind::Simple {
                cleanup_stale_startup_placeholders(&paths);
                close_picker_best_effort(&paths);
                stop_quickshell_best_effort(&project_dir);
                if let Some(wp) = persisted.current_wallpaper.filter(|p| p.is_file()) {
                    let canonical = fs::canonicalize(&wp).unwrap_or(wp);
                    run_simple_wallpaper(&project_dir, &canonical)?;
                }
                stop_daemon_if_live(&client)?;
                return Ok(None);
            }
            if !persisted.startup_placeholder {
                cleanup_stale_startup_placeholders(&paths);
                if is_daemon_live(&client) {
                    call_checked(&client, &Request::Startup)?;
                } else {
                    ensure_daemon_live(&client, &paths, &project_dir, true)?;
                    call_checked(&client, &Request::Startup)?;
                }
                return Ok(None);
            }
            if is_daemon_live(&client) {
                cleanup_stale_startup_placeholders(&paths);
                call_checked(&client, &Request::Startup)?;
                return Ok(None);
            }

            let splash_bin = resolve_splash_binary();
            let extra_bins = placeholder_extra_bins(&paths, splash_bin.as_deref());
            let extra_refs: Vec<&Path> = extra_bins.iter().map(|p| p.as_path()).collect();
            let already_live = is_splash_lock_held(&paths)
                || is_placeholder_pid_alive(
                    &paths.startup_placeholder_front_pid_file(),
                    &extra_refs,
                );
            if !already_live {
                retire_startup_placeholders(&paths, &extra_refs);
            }

            let external_placeholder_override =
                env::var_os("BGPICKER_SPLASH").is_some_and(|s| !s.is_empty())
                    || env::var_os("BGPICKER_SWAYBG").is_some_and(|s| !s.is_empty());
            let (fg_placeholder, bg_placeholder) =
                resolve_startup_placeholders(&paths, &project_dir, &persisted);
            let mut placeholder = if already_live || !external_placeholder_override {
                None
            } else {
                Some(StartupPlaceholderGuard::spawn(
                    &paths,
                    &persisted,
                    fg_placeholder.as_deref(),
                    bg_placeholder.as_deref(),
                ))
            };

            ensure_daemon_live(&client, &paths, &project_dir, true)?;
            let seed_resp = call_checked(
                &client,
                &Request::StartupSeed {
                    placeholder: fg_placeholder,
                },
            )?;
            let seeded = matches!(
                seed_resp.data,
                Some(ResponseData::Message(ref s)) if s == "seeded" || s == "already_restored"
            );

            let uses_swaybg = placeholder.as_ref().is_some_and(|g| g.used_swaybg);
            let settle_delay = startup_placeholder_settle_delay(uses_swaybg);
            if seeded {
                if let Some(ref mut guard) = placeholder {
                    if guard.is_active() && !settle_delay.is_zero() {
                        thread::sleep(settle_delay);
                    }
                    guard.retire();
                }
            }

            call_checked(&client, &Request::StartupSettle)?;

            if let Some(ref mut guard) = placeholder {
                if guard.is_active() && !settle_delay.is_zero() {
                    thread::sleep(settle_delay);
                }
                guard.retire();
            }
            retire_startup_placeholders(&paths, &extra_refs);
            Ok(None)
        }
        CtlCommand::Stop => {
            close_picker_best_effort(&paths);
            stop_quickshell_best_effort(&project_dir);
            cleanup_stale_startup_placeholders(&paths);
            stop_simple_wallpaper_best_effort(&project_dir);
            if stop_daemon_if_live(&client)? {
                Ok(Some("bgpicker daemon stopped.".into()))
            } else {
                Ok(Some("bgpicker daemon is not running.".into()))
            }
        }
        CtlCommand::Status { json, no_start } => {
            if no_start {
                if !is_daemon_live(&client) {
                    return Err("bgpicker-daemon is not running".into());
                }
            } else {
                ensure_daemon_live(&client, &paths, &project_dir, false)?;
            }
            let status = fetch_status(&client)?;
            if json {
                let s = serde_json::to_string_pretty(&status).map_err(|e| e.to_string())?;
                Ok(Some(s))
            } else {
                Ok(Some(format_human_status(&status)))
            }
        }
        CtlCommand::ApplyStatic {
            path,
            mode,
            backdrop,
        } => {
            let was_simple = current_engine_state(&client, &paths)? == EngineKind::Simple;
            ensure_daemon_live(&client, &paths, &project_dir, true)?;
            let abs = absolutize(&path)?;
            call_checked(
                &client,
                &Request::ApplyStatic {
                    path: abs,
                    mode,
                    instant: false,
                    backdrop,
                },
            )?;
            retire_simple_layers_best_effort(was_simple, &paths, &project_dir);
            Ok(None)
        }
        CtlCommand::Restore => {
            ensure_daemon_live(&client, &paths, &project_dir, true)?;
            call_checked(&client, &Request::Restore)?;
            Ok(None)
        }
        CtlCommand::Mode { action, backdrop } => {
            ensure_daemon_live(&client, &paths, &project_dir, false)?;
            match action {
                None => {
                    let st = fetch_status(&client)?;
                    Ok(Some(st.mode.as_str().to_string()))
                }
                Some(action) => {
                    call_checked(
                        &client,
                        &Request::SetMode {
                            mode: action,
                            backdrop,
                        },
                    )?;
                    Ok(None)
                }
            }
        }
        CtlCommand::BackdropParallax { action } => {
            ensure_daemon_live(&client, &paths, &project_dir, false)?;
            match action {
                None => {
                    let st = fetch_status(&client)?;
                    Ok(Some(if st.backdrop_parallax { "on" } else { "off" }.into()))
                }
                Some(action) => {
                    call_checked(&client, &Request::BackdropParallax { action })?;
                    Ok(None)
                }
            }
        }
        CtlCommand::StartupPlaceholder { action } => {
            execute_on_off_option(
                &client,
                action,
                &paths.startup_placeholder_file(),
                PersistedState::load(&paths).startup_placeholder,
                |st| st.startup_placeholder,
                |a| Request::StartupPlaceholder { action: a },
            )
        }
        CtlCommand::PlaceholderCache { action } => {
            let res = execute_on_off_option(
                &client,
                action,
                &paths.placeholder_cache_file(),
                PersistedState::load(&paths).placeholder_cache,
                |st| st.placeholder_cache,
                |a| Request::PlaceholderCache { action: a },
            )?;
            let persisted = PersistedState::load(&paths);
            if action.is_some() && persisted.placeholder_cache {
                warm_placeholder_caches_best_effort(&paths, &project_dir, &persisted);
            }
            Ok(res)
        }
        CtlCommand::ApplyVideo {
            mode,
            path,
            backdrop,
        } => {
            let was_simple = current_engine_state(&client, &paths)? == EngineKind::Simple;
            ensure_daemon_live(&client, &paths, &project_dir, true)?;
            let abs = match path {
                Some(p) => Some(absolutize(&p)?),
                None => None,
            };
            call_checked(
                &client,
                &Request::ApplyVideo {
                    mode,
                    path: abs,
                    backdrop,
                },
            )?;
            retire_simple_layers_best_effort(was_simple, &paths, &project_dir);
            Ok(None)
        }
        CtlCommand::Video { sub, backdrop } => {
            let no_restore = !matches!(sub, VideoSubcommand::Status);
            let was_simple = if no_restore {
                current_engine_state(&client, &paths)? == EngineKind::Simple
            } else {
                false
            };
            ensure_daemon_live(&client, &paths, &project_dir, no_restore)?;
            match sub {
                VideoSubcommand::Status => {
                    let st = fetch_status(&client)?;
                    let vid = st
                        .current_video
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    Ok(Some(format!(
                        "engine: {}\nmode: {}\nvideo: {}",
                        st.engine.as_str(),
                        st.video_mode.as_str(),
                        vid
                    )))
                }
                VideoSubcommand::Normal { path } => {
                    let abs = match path {
                        Some(p) => Some(absolutize(&p)?),
                        None => None,
                    };
                    call_checked(
                        &client,
                        &Request::ApplyVideo {
                            mode: Some(VideoModeAction::Normal),
                            path: abs,
                            backdrop,
                        },
                    )?;
                    retire_simple_layers_best_effort(was_simple, &paths, &project_dir);
                    Ok(None)
                }
                VideoSubcommand::Parallax { path } => {
                    let abs = match path {
                        Some(p) => Some(absolutize(&p)?),
                        None => None,
                    };
                    call_checked(
                        &client,
                        &Request::ApplyVideo {
                            mode: Some(VideoModeAction::Parallax),
                            path: abs,
                            backdrop,
                        },
                    )?;
                    retire_simple_layers_best_effort(was_simple, &paths, &project_dir);
                    Ok(None)
                }
                VideoSubcommand::Next => {
                    let files = list_videos_sorted(&project_dir.join("dynamic"));
                    if files.is_empty() {
                        return Err("BgPicker: no videos found in dynamic/".into());
                    }
                    let st = fetch_status(&client)?;
                    let current = st.current_video.and_then(|p| fs::canonicalize(&p).ok().or(Some(p)));
                    let mut next_idx = 0usize;
                    if let Some(cur) = current {
                        for (i, candidate) in files.iter().enumerate() {
                            let resolved = fs::canonicalize(candidate).unwrap_or_else(|_| candidate.clone());
                            if resolved == cur || candidate == &cur {
                                next_idx = (i + 1) % files.len();
                                break;
                            }
                        }
                    }
                    let chosen = fs::canonicalize(&files[next_idx]).unwrap_or_else(|_| files[next_idx].clone());
                    call_checked(
                        &client,
                        &Request::ApplyVideo {
                            mode: None,
                            path: Some(chosen),
                            backdrop,
                        },
                    )?;
                    retire_simple_layers_best_effort(was_simple, &paths, &project_dir);
                    Ok(None)
                }
            }
        }
        CtlCommand::Engine { action } => match action {
            None => {
                if is_daemon_live(&client) {
                    let st = fetch_status(&client)?;
                    Ok(Some(st.engine.as_str().to_string()))
                } else {
                    let persisted = PersistedState::load(&paths);
                    if persisted.engine == EngineKind::Simple {
                        Ok(Some(EngineKind::Simple.as_str().to_string()))
                    } else {
                        ensure_daemon_live(&client, &paths, &project_dir, false)?;
                        let st = fetch_status(&client)?;
                        Ok(Some(st.engine.as_str().to_string()))
                    }
                }
            }
            Some(action) => {
                let current = current_engine_state(&client, &paths)?;
                let target = match action {
                    EngineAction::Full => EngineKind::Full,
                    EngineAction::Simple => EngineKind::Simple,
                    EngineAction::Video => EngineKind::Video,
                    EngineAction::Toggle => {
                        if current == EngineKind::Simple {
                            EngineKind::Full
                        } else {
                            EngineKind::Simple
                        }
                    }
                };

                if target == EngineKind::Simple {
                    let wp = resolve_current_wallpaper(&client, &paths)?;
                    close_picker_best_effort(&paths);
                    stop_quickshell_best_effort(&project_dir);
                    run_simple_wallpaper(&project_dir, &wp)?;
                    atomic_write_text(&paths.engine_file(), EngineKind::Simple.as_str())
                        .map_err(|e| e.to_string())?;
                    stop_daemon_if_live(&client)?;
                    Ok(None)
                } else {
                    let daemon_action = match target {
                        EngineKind::Full => EngineAction::Full,
                        EngineKind::Video => EngineAction::Video,
                        EngineKind::Simple => unreachable!(),
                    };
                    ensure_daemon_live(&client, &paths, &project_dir, true)?;
                    call_checked(&client, &Request::SetEngine { engine: daemon_action })?;
                    retire_simple_layers_best_effort(
                        current == EngineKind::Simple,
                        &paths,
                        &project_dir,
                    );
                    Ok(None)
                }
            }
        },
        CtlCommand::Transition { transition_type } => {
            ensure_daemon_live(&client, &paths, &project_dir, false)?;
            match transition_type {
                None => {
                    let st = fetch_status(&client)?;
                    let json = serde_json::to_string_pretty(&st.transition)
                        .map_err(|e| e.to_string())?;
                    Ok(Some(json))
                }
                Some(kind) => {
                    call_checked(
                        &client,
                        &Request::SetTransition {
                            transition_type: kind,
                        },
                    )?;
                    Ok(None)
                }
            }
        }
        CtlCommand::ReloadSettings => {
            ensure_daemon_live(&client, &paths, &project_dir, false)?;
            call_checked(&client, &Request::ReloadSettings)?;
            Ok(None)
        }
        CtlCommand::Catalog { images, videos } => {
            ensure_daemon_live(&client, &paths, &project_dir, false)?;
            let img_dir = match images {
                Some(d) => Some(absolutize(&d)?),
                None => None,
            };
            let vid_dir = match videos {
                Some(d) => Some(absolutize(&d)?),
                None => None,
            };
            let long_client = client.with_timeout(Duration::from_secs(120));
            let resp = call_checked(
                &long_client,
                &Request::Catalog {
                    image_dir: img_dir,
                    video_dir: vid_dir,
                },
            )?;
            let Some(ResponseData::Catalog(bundle)) = resp.data else {
                return Err("unexpected response payload for catalog".into());
            };
            let json = serde_json::to_string(&bundle).map_err(|e| e.to_string())?;
            Ok(Some(json))
        }
        CtlCommand::Picker { action } => {
            if use_quickshell_picker() {
                if !matches!(action, PickerAction::Close) {
                    ensure_daemon_live(&client, &paths, &project_dir, false)?;
                }
                ensure_quickshell_running(&project_dir)?;
                let status = Command::new("qs")
                    .args([
                        "ipc",
                        "-p",
                        &project_dir.to_string_lossy(),
                        "call",
                        "wallpaperCarousel",
                        action.qs_ipc_method(),
                    ])
                    .status()
                    .map_err(|e| format!("failed to execute qs ipc: {e}"))?;
                if !status.success() {
                    return Err(format!("qs ipc exited with status {status}"));
                }
                return Ok(None);
            }

            if !matches!(action, PickerAction::Close) {
                ensure_daemon_live(&client, &paths, &project_dir, false)?;
            }
            let picker_client = PickerIpcClient::new(paths.picker_socket_path())
                .with_timeout(Duration::from_millis(500));
            match picker_client.call(&action.picker_command()) {
                Ok(resp) => {
                    if resp.ok {
                        Ok(None)
                    } else {
                        Err(resp.error.unwrap_or_else(|| "picker command failed".into()))
                    }
                }
                Err(ProtocolError::Io(_) | ProtocolError::UnexpectedEof) => {
                    let Some(spawn_action) = action.spawn_action_arg() else {
                        return Ok(None);
                    };
                    spawn_picker_detached(&paths, &project_dir, spawn_action)?;
                    Ok(None)
                }
                Err(err) => Err(format!("picker IPC error: {err}")),
            }
        }
        CtlCommand::ClearCache { deep, picker, disk } => {
            execute_clear_cache(&client, &paths, &project_dir, deep, picker, disk)
        }
    }
}

fn absolutize(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        let cwd = env::current_dir().map_err(|e| e.to_string())?;
        Ok(cwd.join(path))
    }
}

fn is_daemon_live(client: &IpcClient) -> bool {
    let ping_client = client.clone().with_timeout(Duration::from_millis(800));
    matches!(
        ping_client.call(&Request::Ping),
        Ok(resp) if resp.ok && matches!(resp.data, Some(ResponseData::Pong))
    )
}

fn call_checked(client: &IpcClient, req: &Request) -> Result<bgpicker_core::protocol::Response, String> {
    let resp = client.call(req).map_err(|e| e.to_string())?;
    if resp.ok {
        Ok(resp)
    } else {
        Err(resp.error.unwrap_or_else(|| "daemon command failed".into()))
    }
}

fn fetch_status(client: &IpcClient) -> Result<DaemonStatus, String> {
    let resp = call_checked(client, &Request::Status)?;
    match resp.data {
        Some(ResponseData::Status(st)) => Ok(*st),
        _ => Err("daemon returned unexpected status payload".into()),
    }
}

fn format_human_status(st: &DaemonStatus) -> String {
    let wp = st
        .current_wallpaper
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "-".into());
    let vid = st
        .current_video
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "-".into());
    let focused_output = st.focused_output.as_deref().unwrap_or("-");
    format!(
        "engine: {}\nmode: {}\nwallpaper: {}\nvideo_mode: {}\nvideo: {}\nbackdrop_parallax: {}\nstartup_placeholder: {}\nplaceholder_cache: {}\ntransition: {}\nniri_connected: {}\noverview_open: {}\nfocused_output: {}\nbusy: {}\noutputs: {}",
        st.engine.as_str(),
        st.mode.as_str(),
        wp,
        st.video_mode.as_str(),
        vid,
        if st.backdrop_parallax { "on" } else { "off" },
        if st.startup_placeholder { "on" } else { "off" },
        if st.placeholder_cache { "on" } else { "off" },
        st.transition.transition_type.as_str(),
        st.niri_connected,
        st.overview_open,
        focused_output,
        st.busy,
        st.outputs.len(),
    )
}

fn use_quickshell_picker() -> bool {
    matches!(env::var("BGPICKER_USE_QS").as_deref(), Ok("1"))
}

pub fn resolve_picker_binary() -> PathBuf {
    if let Some(env_bin) = env::var_os("BGPICKER_PICKER").filter(|s| !s.is_empty()) {
        return PathBuf::from(env_bin);
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("bgpicker-picker");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from("bgpicker-picker")
}

fn close_picker_best_effort(paths: &BgPickerPaths) -> bool {
    let picker_client = PickerIpcClient::new(paths.picker_socket_path())
        .with_timeout(Duration::from_millis(500));
    matches!(picker_client.call(&PickerCommand::Close), Ok(resp) if resp.ok)
}

fn spawn_picker_detached(
    paths: &BgPickerPaths,
    project_dir: &Path,
    action: &str,
) -> Result<(), String> {
    fs::create_dir_all(&paths.cache_dir).map_err(|e| e.to_string())?;
    let log_path = paths.cache_dir.join("picker.log");
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("failed to open {}: {e}", log_path.display()))?;
    let log_err = log_file.try_clone().map_err(|e| e.to_string())?;

    let picker_bin = resolve_picker_binary();
    let mut cmd = Command::new(&picker_bin);
    cmd.arg("--project-dir")
        .arg(project_dir)
        .arg("--daemon-socket")
        .arg(paths.daemon_socket_path())
        .arg("--picker-socket")
        .arg(paths.picker_socket_path())
        .arg("--action")
        .arg(action)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_err));

    // Detach into its own session so terminal/keybind closure doesn't kill the picker.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    cmd.spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", picker_bin.display()))?;
    Ok(())
}

fn resolve_daemon_binary() -> PathBuf {
    if let Some(env_bin) = env::var_os("BGPICKER_DAEMON").filter(|s| !s.is_empty()) {
        return PathBuf::from(env_bin);
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("bgpicker-daemon");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from("bgpicker-daemon")
}

fn ensure_daemon_live(
    client: &IpcClient,
    paths: &BgPickerPaths,
    project_dir: &Path,
    no_restore: bool,
) -> Result<(), String> {
    if is_daemon_live(client) {
        return Ok(());
    }

    fs::create_dir_all(&paths.cache_dir).map_err(|e| e.to_string())?;
    let log_path = paths.cache_dir.join("daemon.log");
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("failed to open {}: {e}", log_path.display()))?;
    let log_err = log_file.try_clone().map_err(|e| e.to_string())?;

    let daemon_bin = resolve_daemon_binary();
    write_startup_trace(&log_file, "daemon_spawn_requested", "");
    let mut cmd = Command::new(&daemon_bin);
    cmd.arg("--socket")
        .arg(paths.daemon_socket_path())
        .arg("--project-dir")
        .arg(project_dir);
    if no_restore {
        cmd.arg("--no-restore");
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_err));

    // Detach into its own session so terminal closure doesn't kill the daemon.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", daemon_bin.display()))?;

    // Cold-boot NVIDIA EGL + compositor startup can exceed 5s under boot contention.
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        if is_daemon_live(client) {
            return Ok(());
        }
        if let Ok(Some(exit)) = child.try_wait() {
            return Err(format!(
                "bgpicker-daemon exited immediately with {exit}; see {}",
                log_path.display()
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }

    Err(format!(
        "timed out waiting for bgpicker-daemon socket at {}",
        paths.daemon_socket_path().display()
    ))
}

fn is_quickshell_running(project_dir: &Path) -> bool {
    Command::new("qs")
        .args(["ipc", "-p", &project_dir.to_string_lossy(), "show"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn ensure_quickshell_running(project_dir: &Path) -> Result<(), String> {
    if is_quickshell_running(project_dir) {
        return Ok(());
    }
    let mut cmd = Command::new("qs");
    cmd.args(["-p", &project_dir.to_string_lossy(), "-n", "-d"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch qs picker: {e}"))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if is_quickshell_running(project_dir) {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("qs picker exited early with {status}"));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err("timed out waiting for qs picker IPC".into())
}

fn stop_quickshell_best_effort(project_dir: &Path) {
    if !is_quickshell_running(project_dir) {
        return;
    }
    let _ = Command::new("qs")
        .args(["kill", "-p", &project_dir.to_string_lossy()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn simple_script_path(project_dir: &Path) -> PathBuf {
    project_dir.join("simple_wallpaper.sh")
}

fn simple_pids_exist(paths: &BgPickerPaths) -> bool {
    paths.state_dir.join("simple-front.pid").exists()
        || paths.state_dir.join("simple-back.pid").exists()
}

fn current_engine_state(client: &IpcClient, paths: &BgPickerPaths) -> Result<EngineKind, String> {
    if is_daemon_live(client) {
        Ok(fetch_status(client)?.engine)
    } else {
        Ok(PersistedState::load(paths).engine)
    }
}

fn resolve_current_wallpaper(client: &IpcClient, paths: &BgPickerPaths) -> Result<PathBuf, String> {
    let candidate = if is_daemon_live(client) {
        fetch_status(client)?
            .current_wallpaper
            .or_else(|| PersistedState::load(paths).current_wallpaper)
    } else {
        PersistedState::load(paths).current_wallpaper
    };
    let wp = candidate
        .filter(|p| p.is_file())
        .ok_or_else(|| "BgPicker: select a wallpaper first".to_string())?;
    fs::canonicalize(&wp)
        .map_err(|e| format!("BgPicker: failed to resolve {}: {e}", wp.display()))
}

fn run_simple_wallpaper(project_dir: &Path, image: &Path) -> Result<(), String> {
    let script = simple_script_path(project_dir);
    if !script.exists() {
        return Ok(());
    }
    let output = Command::new(&script)
        .arg(image)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run {}: {e}", script.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("exited with status {}", output.status)
        } else {
            stderr
        };
        return Err(format!("{} failed: {detail}", script.display()));
    }
    Ok(())
}

fn stop_simple_wallpaper_best_effort(project_dir: &Path) {
    let script = simple_script_path(project_dir);
    if !script.exists() {
        return;
    }
    let _ = Command::new(&script)
        .arg("--stop")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn retire_simple_layers_best_effort(was_simple: bool, paths: &BgPickerPaths, project_dir: &Path) {
    if !was_simple && !simple_pids_exist(paths) {
        return;
    }
    if !simple_script_path(project_dir).exists() {
        return;
    }
    // Wait briefly so the newly presented bgpicker-daemon frame commits before swaybg retires.
    thread::sleep(Duration::from_millis(100));
    stop_simple_wallpaper_best_effort(project_dir);
}

fn startup_placeholder_settle_delay(uses_swaybg: bool) -> Duration {
    if let Ok(raw) = env::var("BGPICKER_PLACEHOLDER_SETTLE_MS") {
        if let Ok(ms) = raw.trim().parse::<u64>() {
            return Duration::from_millis(ms.min(5000));
        }
    }
    // `bgpicker-splash` is retired by `bgpicker-daemon` immediately after Niri
    // confirms Frame 1 via `wl_surface.frame`, so no blind sleep is needed.
    if uses_swaybg {
        Duration::from_millis(120)
    } else {
        Duration::ZERO
    }
}

fn execute_on_off_option<F, S>(
    client: &IpcClient,
    action: Option<BackdropAction>,
    state_file: &Path,
    persisted_value: bool,
    read_status: S,
    make_req: F,
) -> Result<Option<String>, String>
where
    F: FnOnce(BackdropAction) -> Request,
    S: FnOnce(&DaemonStatus) -> bool,
{
    match action {
        None => {
            let current = if is_daemon_live(client) {
                read_status(&fetch_status(client)?)
            } else {
                persisted_value
            };
            Ok(Some(if current { "on" } else { "off" }.into()))
        }
        Some(act) => {
            if is_daemon_live(client) {
                call_checked(client, &make_req(act))?;
            } else {
                let next = match act {
                    BackdropAction::On => true,
                    BackdropAction::Off => false,
                    BackdropAction::Toggle => !persisted_value,
                };
                atomic_write_text(state_file, if next { "on" } else { "off" })
                    .map_err(|e| e.to_string())?;
            }
            Ok(None)
        }
    }
}

fn warm_placeholder_caches_best_effort(
    paths: &BgPickerPaths,
    project_dir: &Path,
    persisted: &PersistedState,
) {
    let mut fg_static_jpg = None;
    if let Some(ref wp) = persisted.current_wallpaper {
        if wp.is_file() {
            fg_static_jpg = prepare_startup_placeholder(wp, &paths.cache_dir, false).ok();
        }
    }
    let video = persisted
        .video_wallpaper
        .as_ref()
        .filter(|p| p.is_file())
        .cloned()
        .or_else(|| list_videos_sorted(&project_dir.join("dynamic")).into_iter().next());
    let mut fg_video_jpg = None;
    if let Some(vid) = video {
        fg_video_jpg = prepare_startup_placeholder(&vid, &paths.cache_dir, true).ok();
    }

    let splash_src = match persisted.engine {
        EngineKind::Video => fg_video_jpg.as_deref().or(fg_static_jpg.as_deref()),
        EngineKind::Full | EngineKind::Simple => {
            persisted.current_wallpaper.as_deref().filter(|p| p.is_file())
        }
    };
    if let Some(src) = splash_src {
        if let Ok(frame) = FrameInput::from_image_path(src, 8192) {
            let src_is_bgra = match frame.format {
                PixelFormat::Rgba8 => false,
                PixelFormat::Bgra8 => true,
                PixelFormat::Nv12 { .. } => return,
            };
            let _ = write_splash_bin_from_packed(
                &paths.startup_splash_fg_bin_file(),
                frame.width,
                frame.height,
                frame.stride_bytes,
                &frame.data,
                src_is_bgra,
                2560,
                1600,
            );
        }
    }
}

fn resolve_splash_binary() -> Option<PathBuf> {
    if let Some(env_bin) = env::var_os("BGPICKER_SPLASH").filter(|s| !s.is_empty()) {
        let p = PathBuf::from(env_bin);
        return p.is_file().then_some(p);
    }
    // Preserve explicit `BGPICKER_SWAYBG` overrides (e.g. legacy shims and swaybg tests).
    if env::var_os("BGPICKER_SWAYBG").is_some_and(|s| !s.is_empty()) {
        return None;
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("bgpicker-splash");
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join("bgpicker-splash");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn resolve_swaybg_binary() -> Option<PathBuf> {
    if let Some(env_bin) = env::var_os("BGPICKER_SWAYBG").filter(|s| !s.is_empty()) {
        let p = PathBuf::from(env_bin);
        return p.is_file().then_some(p);
    }
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join("swaybg");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn prepare_swaybg_front(paths: &BgPickerPaths, swaybg_bin: &Path) -> PathBuf {
    const OLD_NS: &[u8] = b"wallpaper\0";
    const NEW_NS: &[u8] = b"bgpick-fg\0";

    let Ok(mut data) = fs::read(swaybg_bin) else {
        return swaybg_bin.to_path_buf();
    };
    let positions: Vec<usize> = data
        .windows(OLD_NS.len())
        .enumerate()
        .filter_map(|(idx, win)| (win == OLD_NS).then_some(idx))
        .collect();
    if positions.len() != 1 {
        // E.g. test shell shim or unexpected binary layout -> reuse directly.
        return swaybg_bin.to_path_buf();
    }
    let pos = positions[0];
    data[pos..pos + NEW_NS.len()].copy_from_slice(NEW_NS);

    let target = paths.cache_dir.join("swaybg-front");
    if fs::read(&target).ok().as_deref() == Some(data.as_slice()) {
        return target;
    }
    if fs::create_dir_all(&paths.cache_dir).is_err() {
        return swaybg_bin.to_path_buf();
    }
    let tmp = paths
        .cache_dir
        .join(format!(".swaybg-front.tmp.{}", std::process::id()));
    let write_res = (|| -> std::io::Result<()> {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(&data)?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        let mut perms = fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tmp, perms)?;
        fs::rename(&tmp, &target)?;
        Ok(())
    })();
    if write_res.is_ok() {
        target
    } else {
        let _ = fs::remove_file(&tmp);
        swaybg_bin.to_path_buf()
    }
}

fn placeholder_extra_bins(paths: &BgPickerPaths, splash_bin: Option<&Path>) -> Vec<PathBuf> {
    let mut bins = Vec::new();
    if let Some(sb) = splash_bin {
        bins.push(sb.to_path_buf());
    }
    if let Some(sw) = resolve_swaybg_binary() {
        bins.push(sw);
    }
    bins.push(paths.cache_dir.join("swaybg-front"));
    bins
}

fn cleanup_stale_startup_placeholders(paths: &BgPickerPaths) {
    let splash_bin = resolve_splash_binary();
    let bins = placeholder_extra_bins(paths, splash_bin.as_deref());
    let refs: Vec<&Path> = bins.iter().map(|p| p.as_path()).collect();
    retire_startup_placeholders(paths, &refs);
}

fn wait_for_ready_pipe(read_fd: &OwnedFd, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut pfd = libc::pollfd {
            fd: read_fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if rc < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if rc > 0 {
            return;
        }
    }
}

fn spawn_splash_process(
    bin: &Path,
    fg_bin: Option<&Path>,
    fg_image: Option<&Path>,
    bg_image: Option<&Path>,
    pid_file: &Path,
) -> Option<Child> {
    if fg_bin.is_none() && fg_image.is_none() && bg_image.is_none() {
        return None;
    }
    let mut fds = [-1_i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return None;
    }
    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    let write_raw_fd = write_fd.as_raw_fd();

    let mut cmd = Command::new(bin);
    cmd.args(["--ready-fd", &write_raw_fd.to_string()]);
    if let Some(p) = fg_bin {
        cmd.arg("--fg-bin").arg(p);
    }
    if let Some(p) = fg_image {
        cmd.arg("--fg-image").arg(p);
    }
    if let Some(p) = bg_image {
        cmd.arg("--bg-image").arg(p);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Keep `--ready-fd` inherited across `exec` in the child while tying child
    // lifetime to this parent (`PR_SET_PDEATHSIG`) if startup aborts unexpectedly.
    unsafe {
        cmd.pre_exec(move || {
            let flags = libc::fcntl(write_raw_fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(write_raw_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }

    let child = cmd.spawn().ok()?;
    // Close parent's write end immediately so poll sees POLLHUP if child dies early.
    drop(write_fd);

    let pid = child.id();
    record_placeholder_pid(pid_file, pid);

    // Wait briefly until Frame 1 (`bgpick-fg`) has committed+flushed before spawning
    // `bgpicker-daemon` so Frame 1 reaches the compositor without GPU/CPU contention.
    wait_for_ready_pipe(&read_fd, Duration::from_millis(400));
    Some(child)
}

fn spawn_placeholder_process(bin: &Path, image: &Path, pid_file: &Path) -> Option<Child> {
    let mut cmd = Command::new(bin);
    cmd.args(["-i"])
        .arg(image)
        .args(["-m", "fill"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Ensure kernel terminates temporary placeholders automatically if `bgpickerctl startup` crashes.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let child = cmd.spawn().ok()?;
    let pid = child.id();
    record_placeholder_pid(pid_file, pid);
    Some(child)
}

fn stop_placeholder_child(child: &mut Option<Child>, pid_file: &Path) {
    if let Some(mut proc) = child.take() {
        let pid = proc.id() as libc::pid_t;
        if let Ok(None) = proc.try_wait() {
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_millis(250);
            let mut reaped = false;
            while Instant::now() < deadline {
                if let Ok(Some(_)) = proc.try_wait() {
                    reaped = true;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            if !reaped {
                let _ = proc.kill();
                let _ = proc.wait();
            }
        }
    }
    let _ = fs::remove_file(pid_file);
}

struct StartupPlaceholderGuard {
    back: Option<Child>,
    front: Option<Child>,
    back_pid_file: PathBuf,
    front_pid_file: PathBuf,
    used_swaybg: bool,
}

impl StartupPlaceholderGuard {
    fn spawn(
        paths: &BgPickerPaths,
        persisted: &PersistedState,
        fg_image: Option<&Path>,
        bg_image: Option<&Path>,
    ) -> Self {
        let back_pid_file = paths.startup_placeholder_back_pid_file();
        let front_pid_file = paths.startup_placeholder_front_pid_file();

        if let Some(splash_bin) = resolve_splash_binary() {
            let fg_bin_candidate = paths.startup_splash_fg_bin_file();
            let fg_bin = (persisted.placeholder_cache && fg_bin_candidate.is_file())
                .then_some(fg_bin_candidate.as_path());
            let front =
                spawn_splash_process(&splash_bin, fg_bin, fg_image, bg_image, &front_pid_file);
            return Self {
                back: None,
                front,
                back_pid_file,
                front_pid_file,
                used_swaybg: false,
            };
        }

        let Some(swaybg_bin) = resolve_swaybg_binary() else {
            return Self {
                back: None,
                front: None,
                back_pid_file,
                front_pid_file,
                used_swaybg: false,
            };
        };

        let back = bg_image.and_then(|img| spawn_placeholder_process(&swaybg_bin, img, &back_pid_file));
        let front = fg_image.and_then(|img| {
            let front_bin = prepare_swaybg_front(paths, &swaybg_bin);
            spawn_placeholder_process(&front_bin, img, &front_pid_file)
        });

        let used_swaybg = back.is_some() || front.is_some();
        Self {
            back,
            front,
            back_pid_file,
            front_pid_file,
            used_swaybg,
        }
    }

    fn is_active(&mut self) -> bool {
        let back_alive = self
            .back
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)));
        let front_alive = self
            .front
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)));
        back_alive || front_alive
    }

    fn retire(&mut self) {
        stop_placeholder_child(&mut self.front, &self.front_pid_file);
        stop_placeholder_child(&mut self.back, &self.back_pid_file);
    }
}

impl Drop for StartupPlaceholderGuard {
    fn drop(&mut self) {
        self.retire();
    }
}

fn stop_daemon_if_live(client: &IpcClient) -> Result<bool, String> {
    if !is_daemon_live(client) {
        return Ok(false);
    }
    call_checked(client, &Request::Stop)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !is_daemon_live(client) {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err("bgpicker daemon has not stopped yet.".into())
}

fn execute_clear_cache(
    client: &IpcClient,
    paths: &BgPickerPaths,
    project_dir: &Path,
    deep: bool,
    picker: bool,
    disk: bool,
) -> Result<Option<String>, String> {
    let mut lines = Vec::new();

    if is_daemon_live(client) {
        let st = fetch_status(client)?;
        let report = call_clear_cache(client)?;
        if deep {
            if st.engine == EngineKind::Video {
                lines.push(format!(
                    "daemon trimmed (skipped --deep restart during video): PSS {} -> {}, RSS {} -> {}",
                    format_kib_human(report.pss_before_kib),
                    format_kib_human(report.pss_after_kib),
                    format_kib_human(report.rss_before_kib),
                    format_kib_human(report.rss_after_kib),
                ));
            } else {
                stop_daemon_if_live(client)?;
                ensure_daemon_live(client, paths, project_dir, true)?;
                call_checked(client, &Request::Startup)?;
                let fresh = call_clear_cache(client)?;
                lines.push(format!(
                    "daemon restarted (--deep): PSS {} -> {}, RSS {} -> {}",
                    format_kib_human(report.pss_before_kib),
                    format_kib_human(fresh.pss_after_kib),
                    format_kib_human(report.rss_before_kib),
                    format_kib_human(fresh.rss_after_kib),
                ));
            }
        } else {
            let video_note = if report.video_backend_stopped {
                ", idle video worker released"
            } else {
                ""
            };
            lines.push(format!(
                "daemon trimmed: PSS {} -> {}, RSS {} -> {}{video_note}",
                format_kib_human(report.pss_before_kib),
                format_kib_human(report.pss_after_kib),
                format_kib_human(report.rss_before_kib),
                format_kib_human(report.rss_after_kib),
            ));
        }
    } else {
        lines.push("daemon: not running".into());
    }

    if picker {
        let closed_native = close_picker_best_effort(paths);
        if closed_native {
            lines.push("picker: closed".into());
        }
        if is_quickshell_running(project_dir) {
            stop_quickshell_and_wait(project_dir);
            if use_quickshell_picker() {
                ensure_quickshell_running(project_dir)?;
                lines.push("qs picker: restarted".into());
            } else {
                lines.push("qs picker: stopped".into());
            }
        } else if !closed_native {
            lines.push("picker: not running".into());
        }
    }

    if disk {
        let freed = clean_disk_caches(paths)?;
        lines.push(format!("disk caches cleared: {}", format_bytes_human(freed)));
    }

    Ok(Some(lines.join("\n")))
}

fn call_clear_cache(client: &IpcClient) -> Result<ClearCacheReport, String> {
    let resp = call_checked(client, &Request::ClearCache)?;
    match resp.data {
        Some(ResponseData::CacheCleared(report)) => Ok(report),
        _ => Err("daemon returned unexpected clear-cache payload".into()),
    }
}

fn stop_quickshell_and_wait(project_dir: &Path) {
    stop_quickshell_best_effort(project_dir);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if !is_quickshell_running(project_dir) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn clean_disk_caches(paths: &BgPickerPaths) -> Result<u64, String> {
    let persisted = PersistedState::load(paths);
    let keep_blur_file = if persisted.engine == EngineKind::Simple {
        fs::read_to_string(paths.legacy_swaybg_cache_file())
            .ok()
            .map(|s| PathBuf::from(s.trim()))
            .filter(|p| !p.as_os_str().is_empty())
    } else {
        None
    };

    let mut freed = 0u64;
    for dir in [
        paths.video_blur_cache_dir(),
        paths.image_thumbnails_dir(),
        paths.video_thumbnails_dir(),
        paths.blur_cache_dir(),
        paths.startup_placeholder_cache_dir(),
    ] {
        freed += clear_cache_dir(&dir, keep_blur_file.as_deref()).map_err(|e| e.to_string())?;
    }

    let splash_bin = paths.startup_splash_fg_bin_file();
    if let Ok(meta) = fs::metadata(&splash_bin) {
        if meta.is_file() && fs::remove_file(&splash_bin).is_ok() {
            freed += meta.len();
        }
    }

    for log_name in ["daemon.log", "picker.log", "simple-swaybg.log", "video.log"] {
        let log_path = paths.cache_dir.join(log_name);
        if let Ok(meta) = fs::metadata(&log_path) {
            if meta.is_file()
                && meta.len() > 0
                && OpenOptions::new().write(true).truncate(true).open(&log_path).is_ok()
            {
                freed += meta.len();
            }
        }
    }
    Ok(freed)
}

fn clear_cache_dir(dir: &Path, keep_file: Option<&Path>) -> std::io::Result<u64> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(0);
    };
    let mut freed = 0u64;
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            freed += clear_cache_dir(&path, keep_file)?;
            let _ = fs::remove_dir(&path);
            continue;
        }
        if keep_file.is_some_and(|k| k == path) {
            continue;
        }
        let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if fs::remove_file(&path).is_ok() {
            freed += len;
        }
    }
    Ok(freed)
}

fn format_kib_human(kib: u64) -> String {
    format_bytes_human(kib.saturating_mul(1024))
}

fn format_bytes_human(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * KIB;
    const GIB: f64 = 1024.0 * MIB;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.0} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}
