//! Wallpaper geometry, transition specifications, Kawase blur parameters, and easing curves.
//!
//! Matches `WallpaperMath.js`, `WallpaperTransitions.js`, `ParallaxWallpaper.qml`,
//! `DualKawaseBlur.qml`, and `VideoTransitionLayer.qml`.

/// Computed placement rectangle of the wallpaper texture in viewport coordinates.
///
/// Origin `(0, 0)` is the top-left of the viewport; `(x, y)` may be negative when
/// the scaled media overflows the viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallpaperGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub overflow_x: f32,
}

impl WallpaperGeometry {
    #[inline]
    pub fn as_rect_uniform(self) -> [f32; 4] {
        [self.x, self.y, self.width, self.height]
    }
}

/// Parallax and texture limits matching `WallpaperMath.settings`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParallaxSettings {
    pub zoom: f32,
    pub duration_ms: u32,
    pub mode_transition_ms: u32,
    pub transition_ms: u32,
    pub max_texture_size: u32,
}

impl Default for ParallaxSettings {
    fn default() -> Self {
        Self {
            zoom: 1.12,
            duration_ms: 350,
            mode_transition_ms: 500,
            transition_ms: 350,
            max_texture_size: 8192,
        }
    }
}

impl ParallaxSettings {
    pub fn clamped(self) -> Self {
        Self {
            zoom: if self.zoom.is_finite() {
                self.zoom.clamp(1.0, 2.0)
            } else {
                1.12
            },
            duration_ms: self.duration_ms.min(3000),
            mode_transition_ms: self.mode_transition_ms.min(3000),
            transition_ms: self.transition_ms.min(3000),
            max_texture_size: self.max_texture_size.clamp(1024, 16384),
        }
    }
}

/// Supported GPU transition shader effects matching `shaders/wallpaper-transition.frag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TransitionEffect {
    #[default]
    Fade,
    Wipe,
    Wave,
    Grow,
    Outer,
    None,
}

impl TransitionEffect {
    /// Numeric `effectKind` uniform consumed by the transition fragment shader.
    #[inline]
    pub const fn shader_kind(self) -> i32 {
        match self {
            Self::Fade => 0,
            Self::Wipe => 1,
            Self::Wave => 2,
            Self::Grow => 3,
            Self::Outer => 4,
            Self::None => 5,
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name {
            "fade" => Self::Fade,
            "wipe" => Self::Wipe,
            "wave" => Self::Wave,
            "grow" => Self::Grow,
            "outer" => Self::Outer,
            "none" => Self::None,
            _ => Self::Fade,
        }
    }
}

/// Immutable snapshot of parameters for one wallpaper/video transition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitionSpec {
    pub effect: TransitionEffect,
    pub duration_ms: u32,
    pub angle_deg: f32,
    pub position: [f32; 2],
    pub wave: [f32; 2],
}

impl Default for TransitionSpec {
    fn default() -> Self {
        Self {
            effect: TransitionEffect::Fade,
            duration_ms: 800,
            angle_deg: 45.0,
            position: [0.5, 0.5],
            wave: [20.0, 20.0],
        }
    }
}

impl TransitionSpec {
    pub fn instant() -> Self {
        Self {
            effect: TransitionEffect::None,
            duration_ms: 0,
            ..Self::default()
        }
    }

    pub fn clamped(self) -> Self {
        let angle = if self.angle_deg.is_finite() {
            self.angle_deg.clamp(0.0, 360.0)
        } else {
            45.0
        };
        let pos_x = if self.position[0].is_finite() {
            self.position[0].clamp(0.0, 1.0)
        } else {
            0.5
        };
        let pos_y = if self.position[1].is_finite() {
            self.position[1].clamp(0.0, 1.0)
        } else {
            0.5
        };
        let wave_x = if self.wave[0].is_finite() {
            self.wave[0].clamp(1.0, 500.0)
        } else {
            20.0
        };
        let wave_y = if self.wave[1].is_finite() {
            self.wave[1].clamp(1.0, 500.0)
        } else {
            20.0
        };
        Self {
            effect: self.effect,
            duration_ms: self.duration_ms.min(5000),
            angle_deg: angle,
            position: [pos_x, pos_y],
            wave: [wave_x, wave_y],
        }
    }

    /// Direction vector matching `ParallaxWallpaper.qml` and `VideoTransitionLayer.qml`.
    #[inline]
    pub fn direction_vec(self) -> [f32; 2] {
        let rad = self.angle_deg.to_radians();
        [-rad.cos(), rad.sin()]
    }
}

/// Real-time 3-pass Dual-Kawase blur parameters matching `DualKawaseBlur.qml`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KawaseSettings {
    pub passes: u32,
    pub offset: f32,
    pub saturation: f32,
    pub brightness: f32,
}

impl Default for KawaseSettings {
    fn default() -> Self {
        Self {
            passes: 3,
            offset: 1.25,
            saturation: 1.12,
            brightness: -0.04,
        }
    }
}

/// Computes cover-mode geometry with optional overscan zoom (`WallpaperMath.geometry`).
///
/// Wide panoramas (`aspect > viewport_aspect * 1.1`) already provide horizontal travel
/// without upscaling by `zoom`.
pub fn compute_geometry(
    viewport_w: f32,
    viewport_h: f32,
    media_w: f32,
    media_h: f32,
    zoom: f32,
) -> WallpaperGeometry {
    let w = viewport_w.max(1.0);
    let h = viewport_h.max(1.0);
    let aspect = if media_w > 0.0 && media_h > 0.0 {
        media_w / media_h
    } else {
        w / h
    };
    let factor = if aspect > (w / h) * 1.1 { 1.0 } else { zoom };
    let canvas_w = (h * aspect).max(w) * factor;
    let canvas_h = (w / aspect).max(h) * factor;
    let overflow_x = (canvas_w - w).max(0.0);
    let y = (h - canvas_h) * 0.5;
    WallpaperGeometry {
        x: 0.0,
        y,
        width: canvas_w,
        height: canvas_h,
        overflow_x,
    }
}

/// Interpolates between centered normal mode (`mode_blend = 0`) and zoomed/panned
/// parallax mode (`mode_blend = 1`), matching `WallpaperMath.modeGeometry`.
pub fn compute_mode_geometry(
    viewport_w: f32,
    viewport_h: f32,
    media_w: f32,
    media_h: f32,
    zoom: f32,
    progress: f32,
    mode_blend: f32,
) -> WallpaperGeometry {
    let normal = compute_geometry(viewport_w, viewport_h, media_w, media_h, 1.0);
    let parallax = compute_geometry(viewport_w, viewport_h, media_w, media_h, zoom);
    let t = if mode_blend.is_finite() {
        mode_blend.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let p = if progress.is_finite() {
        progress.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let mix = |a: f32, b: f32| a + (b - a) * t;
    let x = mix(-normal.overflow_x * 0.5, -parallax.overflow_x * p);
    let y = mix(normal.y, parallax.y);
    let width = mix(normal.width, parallax.width);
    let height = mix(normal.height, parallax.height);
    let overflow_x = (width - viewport_w.max(1.0)).max(0.0);
    WallpaperGeometry {
        x,
        y,
        width,
        height,
        overflow_x,
    }
}

#[inline]
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

#[inline]
pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let f = -2.0 * t + 2.0;
        1.0 - f * f * f * 0.5
    }
}

/// Evaluates the cubic-bezier curve `(0.54, 0.0, 0.34, 0.99)` used by
/// `ParallaxWallpaper.qml` and `VideoTransitionLayer.qml` for wallpaper transitions.
pub fn ease_transition_bezier(t: f32) -> f32 {
    let x_target = t.clamp(0.0, 1.0);
    if x_target <= 0.0 {
        return 0.0;
    }
    if x_target >= 1.0 {
        return 1.0;
    }
    cubic_bezier_eval(0.54, 0.0, 0.34, 0.99, x_target)
}

fn cubic_bezier_eval(x1: f32, y1: f32, x2: f32, y2: f32, x_target: f32) -> f32 {
    let sample_x = |u: f32| -> f32 {
        let inv = 1.0 - u;
        3.0 * inv * inv * u * x1 + 3.0 * inv * u * u * x2 + u * u * u
    };
    let sample_dx = |u: f32| -> f32 {
        let inv = 1.0 - u;
        3.0 * inv * inv * x1 + 6.0 * inv * u * (x2 - x1) + 3.0 * u * u * (1.0 - x2)
    };
    let sample_y = |u: f32| -> f32 {
        let inv = 1.0 - u;
        3.0 * inv * inv * u * y1 + 3.0 * inv * u * u * y2 + u * u * u
    };

    let mut lo = 0.0_f32;
    let mut hi = 1.0_f32;
    let mut u = x_target;

    for _ in 0..8 {
        let err = sample_x(u) - x_target;
        if err.abs() < 1e-6 {
            return sample_y(u);
        }
        if err < 0.0 {
            lo = u;
        } else {
            hi = u;
        }
        let dx = sample_dx(u);
        if dx.abs() > 1e-5 {
            let next = u - err / dx;
            if next > lo && next < hi {
                u = next;
                continue;
            }
        }
        u = 0.5 * (lo + hi);
    }

    for _ in 0..12 {
        let err = sample_x(u) - x_target;
        if err.abs() < 1e-6 {
            break;
        }
        if err < 0.0 {
            lo = u;
        } else {
            hi = u;
        }
        u = 0.5 * (lo + hi);
    }
    sample_y(u)
}
