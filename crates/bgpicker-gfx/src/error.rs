use thiserror::Error;

/// Errors returned by the Wayland GPU renderer and shader pipeline.
#[derive(Debug, Error)]
pub enum GfxError {
    #[error("failed to connect to Wayland display: {0}")]
    WaylandConnect(String),

    #[error("Wayland global/registry error: {0}")]
    WaylandGlobal(String),

    #[error("Wayland dispatch error: {0}")]
    WaylandDispatch(String),

    #[error("failed to load or initialize EGL: {0}")]
    Egl(String),

    #[error("failed to create EGL window surface: {0}")]
    EglSurface(String),

    #[error("OpenGL error during {context}: {detail}")]
    Gl { context: &'static str, detail: String },

    #[error("shader compilation/linking failed ({stage}): {log}")]
    Shader { stage: &'static str, log: String },

    #[error("failed to decode image '{path}': {detail}")]
    ImageDecode { path: String, detail: String },

    #[error("invalid frame input: {0}")]
    InvalidFrame(String),

    #[error("renderer channel closed")]
    ChannelClosed,
}

pub type Result<T> = std::result::Result<T, GfxError>;
