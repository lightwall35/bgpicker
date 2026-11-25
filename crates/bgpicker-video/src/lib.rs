//! Standalone asynchronous video decoder crate for `bgpicker`.
//!
//! Designed to eliminate Qt6's main-thread CUDA/FFmpeg stall and duplicate-decoder
//! overhead by:
//! 1. Executing all demuxing, hardware device initialization, frame decoding,
//!    seeking, looping, and old-decoder teardown strictly off the render thread.
//! 2. Supporting hardware acceleration (VAAPI zero-copy DRM PRIME DMA-BUF,
//!    VAAPI/CUDA off-thread upload, and software fallback) for a single active
//!    stream shared by both foreground and backdrop surfaces.
//! 3. Providing lock-free wait-free triple buffers with overview-closed backdrop
//!    frame seeding vs. overview-open live streaming.

pub mod error;
mod ffi;
pub mod frame;
pub mod triple_buffer;
pub mod worker;

pub use error::VideoError;
pub use frame::{
    BufferPool, CpuFrame, CpuPixelFormat, CpuPlane, DecodedFrame, DmaBufFrame, DmaBufLayer,
    DmaBufObject, DmaBufPlane, FrameData, FrameTarget, HwAccelPreference, HwBackend, PooledBuffer,
    StreamInfo,
};
pub use triple_buffer::{triple_buffer, TripleConsumer, TripleProducer};
pub use worker::{
    DecoderConfig, FrameConsumer, PlaybackState, PlayerStatusSnapshot, VideoPlayer,
};
