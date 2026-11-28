#ifndef BGPICKER_FFMPEG_SHIM_H
#define BGPICKER_FFMPEG_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define BGV_MAX_PLANES 4
#define BGV_MAX_LAYERS 4

typedef enum BgvHwMode {
    BGV_HW_AUTO = 0,
    BGV_HW_VAAPI_ONLY = 1,
    BGV_HW_CUDA_ONLY = 2,
    BGV_HW_SOFTWARE_ONLY = 3,
    BGV_HW_CUDA_FIRST = 4,
} BgvHwMode;

typedef enum BgvBackendKind {
    BGV_BACKEND_SOFTWARE = 0,
    BGV_BACKEND_VAAPI_DMABUF = 1,
    BGV_BACKEND_VAAPI_UPLOAD = 2,
    BGV_BACKEND_CUDA_UPLOAD = 3,
} BgvBackendKind;

typedef enum BgvCpuFormat {
    BGV_CPU_NV12 = 1,
    BGV_CPU_RGBA8 = 2,
    BGV_CPU_BGRA8 = 3,
} BgvCpuFormat;

typedef enum BgvFrameKind {
    BGV_FRAME_DMABUF = 1,
    BGV_FRAME_CPU = 2,
} BgvFrameKind;

typedef enum BgvStepStatus {
    BGV_STEP_FRAME = 0,
    BGV_STEP_EOF = 1,
    BGV_STEP_HW_ERROR = -2,
    BGV_STEP_ERROR = -1,
} BgvStepStatus;

typedef struct BgvOpenOptions {
    int hw_mode;          /* BgvHwMode */
    int prefer_dmabuf;    /* 1 = export DRM_PRIME on VAAPI when available */
    int cpu_format;       /* BgvCpuFormat */
    int sw_threads;       /* 0 = auto (capped), >0 = explicit */
    const char *vaapi_device; /* optional DRM render node path or NULL */
} BgvOpenOptions;

typedef struct BgvStreamInfo {
    uint32_t width;
    uint32_t height;
    double fps;
    int64_t duration_ms;
    int backend_kind;     /* BgvBackendKind */
    char codec_name[64];
} BgvStreamInfo;

typedef struct BgvDmaBufObject {
    int fd;
    size_t size;
    uint64_t modifier;
} BgvDmaBufObject;

typedef struct BgvDmaBufPlane {
    int object_index;
    uint32_t offset;
    uint32_t pitch;
} BgvDmaBufPlane;

typedef struct BgvDmaBufLayer {
    uint32_t drm_fourcc;
    int nb_planes;
    BgvDmaBufPlane planes[BGV_MAX_PLANES];
} BgvDmaBufLayer;

typedef struct BgvCpuPlane {
    size_t offset;
    uint32_t stride;
    uint32_t rows;
} BgvCpuPlane;

typedef struct BgvFrameOut {
    int kind;             /* BgvFrameKind */
    int backend_kind;     /* BgvBackendKind */
    uint32_t width;
    uint32_t height;
    int64_t pts_ms;
    int64_t duration_ms;

    /* Valid when kind == BGV_FRAME_DMABUF */
    int nb_objects;
    BgvDmaBufObject objects[BGV_MAX_PLANES];
    int nb_layers;
    BgvDmaBufLayer layers[BGV_MAX_LAYERS];

    /* Valid when kind == BGV_FRAME_CPU */
    int cpu_format;       /* BgvCpuFormat */
    int nb_cpu_planes;
    BgvCpuPlane cpu_planes[BGV_MAX_PLANES];
    size_t cpu_total_bytes;
} BgvFrameOut;

typedef uint8_t *(*BgvAllocBufferFn)(void *user_data, size_t required_bytes);

typedef struct BgvDecoder BgvDecoder;

int bgv_open(
    const char *path,
    const BgvOpenOptions *opts,
    BgvDecoder **out_dec,
    BgvStreamInfo *out_info,
    char *err_buf,
    size_t err_buf_len
);

int bgv_decode_next(
    BgvDecoder *dec,
    BgvFrameOut *out_frame,
    BgvAllocBufferFn alloc_fn,
    void *alloc_user_data,
    char *err_buf,
    size_t err_buf_len
);

int bgv_seek_ms(
    BgvDecoder *dec,
    int64_t target_ms,
    char *err_buf,
    size_t err_buf_len
);

void bgv_close(BgvDecoder *dec);

void bgv_set_quiet_logs(void);

#ifdef __cplusplus
}
#endif

#endif
