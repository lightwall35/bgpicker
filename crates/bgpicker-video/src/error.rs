use thiserror::Error;

/// Errors returned by the async video decoder crate.
#[derive(Debug, Error)]
pub enum VideoError {
    #[error("invalid media path: {0}")]
    InvalidPath(String),

    #[error("failed to spawn worker thread: {0}")]
    ThreadSpawn(#[from] std::io::Error),

    #[error("decoder backend error: {0}")]
    Backend(String),

    #[error("hardware decode failed and requires software fallback: {0}")]
    HardwareFallback(String),

    #[error("video worker has shut down")]
    WorkerStopped,
}
