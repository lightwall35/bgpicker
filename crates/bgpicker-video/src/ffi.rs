use std::ffi::{CStr, CString};
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::Path;
use std::ptr;
use std::sync::Arc;

use libc::{c_char, c_double, c_int, c_void};

use crate::error::VideoError;
use crate::frame::{
    BufferPool, CpuFrame, CpuPixelFormat, CpuPlane, DecodedFrame, DmaBufFrame, DmaBufLayer,
    DmaBufObject, DmaBufPlane, FrameData, HwAccelPreference, HwBackend, PooledBuffer, StreamInfo,
};

const BGV_MAX_PLANES: usize = 4;
const BGV_MAX_LAYERS: usize = 4;

const BGV_HW_AUTO: c_int = 0;
const BGV_HW_VAAPI_ONLY: c_int = 1;
const BGV_HW_CUDA_ONLY: c_int = 2;
const BGV_HW_SOFTWARE_ONLY: c_int = 3;

const BGV_BACKEND_SOFTWARE: c_int = 0;
const BGV_BACKEND_VAAPI_DMABUF: c_int = 1;
const BGV_BACKEND_VAAPI_UPLOAD: c_int = 2;
const BGV_BACKEND_CUDA_UPLOAD: c_int = 3;

const BGV_CPU_NV12: c_int = 1;
const BGV_CPU_RGBA8: c_int = 2;
const BGV_CPU_BGRA8: c_int = 3;

const BGV_FRAME_DMABUF: c_int = 1;
const BGV_FRAME_CPU: c_int = 2;

const BGV_STEP_FRAME: c_int = 0;
const BGV_STEP_EOF: c_int = 1;
const BGV_STEP_HW_ERROR: c_int = -2;

#[repr(C)]
struct BgvDecoderOpaque {
    _unused: [u8; 0],
}

#[repr(C)]
struct BgvOpenOptions {
    hw_mode: c_int,
    prefer_dmabuf: c_int,
    cpu_format: c_int,
    sw_threads: c_int,
    vaapi_device: *const c_char,
}

#[repr(C)]
struct BgvStreamInfo {
    width: u32,
    height: u32,
    fps: c_double,
    duration_ms: i64,
    backend_kind: c_int,
    codec_name: [c_char; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BgvDmaBufObject {
    fd: c_int,
    size: usize,
    modifier: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BgvDmaBufPlane {
    object_index: c_int,
    offset: u32,
    pitch: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BgvDmaBufLayer {
    drm_fourcc: u32,
    nb_planes: c_int,
    planes: [BgvDmaBufPlane; BGV_MAX_PLANES],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BgvCpuPlane {
    offset: usize,
    stride: u32,
    rows: u32,
}

#[repr(C)]
struct BgvFrameOut {
    kind: c_int,
    backend_kind: c_int,
    width: u32,
    height: u32,
    pts_ms: i64,
    duration_ms: i64,
    nb_objects: c_int,
    objects: [BgvDmaBufObject; BGV_MAX_PLANES],
    nb_layers: c_int,
    layers: [BgvDmaBufLayer; BGV_MAX_LAYERS],
    cpu_format: c_int,
    nb_cpu_planes: c_int,
    cpu_planes: [BgvCpuPlane; BGV_MAX_PLANES],
    cpu_total_bytes: usize,
}

impl Default for BgvFrameOut {
    fn default() -> Self {
        // SAFETY: All fields are plain integers/arrays where zero is valid.
        unsafe { std::mem::zeroed() }
    }
}

type BgvAllocBufferFn = Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut u8>;

extern "C" {
    fn bgv_open(
        path: *const c_char,
        opts: *const BgvOpenOptions,
        out_dec: *mut *mut BgvDecoderOpaque,
        out_info: *mut BgvStreamInfo,
        err_buf: *mut c_char,
        err_buf_len: usize,
    ) -> c_int;

    fn bgv_decode_next(
        dec: *mut BgvDecoderOpaque,
        out_frame: *mut BgvFrameOut,
        alloc_fn: BgvAllocBufferFn,
        alloc_user_data: *mut c_void,
        err_buf: *mut c_char,
        err_buf_len: usize,
    ) -> c_int;

    fn bgv_seek_ms(
        dec: *mut BgvDecoderOpaque,
        target_ms: i64,
        err_buf: *mut c_char,
        err_buf_len: usize,
    ) -> c_int;

    fn bgv_close(dec: *mut BgvDecoderOpaque);
}

struct AllocContext<'a> {
    pool: &'a Arc<BufferPool>,
    buf: Option<PooledBuffer>,
}

unsafe extern "C" fn alloc_buffer_cb(user_data: *mut c_void, required_bytes: usize) -> *mut u8 {
    let ctx = &mut *(user_data as *mut AllocContext<'_>);
    let mut pooled = ctx.pool.acquire(required_bytes);
    let ptr = pooled.as_mut_slice().as_mut_ptr();
    ctx.buf = Some(pooled);
    ptr
}

pub enum DecodeStep {
    Frame(DecodedFrame),
    Eof,
}

pub struct RawOpenParams {
    pub hw_accel: HwAccelPreference,
    pub prefer_dmabuf: bool,
    pub cpu_format: CpuPixelFormat,
    pub sw_threads: usize,
    pub vaapi_device: Option<String>,
}

/// Owned FFmpeg decoder session. Must only be created, used, and dropped off the render thread.
pub struct RawDecoder {
    ptr: *mut BgvDecoderOpaque,
    pub info: StreamInfo,
}

// SAFETY: RawDecoder owns all FFmpeg structs exclusively and is never shared concurrently;
// transferring it between worker and teardown threads is safe once the worker releases it.
unsafe impl Send for RawDecoder {}

fn backend_from_c(kind: c_int) -> HwBackend {
    match kind {
        BGV_BACKEND_VAAPI_DMABUF => HwBackend::VaapiDmaBuf,
        BGV_BACKEND_VAAPI_UPLOAD => HwBackend::VaapiUpload,
        BGV_BACKEND_CUDA_UPLOAD => HwBackend::CudaUpload,
        BGV_BACKEND_SOFTWARE => HwBackend::Software,
        _ => HwBackend::Software,
    }
}

fn c_err_string(buf: &[c_char]) -> String {
    // SAFETY: `buf` is nul-terminated by C `snprintf`.
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr()) };
    cstr.to_string_lossy().into_owned()
}

impl RawDecoder {
    pub fn open(path: &Path, params: &RawOpenParams) -> Result<Self, VideoError> {
        let path_str = path
            .to_str()
            .ok_or_else(|| VideoError::InvalidPath(path.display().to_string()))?;
        let c_path =
            CString::new(path_str).map_err(|e| VideoError::InvalidPath(e.to_string()))?;

        let c_vaapi = match &params.vaapi_device {
            Some(dev) => Some(CString::new(dev.as_str()).map_err(|e| VideoError::InvalidPath(e.to_string()))?),
            None => None,
        };

        let opts = BgvOpenOptions {
            hw_mode: match params.hw_accel {
                HwAccelPreference::Auto => BGV_HW_AUTO,
                HwAccelPreference::VaapiOnly => BGV_HW_VAAPI_ONLY,
                HwAccelPreference::CudaOnly => BGV_HW_CUDA_ONLY,
                HwAccelPreference::SoftwareOnly => BGV_HW_SOFTWARE_ONLY,
            },
            prefer_dmabuf: i32::from(params.prefer_dmabuf),
            cpu_format: match params.cpu_format {
                CpuPixelFormat::Nv12 => BGV_CPU_NV12,
                CpuPixelFormat::Rgba8 => BGV_CPU_RGBA8,
                CpuPixelFormat::Bgra8 => BGV_CPU_BGRA8,
            },
            sw_threads: params.sw_threads as c_int,
            vaapi_device: c_vaapi.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        };

        let mut dec_ptr: *mut BgvDecoderOpaque = ptr::null_mut();
        let mut raw_info = BgvStreamInfo {
            width: 0,
            height: 0,
            fps: 0.0,
            duration_ms: 0,
            backend_kind: 0,
            codec_name: [0; 64],
        };
        let mut err_buf = [0 as c_char; 512];

        let rc = unsafe {
            bgv_open(
                c_path.as_ptr(),
                &opts,
                &mut dec_ptr,
                &mut raw_info,
                err_buf.as_mut_ptr(),
                err_buf.len(),
            )
        };
        if rc != 0 || dec_ptr.is_null() {
            return Err(VideoError::Backend(c_err_string(&err_buf)));
        }

        let codec_name = unsafe { CStr::from_ptr(raw_info.codec_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();

        let info = StreamInfo {
            width: raw_info.width,
            height: raw_info.height,
            fps: raw_info.fps,
            duration_ms: raw_info.duration_ms.max(0) as u64,
            backend: backend_from_c(raw_info.backend_kind),
            codec_name,
        };

        Ok(Self { ptr: dec_ptr, info })
    }

    pub fn decode_next(
        &mut self,
        stream_id: u64,
        frame_seq: u64,
        pool: &Arc<BufferPool>,
    ) -> Result<DecodeStep, VideoError> {
        let mut out = BgvFrameOut::default();
        let mut alloc_ctx = AllocContext { pool, buf: None };
        let mut err_buf = [0 as c_char; 512];

        let rc = unsafe {
            bgv_decode_next(
                self.ptr,
                &mut out,
                Some(alloc_buffer_cb),
                (&mut alloc_ctx as *mut AllocContext<'_>).cast::<c_void>(),
                err_buf.as_mut_ptr(),
                err_buf.len(),
            )
        };

        if rc == BGV_STEP_EOF {
            return Ok(DecodeStep::Eof);
        }
        if rc == BGV_STEP_HW_ERROR {
            return Err(VideoError::HardwareFallback(c_err_string(&err_buf)));
        }
        if rc != BGV_STEP_FRAME {
            return Err(VideoError::Backend(c_err_string(&err_buf)));
        }

        let backend = backend_from_c(out.backend_kind);
        self.info.backend = backend;

        let data = match out.kind {
            BGV_FRAME_DMABUF => {
                let nb_obj = (out.nb_objects as usize).min(BGV_MAX_PLANES);
                let mut objects = Vec::with_capacity(nb_obj);
                for obj in &out.objects[..nb_obj] {
                    // SAFETY: `obj.fd` was freshly duplicated with `F_DUPFD_CLOEXEC` by C shim.
                    let owned_fd = unsafe { OwnedFd::from_raw_fd(obj.fd) };
                    objects.push(DmaBufObject {
                        fd: owned_fd,
                        size: obj.size,
                        modifier: obj.modifier,
                    });
                }

                let nb_layers = (out.nb_layers as usize).min(BGV_MAX_LAYERS);
                let mut layers = Vec::with_capacity(nb_layers);
                for layer in &out.layers[..nb_layers] {
                    let nb_planes = (layer.nb_planes as usize).min(BGV_MAX_PLANES);
                    let mut planes = Vec::with_capacity(nb_planes);
                    for p in &layer.planes[..nb_planes] {
                        planes.push(DmaBufPlane {
                            object_index: p.object_index as usize,
                            offset: p.offset,
                            pitch: p.pitch,
                        });
                    }
                    layers.push(DmaBufLayer {
                        drm_fourcc: layer.drm_fourcc,
                        planes,
                    });
                }

                FrameData::DmaBuf(DmaBufFrame {
                    width: out.width,
                    height: out.height,
                    objects,
                    layers,
                })
            }
            BGV_FRAME_CPU => {
                let data = alloc_ctx.buf.take().ok_or_else(|| {
                    VideoError::Backend("missing CPU frame buffer from allocator".into())
                })?;
                let format = match out.cpu_format {
                    BGV_CPU_RGBA8 => CpuPixelFormat::Rgba8,
                    BGV_CPU_BGRA8 => CpuPixelFormat::Bgra8,
                    _ => CpuPixelFormat::Nv12,
                };
                let nb_planes = (out.nb_cpu_planes as usize).min(BGV_MAX_PLANES);
                let planes = out.cpu_planes[..nb_planes]
                    .iter()
                    .map(|p| CpuPlane {
                        offset: p.offset,
                        stride: p.stride,
                        rows: p.rows,
                    })
                    .collect();

                FrameData::Cpu(CpuFrame {
                    format,
                    width: out.width,
                    height: out.height,
                    planes,
                    data,
                })
            }
            other => {
                return Err(VideoError::Backend(format!(
                    "unexpected frame kind from C shim: {other}"
                )));
            }
        };

        Ok(DecodeStep::Frame(DecodedFrame {
            stream_id,
            frame_seq,
            pts_ms: out.pts_ms.max(0) as u64,
            duration_ms: out.duration_ms.max(1) as u64,
            backend,
            data,
        }))
    }

    pub fn seek_ms(&mut self, target_ms: u64) -> Result<(), VideoError> {
        let mut err_buf = [0 as c_char; 512];
        let rc = unsafe {
            bgv_seek_ms(
                self.ptr,
                target_ms as i64,
                err_buf.as_mut_ptr(),
                err_buf.len(),
            )
        };
        if rc != 0 {
            return Err(VideoError::Backend(c_err_string(&err_buf)));
        }
        Ok(())
    }
}

impl Drop for RawDecoder {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { bgv_close(self.ptr) };
            self.ptr = ptr::null_mut();
        }
    }
}
