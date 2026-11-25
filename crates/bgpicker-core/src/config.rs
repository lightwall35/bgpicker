//! Settings parsing, clamping/validation, random transition resolution, and
//! atomic `settings.json` updates matching `wallpaper_settings.py`,
//! `WallpaperTransitions.js`, and `WallpaperMath.js`.

use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::str::FromStr;

/// Errors returned while reading, validating, or persisting `settings.json`.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON in settings.json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("settings.json must contain a JSON object")]
    NotAnObject,
    #[error("unknown transition type: {0}")]
    InvalidTransitionType(String),
}

/// Supported transition effect types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransitionType {
    #[default]
    Fade,
    Wipe,
    Wave,
    Grow,
    Outer,
    Random,
    None,
}

impl TransitionType {
    /// All concrete animated effects eligible for `"random"` selection.
    pub const CONCRETE_EFFECTS: [Self; 5] =
        [Self::Fade, Self::Wipe, Self::Wave, Self::Grow, Self::Outer];

    /// All valid user-facing choices in UI order.
    pub const ALL: [Self; 7] = [
        Self::Fade,
        Self::Wipe,
        Self::Wave,
        Self::Grow,
        Self::Outer,
        Self::Random,
        Self::None,
    ];

    /// Canonical lowercase string identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fade => "fade",
            Self::Wipe => "wipe",
            Self::Wave => "wave",
            Self::Grow => "grow",
            Self::Outer => "outer",
            Self::Random => "random",
            Self::None => "none",
        }
    }

    /// Shader uniform kind integer matching `WallpaperTransitions.kind(type)`.
    pub const fn shader_kind(self) -> u32 {
        match self {
            Self::Fade | Self::Random => 0,
            Self::Wipe => 1,
            Self::Wave => 2,
            Self::Grow => 3,
            Self::Outer => 4,
            Self::None => 5,
        }
    }
}

impl FromStr for TransitionType {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fade" => Ok(Self::Fade),
            "wipe" => Ok(Self::Wipe),
            "wave" => Ok(Self::Wave),
            "grow" => Ok(Self::Grow),
            "outer" => Ok(Self::Outer),
            "random" => Ok(Self::Random),
            "none" => Ok(Self::None),
            other => Err(ConfigError::InvalidTransitionType(other.to_string())),
        }
    }
}

impl std::fmt::Display for TransitionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// UI choice entry matching `WallpaperTransitions.choices`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TransitionChoice {
    pub value: TransitionType,
    pub label: &'static str,
}

/// Transition choice table matching `WallpaperTransitions.js`.
pub const TRANSITION_CHOICES: [TransitionChoice; 7] = [
    TransitionChoice {
        value: TransitionType::Fade,
        label: "淡化",
    },
    TransitionChoice {
        value: TransitionType::Wipe,
        label: "斜扫",
    },
    TransitionChoice {
        value: TransitionType::Wave,
        label: "波浪",
    },
    TransitionChoice {
        value: TransitionType::Grow,
        label: "圆形展开",
    },
    TransitionChoice {
        value: TransitionType::Outer,
        label: "圆形收拢",
    },
    TransitionChoice {
        value: TransitionType::Random,
        label: "随机",
    },
    TransitionChoice {
        value: TransitionType::None,
        label: "直接",
    },
];

/// Parallax configuration normalized from `settings.json` (`WallpaperMath.settings`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParallaxSettings {
    pub zoom: f64,
    pub column_span: u32,
    pub duration_ms: u32,
    pub mode_transition_ms: u32,
    pub transition_ms: u32,
    pub max_texture_size: u32,
}

impl Default for ParallaxSettings {
    fn default() -> Self {
        Self {
            zoom: 1.12,
            column_span: 6,
            duration_ms: 350,
            mode_transition_ms: 500,
            transition_ms: 350,
            max_texture_size: 8192,
        }
    }
}

/// Transition preferences normalized from `settings.json` (`wallpaper_settings.normalize`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionSettings {
    #[serde(rename = "type")]
    pub transition_type: TransitionType,
    pub duration_ms: u32,
    pub fps: u32,
    pub angle: f64,
    pub position: [f64; 2],
    pub wave: [f64; 2],
}

impl Default for TransitionSettings {
    fn default() -> Self {
        Self {
            transition_type: TransitionType::Fade,
            duration_ms: 800,
            fps: 60,
            angle: 45.0,
            position: [0.5, 0.5],
            wave: [20.0, 20.0],
        }
    }
}

/// Optional mode-switch hint attached to a transition commit snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModeSwitch {
    Enter,
    Exit,
}

/// Concrete immutable transition snapshot for one wallpaper commit (`wallpaper_settings.resolve`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedTransition {
    #[serde(rename = "type")]
    pub transition_type: TransitionType,
    pub duration_ms: u32,
    pub fps: u32,
    pub angle: f64,
    pub position: [f64; 2],
    pub wave: [f64; 2],
    pub selected_type: TransitionType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode_switch: Option<ModeSwitch>,
}

impl ResolvedTransition {
    /// Builds CLI-compatible transition flags matching `wallpaper_settings.awww_args`.
    pub fn awww_args(&self) -> Vec<String> {
        let [x, y] = self.position;
        let [wave_w, wave_h] = self.wave;
        vec![
            "--transition-type".into(),
            self.transition_type.as_str().into(),
            "--transition-duration".into(),
            format!("{}", f64::from(self.duration_ms) / 1000.0),
            "--transition-fps".into(),
            self.fps.to_string(),
            "--transition-angle".into(),
            format!("{}", self.angle),
            "--transition-pos".into(),
            format!("{x:.6},{y:.6}"),
            "--invert-y".into(),
            "--transition-wave".into(),
            format!("{wave_w},{wave_h}"),
            "--transition-bezier".into(),
            ".54,0,.34,.99".into(),
        ]
    }
}

/// Combined normalized configuration parsed from `settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BgPickerConfig {
    pub parallax: ParallaxSettings,
    pub transition: TransitionSettings,
}

impl BgPickerConfig {
    /// Normalizes a raw JSON object into bounded parallax and transition settings.
    pub fn from_value(value: &Value) -> Self {
        Self {
            parallax: normalize_parallax(value),
            transition: normalize_transition(value),
        }
    }

    /// Reads and normalizes `settings_path` (missing file yields defaults).
    pub fn load(settings_path: &Path) -> Result<Self, ConfigError> {
        let value = read_settings_json(settings_path)?;
        Ok(Self::from_value(&value))
    }
}

fn bounded_f64(raw: Option<&Value>, fallback: f64, low: f64, high: f64) -> f64 {
    let Some(Value::Number(num)) = raw else {
        return fallback;
    };
    let Some(n) = num.as_f64() else {
        return fallback;
    };
    if n.is_finite() {
        n.clamp(low, high)
    } else {
        fallback
    }
}

fn bounded_u32(raw: Option<&Value>, fallback: f64, low: f64, high: f64) -> u32 {
    bounded_f64(raw, fallback, low, high).round() as u32
}

fn bounded_pair(raw: Option<&Value>, fallback: [f64; 2], low: f64, high: f64) -> [f64; 2] {
    let Some(Value::Array(items)) = raw else {
        return fallback;
    };
    if items.len() != 2 {
        return fallback;
    }
    [
        bounded_f64(Some(&items[0]), fallback[0], low, high),
        bounded_f64(Some(&items[1]), fallback[1], low, high),
    ]
}

/// Normalizes `config.parallax` matching `WallpaperMath.settings(value)`.
pub fn normalize_parallax(config: &Value) -> ParallaxSettings {
    let p = config.get("parallax").and_then(Value::as_object);
    let get = |k: &str| p.and_then(|obj| obj.get(k));
    ParallaxSettings {
        zoom: bounded_f64(get("zoom"), 1.12, 1.0, 2.0),
        column_span: bounded_u32(get("columnSpan"), 6.0, 0.0, 100.0),
        duration_ms: bounded_u32(get("durationMs"), 350.0, 0.0, 3000.0),
        mode_transition_ms: bounded_u32(get("modeTransitionMs"), 500.0, 0.0, 3000.0),
        transition_ms: bounded_u32(get("transitionMs"), 350.0, 0.0, 3000.0),
        max_texture_size: bounded_u32(get("maxTextureSize"), 8192.0, 1024.0, 16384.0),
    }
}

/// Normalizes `config.transition` matching `wallpaper_settings.normalize(config)`.
pub fn normalize_transition(config: &Value) -> TransitionSettings {
    let p = config.get("transition").and_then(Value::as_object);
    let legacy = config
        .get("parallax")
        .and_then(Value::as_object)
        .and_then(|obj| obj.get("transitionMs"));
    let get = |k: &str| p.and_then(|obj| obj.get(k));

    let transition_type = get("type")
        .and_then(Value::as_str)
        .and_then(|s| TransitionType::from_str(s).ok())
        .unwrap_or(TransitionType::Fade);
    let duration_fallback = bounded_f64(legacy, 800.0, 0.0, 5000.0);

    TransitionSettings {
        transition_type,
        duration_ms: bounded_u32(get("durationMs"), duration_fallback, 0.0, 5000.0),
        fps: bounded_u32(get("fps"), 60.0, 1.0, 240.0),
        angle: bounded_f64(get("angle"), 45.0, 0.0, 360.0),
        position: bounded_pair(get("position"), [0.5, 0.5], 0.0, 1.0),
        wave: bounded_pair(get("wave"), [20.0, 20.0], 1.0, 500.0),
    }
}

/// Resolves a single concrete transition snapshot using `rand::thread_rng()`.
pub fn resolve_transition(config: &Value, instant: bool) -> ResolvedTransition {
    let mut rng = rand::thread_rng();
    resolve_transition_with_rng(config, instant, &mut rng)
}

/// Resolves a single concrete transition snapshot using a caller-supplied RNG.
pub fn resolve_transition_with_rng<R: Rng + ?Sized>(
    config: &Value,
    instant: bool,
    rng: &mut R,
) -> ResolvedTransition {
    let normalized = normalize_transition(config);
    let selected_type = normalized.transition_type;
    let mut resolved_type = if selected_type == TransitionType::Random {
        let idx = rng.gen_range(0..TransitionType::CONCRETE_EFFECTS.len());
        TransitionType::CONCRETE_EFFECTS[idx]
    } else {
        selected_type
    };
    let mut duration_ms = normalized.duration_ms;
    if instant || duration_ms == 0 {
        resolved_type = TransitionType::None;
        duration_ms = 0;
    }
    ResolvedTransition {
        transition_type: resolved_type,
        duration_ms,
        fps: normalized.fps,
        angle: normalized.angle,
        position: normalized.position,
        wave: normalized.wave,
        selected_type,
        mode_switch: None,
    }
}

/// Reads `settings.json` as a raw JSON object (`{}` if missing).
///
/// Returns an error if the file contains invalid JSON or a non-object root.
pub fn read_settings_json(settings_path: &Path) -> Result<Value, ConfigError> {
    let raw = match fs::read_to_string(settings_path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Object(Map::new()));
        }
        Err(err) => return Err(ConfigError::Io(err)),
    };
    let value: Value = serde_json::from_str(&raw)?;
    if !value.is_object() {
        return Err(ConfigError::NotAnObject);
    }
    Ok(value)
}

/// Acquires an exclusive `flock` on `lock_path`, executes `f`, and releases the lock.
pub fn with_preference_lock<T, F>(lock_path: &Path, f: F) -> Result<T, ConfigError>
where
    F: FnOnce() -> Result<T, ConfigError>,
{
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(lock_path)?;
    flock_exclusive(&lock_file)?;
    let res = f();
    let _ = flock_unlock(&lock_file);
    res
}

fn flock_exclusive(file: &File) -> std::io::Result<()> {
    loop {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

fn flock_unlock(file: &File) -> std::io::Result<()> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Atomically updates `transition.type` in `settings_path` under `lock_path`,
/// preserving all unrelated fields (matching `wallpaper_settings.set_transition`).
pub fn set_transition_type(
    settings_path: &Path,
    lock_path: &Path,
    kind: TransitionType,
) -> Result<(), ConfigError> {
    with_preference_lock(lock_path, || {
        let mut root = read_settings_json(settings_path)?;
        let root_obj = root.as_object_mut().ok_or(ConfigError::NotAnObject)?;
        let mut transition = match root_obj.get("transition") {
            Some(Value::Object(prev)) => prev.clone(),
            _ => Map::new(),
        };
        transition.insert(
            "type".to_string(),
            Value::String(kind.as_str().to_string()),
        );
        root_obj.insert("transition".to_string(), Value::Object(transition));

        let parent = settings_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;

        let tmp_name = format!(
            ".settings-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        );
        let tmp_path = parent.join(tmp_name);
        let result = (|| -> Result<(), ConfigError> {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)?;
            serde_json::to_writer_pretty(&mut file, &root)?;
            file.write_all(b"\n")?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp_path, settings_path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        result
    })
}
