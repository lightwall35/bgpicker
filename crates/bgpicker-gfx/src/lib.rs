//! Unified Wayland `wlr-layer-shell` GPU renderer crate for `bgpicker`.
//!
//! Provides:
//! - Multi-monitor `bgpicker:wallpaper` and `bgpicker:backdrop` layer-shell surfaces
//!   with hotplug and fractional scaling (`wp_fractional_scale_v1` + `wp_viewporter`).
//! - Hardware EGL/OpenGL (ES 3.0 / Desktop 3.3) shader pipeline implementing all
//!   wallpaper transitions (`fade`, `wipe`, `wave`, `grow`, `outer`, `none`), smooth
//!   in-surface `modeBlend` zoom/pan interpolation, and real-time 3-pass Dual-Kawase blur.
//! - Unified [`FrameInput`] and [`TextureSlot`] interface so static images and async
//!   video frames drive the exact same layer pipeline.

pub mod blur;
pub mod egl;
pub mod error;
pub mod frame;
pub mod layer;
pub mod math;
pub mod shaders;
pub mod splash;
pub mod wayland;

pub use blur::DualKawaseBlur;
pub use egl::EglContext;
pub use error::{GfxError, Result};
pub use frame::{FrameInput, PixelFormat, SlotKind, TextureSlot};
pub use layer::{LayerPipeline, LayerRole};
pub use math::{
    KawaseSettings, ParallaxSettings, TransitionEffect, TransitionSpec, WallpaperGeometry,
    compute_geometry, compute_mode_geometry, ease_in_out_cubic, ease_out_cubic,
    ease_transition_bezier,
};
pub use shaders::ShaderPrograms;
pub use splash::StartupSplashSpec;
pub use wayland::{
    FrameModeTargets, LayerTargets, OutputSnapshot, RenderCommand, RendererHandle, SurfaceSnapshot,
    WaylandRenderer, compute_scaled_size,
};
