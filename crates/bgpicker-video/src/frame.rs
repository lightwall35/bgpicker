use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

/// Hardware acceleration preference for opening a video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HwAccelPreference {
    /// Try VAAPI (DMA-BUF zero-copy first, then upload), then CUDA/NVDEC, then software.
    #[default]
    Auto,
    /// Require VAAPI (no fallback to CUDA or software).
    VaapiOnly,
    /// Require CUDA/NVDEC (no fallback to VAAPI or software).
    CudaOnly,
    /// Force multi-threaded software decoding.
    SoftwareOnly,
}

/// Active decoding and frame-transfer backend used by the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwBackend {
    /// VAAPI hardware decode with DRM PRIME DMA-BUF zero-copy export.
    VaapiDmaBuf,
    /// VAAPI hardware decode with CPU pixel transfer upload.
    VaapiUpload,
    /// CUDA/NVDEC hardware decode with off-thread pixel transfer upload.
    CudaUpload,
    /// Software decoding + libswscale pixel conversion.
    Software,
}

/// CPU pixel layout used when DMA-BUF zero-copy is unavailable or disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CpuPixelFormat {
    /// Semi-planar YUV 4:2:0 (Y plane + interleaved UV plane).
    #[default]
    Nv12,
    /// Packed 8-bit RGBA.
    Rgba8,
    /// Packed 8-bit BGRA.
    Bgra8,
}

/// Target consumer role for frame notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameTarget {
    Foreground,
    Backdrop,
    Both,
}

/// Static metadata about the currently active video stream.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration_ms: u64,
    pub backend: HwBackend,
    pub codec_name: String,
}

/// One backing DRM PRIME buffer object.
#[derive(Debug)]
pub struct DmaBufObject {
    pub fd: OwnedFd,
    pub size: usize,
    pub modifier: u64,
}

/// One plane within a DRM PRIME layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaBufPlane {
    pub object_index: usize,
    pub offset: u32,
    pub pitch: u32,
}

/// One DRM PRIME layer (e.g. NV12 single layer, or R8 + GR88 split layers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmaBufLayer {
    pub drm_fourcc: u32,
    pub planes: Vec<DmaBufPlane>,
}

/// Zero-copy GPU-resident video frame exported via DRM PRIME DMA-BUF.
#[derive(Debug)]
pub struct DmaBufFrame {
    pub width: u32,
    pub height: u32,
    pub objects: Vec<DmaBufObject>,
    pub layers: Vec<DmaBufLayer>,
}

/// Plane offset/stride metadata inside a [`CpuFrame`]'s backing buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuPlane {
    pub offset: usize,
    pub stride: u32,
    pub rows: u32,
}

/// Reusable pool of byte buffers to eliminate per-frame heap allocations on CPU fallback paths.
#[derive(Debug)]
pub struct BufferPool {
    buffers: Mutex<Vec<Vec<u8>>>,
    max_cached: usize,
}

impl BufferPool {
    pub fn new(max_cached: usize) -> Arc<Self> {
        Arc::new(Self {
            buffers: Mutex::new(Vec::with_capacity(max_cached)),
            max_cached,
        })
    }

    pub fn acquire(self: &Arc<Self>, required_bytes: usize) -> PooledBuffer {
        let mut buf = self
            .buffers
            .lock()
            .expect("buffer pool mutex poisoned")
            .pop()
            .unwrap_or_default();
        if buf.len() < required_bytes {
            buf.resize(required_bytes, 0);
        }
        PooledBuffer {
            data: Some(buf),
            len: required_bytes,
            pool: Arc::clone(self),
        }
    }

    fn recycle(&self, mut buf: Vec<u8>) {
        let mut guard = self.buffers.lock().expect("buffer pool mutex poisoned");
        if guard.len() < self.max_cached {
            buf.clear();
            guard.push(buf);
        }
    }

    #[cfg(test)]
    pub fn cached_count(&self) -> usize {
        self.buffers.lock().expect("buffer pool mutex poisoned").len()
    }
}

/// Contiguous byte buffer automatically returned to its [`BufferPool`] on drop.
#[derive(Debug)]
pub struct PooledBuffer {
    data: Option<Vec<u8>>,
    len: usize,
    pool: Arc<BufferPool>,
}

impl PooledBuffer {
    pub fn as_slice(&self) -> &[u8] {
        let vec = self.data.as_ref().expect("buffer taken");
        &vec[..self.len]
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        let vec = self.data.as_mut().expect("buffer taken");
        &mut vec[..self.len]
    }
}

impl Drop for PooledBuffer {
    fn drop(&mut self) {
        if let Some(buf) = self.data.take() {
            self.pool.recycle(buf);
        }
    }
}

/// CPU-accessible video frame ready for direct texture upload (NV12, RGBA8, or BGRA8).
#[derive(Debug)]
pub struct CpuFrame {
    pub format: CpuPixelFormat,
    pub width: u32,
    pub height: u32,
    pub planes: Vec<CpuPlane>,
    pub data: PooledBuffer,
}

impl CpuFrame {
    /// Returns the packed bytes for the whole frame.
    pub fn as_bytes(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// Returns the slice for plane `index` (e.g., 0 = Y, 1 = UV for NV12).
    pub fn plane_bytes(&self, index: usize) -> Option<&[u8]> {
        let plane = self.planes.get(index)?;
        let size = (plane.stride as usize).saturating_mul(plane.rows as usize);
        let end = plane.offset.checked_add(size)?;
        self.data.as_slice().get(plane.offset..end)
    }
}

/// Pixel storage backing a decoded frame.
#[derive(Debug)]
pub enum FrameData {
    DmaBuf(DmaBufFrame),
    Cpu(CpuFrame),
}

/// One decoded frame shared via `Arc<DecodedFrame>` between foreground and backdrop consumers.
#[derive(Debug)]
pub struct DecodedFrame {
    pub stream_id: u64,
    pub frame_seq: u64,
    pub pts_ms: u64,
    pub duration_ms: u64,
    pub backend: HwBackend,
    pub data: FrameData,
}

impl DecodedFrame {
    pub fn dimensions(&self) -> (u32, u32) {
        match &self.data {
            FrameData::DmaBuf(d) => (d.width, d.height),
            FrameData::Cpu(c) => (c.width, c.height),
        }
    }

    pub fn is_dmabuf(&self) -> bool {
        matches!(self.data, FrameData::DmaBuf(_))
    }
}
