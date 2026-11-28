#define _POSIX_C_SOURCE 200809L

#include "ffmpeg_shim.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <drm_fourcc.h>
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/avutil.h>
#include <libavutil/error.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_drm.h>
#include <libavutil/imgutils.h>
#include <libavutil/log.h>
#include <libavutil/pixdesc.h>
#include <libswscale/swscale.h>

struct BgvDecoder {
    AVFormatContext *fmt_ctx;
    AVCodecContext *codec_ctx;
    AVBufferRef *hw_device_ctx;
    enum AVPixelFormat hw_pix_fmt;
    enum AVHWDeviceType hw_device_type;
    int stream_index;
    AVRational time_base;
    double fps;
    int64_t duration_ms;
    int64_t start_time;

    AVPacket *pkt;
    AVFrame *frame;
    AVFrame *sw_frame;
    AVFrame *drm_frame;
    struct SwsContext *sws_ctx;
    int sws_w;
    int sws_h;
    enum AVPixelFormat sws_src_fmt;
    enum AVPixelFormat sws_dst_fmt;

    int allow_dmabuf;
    int cpu_format;
    int backend_kind;
    int draining;
    int eof_reached;
    int decoded_any_hw_frame;
};

static pthread_once_t g_log_once = PTHREAD_ONCE_INIT;

static void init_quiet_log(void) {
    av_log_set_level(AV_LOG_ERROR);
}

void bgv_set_quiet_logs(void) {
    pthread_once(&g_log_once, init_quiet_log);
}

static void write_err(char *err_buf, size_t err_buf_len, const char *prefix, int av_err) {
    if (!err_buf || err_buf_len == 0) {
        return;
    }
    if (av_err != 0) {
        char av_msg[128];
        av_strerror(av_err, av_msg, sizeof(av_msg));
        snprintf(err_buf, err_buf_len, "%s: %s", prefix, av_msg);
    } else {
        snprintf(err_buf, err_buf_len, "%s", prefix);
    }
}

static enum AVPixelFormat bgv_get_hw_format(AVCodecContext *ctx, const enum AVPixelFormat *pix_fmts) {
    struct BgvDecoder *dec = (struct BgvDecoder *)ctx->opaque;
    if (dec && dec->hw_pix_fmt != AV_PIX_FMT_NONE) {
        for (const enum AVPixelFormat *p = pix_fmts; *p != AV_PIX_FMT_NONE; p++) {
            if (*p == dec->hw_pix_fmt) {
                return *p;
            }
        }
    }
    for (const enum AVPixelFormat *p = pix_fmts; *p != AV_PIX_FMT_NONE; p++) {
        const AVPixFmtDescriptor *desc = av_pix_fmt_desc_get(*p);
        if (desc && !(desc->flags & AV_PIX_FMT_FLAG_HWACCEL)) {
            return *p;
        }
    }
    return pix_fmts[0];
}

static enum AVPixelFormat find_hw_pix_fmt(const AVCodec *codec, enum AVHWDeviceType type) {
    for (int i = 0;; i++) {
        const AVCodecHWConfig *config = avcodec_get_hw_config(codec, i);
        if (!config) {
            return AV_PIX_FMT_NONE;
        }
        if ((config->methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX) &&
            config->device_type == type) {
            return config->pix_fmt;
        }
    }
}

static int try_create_vaapi_device(const char *explicit_dev, AVBufferRef **out_ctx) {
    if (explicit_dev && explicit_dev[0] != '\0') {
        return av_hwdevice_ctx_create(out_ctx, AV_HWDEVICE_TYPE_VAAPI, explicit_dev, NULL, 0);
    }
    const char *candidates[] = {
        "/dev/dri/renderD128",
        "/dev/dri/renderD129",
        NULL
    };
    int last_err = AVERROR(ENODEV);
    for (size_t i = 0; i < sizeof(candidates) / sizeof(candidates[0]); i++) {
        if (candidates[i] != NULL && access(candidates[i], R_OK | W_OK) != 0) {
            continue;
        }
        int err = av_hwdevice_ctx_create(out_ctx, AV_HWDEVICE_TYPE_VAAPI, candidates[i], NULL, 0);
        if (err >= 0) {
            return 0;
        }
        last_err = err;
    }
    return last_err;
}

static enum AVPixelFormat target_av_cpu_fmt(int cpu_format) {
    switch (cpu_format) {
    case BGV_CPU_RGBA8:
        return AV_PIX_FMT_RGBA;
    case BGV_CPU_BGRA8:
        return AV_PIX_FMT_BGRA;
    case BGV_CPU_NV12:
    default:
        return AV_PIX_FMT_NV12;
    }
}

static int open_codec_with_backend(
    struct BgvDecoder *dec,
    AVStream *stream,
    const AVCodec *codec,
    enum AVHWDeviceType hw_type,
    const BgvOpenOptions *opts,
    char *err_buf,
    size_t err_buf_len
) {
    AVCodecContext *ctx = avcodec_alloc_context3(codec);
    if (!ctx) {
        write_err(err_buf, err_buf_len, "avcodec_alloc_context3 failed", AVERROR(ENOMEM));
        return AVERROR(ENOMEM);
    }

    int ret = avcodec_parameters_to_context(ctx, stream->codecpar);
    if (ret < 0) {
        write_err(err_buf, err_buf_len, "avcodec_parameters_to_context failed", ret);
        avcodec_free_context(&ctx);
        return ret;
    }

    AVBufferRef *hw_ctx = NULL;
    enum AVPixelFormat hw_fmt = AV_PIX_FMT_NONE;

    if (hw_type != AV_HWDEVICE_TYPE_NONE) {
        hw_fmt = find_hw_pix_fmt(codec, hw_type);
        if (hw_fmt == AV_PIX_FMT_NONE) {
            write_err(err_buf, err_buf_len, "codec does not support requested hwaccel", AVERROR(ENOSYS));
            avcodec_free_context(&ctx);
            return AVERROR(ENOSYS);
        }

        if (hw_type == AV_HWDEVICE_TYPE_VAAPI) {
            ret = try_create_vaapi_device(opts ? opts->vaapi_device : NULL, &hw_ctx);
        } else {
            ret = av_hwdevice_ctx_create(&hw_ctx, hw_type, NULL, NULL, 0);
        }
        if (ret < 0) {
            write_err(err_buf, err_buf_len, "av_hwdevice_ctx_create failed", ret);
            avcodec_free_context(&ctx);
            return ret;
        }

        dec->hw_pix_fmt = hw_fmt;
        dec->hw_device_type = hw_type;
        ctx->opaque = dec;
        ctx->get_format = bgv_get_hw_format;
        ctx->hw_device_ctx = av_buffer_ref(hw_ctx);
        if (!ctx->hw_device_ctx) {
            av_buffer_unref(&hw_ctx);
            avcodec_free_context(&ctx);
            return AVERROR(ENOMEM);
        }
    } else {
        dec->hw_pix_fmt = AV_PIX_FMT_NONE;
        dec->hw_device_type = AV_HWDEVICE_TYPE_NONE;
        int threads = (opts && opts->sw_threads > 0) ? opts->sw_threads : 4;
        ctx->thread_count = threads;
        ctx->thread_type = FF_THREAD_FRAME | FF_THREAD_SLICE;
    }

    ret = avcodec_open2(ctx, codec, NULL);
    if (ret < 0) {
        write_err(err_buf, err_buf_len, "avcodec_open2 failed", ret);
        av_buffer_unref(&hw_ctx);
        avcodec_free_context(&ctx);
        return ret;
    }

    dec->codec_ctx = ctx;
    dec->hw_device_ctx = hw_ctx;
    if (hw_type == AV_HWDEVICE_TYPE_VAAPI) {
        dec->allow_dmabuf = (opts && opts->prefer_dmabuf) ? 1 : 0;
        dec->backend_kind = dec->allow_dmabuf ? BGV_BACKEND_VAAPI_DMABUF : BGV_BACKEND_VAAPI_UPLOAD;
    } else if (hw_type == AV_HWDEVICE_TYPE_CUDA) {
        dec->allow_dmabuf = 0;
        dec->backend_kind = BGV_BACKEND_CUDA_UPLOAD;
    } else {
        dec->allow_dmabuf = 0;
        dec->backend_kind = BGV_BACKEND_SOFTWARE;
    }
    return 0;
}

int bgv_open(
    const char *path,
    const BgvOpenOptions *opts,
    BgvDecoder **out_dec,
    BgvStreamInfo *out_info,
    char *err_buf,
    size_t err_buf_len
) {
    if (!path || !out_dec || !out_info) {
        write_err(err_buf, err_buf_len, "invalid arguments to bgv_open", 0);
        return -1;
    }
    *out_dec = NULL;
    memset(out_info, 0, sizeof(*out_info));
    bgv_set_quiet_logs();

    struct BgvDecoder *dec = (struct BgvDecoder *)calloc(1, sizeof(struct BgvDecoder));
    if (!dec) {
        write_err(err_buf, err_buf_len, "out of memory", AVERROR(ENOMEM));
        return -1;
    }
    dec->stream_index = -1;
    dec->cpu_format = (opts && opts->cpu_format >= BGV_CPU_NV12 && opts->cpu_format <= BGV_CPU_BGRA8)
        ? opts->cpu_format
        : BGV_CPU_NV12;

    int ret = avformat_open_input(&dec->fmt_ctx, path, NULL, NULL);
    if (ret < 0) {
        write_err(err_buf, err_buf_len, "avformat_open_input failed", ret);
        bgv_close(dec);
        return -1;
    }

    ret = avformat_find_stream_info(dec->fmt_ctx, NULL);
    if (ret < 0) {
        write_err(err_buf, err_buf_len, "avformat_find_stream_info failed", ret);
        bgv_close(dec);
        return -1;
    }

    const AVCodec *codec = NULL;
    int stream_idx = av_find_best_stream(dec->fmt_ctx, AVMEDIA_TYPE_VIDEO, -1, -1, &codec, 0);
    if (stream_idx < 0 || !codec) {
        write_err(err_buf, err_buf_len, "no video stream found", stream_idx < 0 ? stream_idx : 0);
        bgv_close(dec);
        return -1;
    }

    dec->stream_index = stream_idx;
    AVStream *stream = dec->fmt_ctx->streams[stream_idx];
    dec->time_base = stream->time_base;
    dec->start_time = (stream->start_time != AV_NOPTS_VALUE) ? stream->start_time : 0;

    AVRational fr = av_guess_frame_rate(dec->fmt_ctx, stream, NULL);
    if (fr.num > 0 && fr.den > 0) {
        dec->fps = av_q2d(fr);
    } else if (stream->avg_frame_rate.num > 0 && stream->avg_frame_rate.den > 0) {
        dec->fps = av_q2d(stream->avg_frame_rate);
    } else {
        dec->fps = 30.0;
    }

    if (stream->duration != AV_NOPTS_VALUE) {
        dec->duration_ms = av_rescale_q(stream->duration, stream->time_base, (AVRational){1, 1000});
    } else if (dec->fmt_ctx->duration != AV_NOPTS_VALUE) {
        dec->duration_ms = dec->fmt_ctx->duration / (AV_TIME_BASE / 1000);
    } else {
        dec->duration_ms = 0;
    }

    int hw_mode = opts ? opts->hw_mode : BGV_HW_AUTO;
    enum AVHWDeviceType attempts[3];
    int nb_attempts = 0;

    if (hw_mode == BGV_HW_VAAPI_ONLY) {
        attempts[nb_attempts++] = AV_HWDEVICE_TYPE_VAAPI;
    } else if (hw_mode == BGV_HW_CUDA_ONLY) {
        attempts[nb_attempts++] = AV_HWDEVICE_TYPE_CUDA;
    } else if (hw_mode == BGV_HW_SOFTWARE_ONLY) {
        attempts[nb_attempts++] = AV_HWDEVICE_TYPE_NONE;
    } else if (hw_mode == BGV_HW_CUDA_FIRST) {
        attempts[nb_attempts++] = AV_HWDEVICE_TYPE_CUDA;
        attempts[nb_attempts++] = AV_HWDEVICE_TYPE_VAAPI;
        attempts[nb_attempts++] = AV_HWDEVICE_TYPE_NONE;
    } else {
        attempts[nb_attempts++] = AV_HWDEVICE_TYPE_VAAPI;
        attempts[nb_attempts++] = AV_HWDEVICE_TYPE_CUDA;
        attempts[nb_attempts++] = AV_HWDEVICE_TYPE_NONE;
    }

    int opened = 0;
    char attempt_err[256] = {0};
    for (int i = 0; i < nb_attempts; i++) {
        if (open_codec_with_backend(dec, stream, codec, attempts[i], opts, attempt_err, sizeof(attempt_err)) == 0) {
            opened = 1;
            break;
        }
    }

    if (!opened) {
        write_err(err_buf, err_buf_len, attempt_err[0] ? attempt_err : "failed to open any decoder backend", 0);
        bgv_close(dec);
        return -1;
    }

    dec->pkt = av_packet_alloc();
    dec->frame = av_frame_alloc();
    dec->sw_frame = av_frame_alloc();
    dec->drm_frame = av_frame_alloc();
    if (!dec->pkt || !dec->frame || !dec->sw_frame || !dec->drm_frame) {
        write_err(err_buf, err_buf_len, "failed to allocate AVPacket/AVFrame", AVERROR(ENOMEM));
        bgv_close(dec);
        return -1;
    }

    out_info->width = (uint32_t)(stream->codecpar->width > 0 ? stream->codecpar->width : dec->codec_ctx->width);
    out_info->height = (uint32_t)(stream->codecpar->height > 0 ? stream->codecpar->height : dec->codec_ctx->height);
    out_info->fps = dec->fps;
    out_info->duration_ms = dec->duration_ms;
    out_info->backend_kind = dec->backend_kind;
    if (codec->name) {
        snprintf(out_info->codec_name, sizeof(out_info->codec_name), "%s", codec->name);
    }

    *out_dec = dec;
    return 0;
}

static int export_dmabuf(struct BgvDecoder *dec, AVFrame *hw_frame, BgvFrameOut *out) {
    av_frame_unref(dec->drm_frame);
    dec->drm_frame->format = AV_PIX_FMT_DRM_PRIME;

    int ret = av_hwframe_map(dec->drm_frame, hw_frame, AV_HWFRAME_MAP_READ);
    if (ret < 0 || !dec->drm_frame->data[0]) {
        av_frame_unref(dec->drm_frame);
        return -1;
    }

    const AVDRMFrameDescriptor *desc = (const AVDRMFrameDescriptor *)dec->drm_frame->data[0];
    if (desc->nb_objects <= 0 || desc->nb_objects > BGV_MAX_PLANES ||
        desc->nb_layers <= 0 || desc->nb_layers > BGV_MAX_LAYERS) {
        av_frame_unref(dec->drm_frame);
        return -1;
    }

    int dup_fds[BGV_MAX_PLANES] = {-1, -1, -1, -1};
    for (int i = 0; i < desc->nb_objects; i++) {
        if (desc->objects[i].fd < 0) {
            for (int j = 0; j < i; j++) {
                if (dup_fds[j] >= 0) close(dup_fds[j]);
            }
            av_frame_unref(dec->drm_frame);
            return -1;
        }
        int dupfd = fcntl(desc->objects[i].fd, F_DUPFD_CLOEXEC, 0);
        if (dupfd < 0) {
            for (int j = 0; j < i; j++) {
                if (dup_fds[j] >= 0) close(dup_fds[j]);
            }
            av_frame_unref(dec->drm_frame);
            return -1;
        }
        dup_fds[i] = dupfd;
    }

    out->kind = BGV_FRAME_DMABUF;
    out->backend_kind = BGV_BACKEND_VAAPI_DMABUF;
    out->width = (uint32_t)hw_frame->width;
    out->height = (uint32_t)hw_frame->height;
    out->nb_objects = desc->nb_objects;
    for (int i = 0; i < desc->nb_objects; i++) {
        out->objects[i].fd = dup_fds[i];
        out->objects[i].size = desc->objects[i].size;
        out->objects[i].modifier = desc->objects[i].format_modifier;
    }

    out->nb_layers = desc->nb_layers;
    for (int l = 0; l < desc->nb_layers; l++) {
        const AVDRMLayerDescriptor *layer = &desc->layers[l];
        int nb_planes = layer->nb_planes;
        if (nb_planes > BGV_MAX_PLANES) nb_planes = BGV_MAX_PLANES;
        out->layers[l].drm_fourcc = layer->format;
        out->layers[l].nb_planes = nb_planes;
        for (int p = 0; p < nb_planes; p++) {
            out->layers[l].planes[p].object_index = layer->planes[p].object_index;
            out->layers[l].planes[p].offset = (uint32_t)layer->planes[p].offset;
            out->layers[l].planes[p].pitch = (uint32_t)layer->planes[p].pitch;
        }
    }

    av_frame_unref(dec->drm_frame);
    return 0;
}

static int export_cpu_frame(
    struct BgvDecoder *dec,
    AVFrame *src,
    BgvFrameOut *out,
    BgvAllocBufferFn alloc_fn,
    void *alloc_user_data,
    char *err_buf,
    size_t err_buf_len
) {
    if (!alloc_fn) {
        write_err(err_buf, err_buf_len, "missing buffer allocator callback", 0);
        return -1;
    }

    uint32_t w = (uint32_t)src->width;
    uint32_t h = (uint32_t)src->height;
    enum AVPixelFormat dst_fmt = target_av_cpu_fmt(dec->cpu_format);

    if (dec->cpu_format == BGV_CPU_NV12) {
        uint32_t y_stride = w;
        uint32_t uv_rows = (h + 1) / 2;
        size_t y_bytes = (size_t)y_stride * (size_t)h;
        size_t uv_bytes = (size_t)y_stride * (size_t)uv_rows;
        size_t total = y_bytes + uv_bytes;
        uint8_t *dst = alloc_fn(alloc_user_data, total);
        if (!dst) {
            write_err(err_buf, err_buf_len, "buffer allocation returned null", 0);
            return -1;
        }

        if ((enum AVPixelFormat)src->format == AV_PIX_FMT_NV12 && src->data[0] && src->data[1]) {
            for (uint32_t r = 0; r < h; r++) {
                memcpy(dst + (size_t)r * y_stride,
                       src->data[0] + (size_t)r * (size_t)src->linesize[0],
                       w);
            }
            uint8_t *uv_dst = dst + y_bytes;
            for (uint32_t r = 0; r < uv_rows; r++) {
                memcpy(uv_dst + (size_t)r * y_stride,
                       src->data[1] + (size_t)r * (size_t)src->linesize[1],
                       w);
            }
        } else {
            dec->sws_ctx = sws_getCachedContext(
                dec->sws_ctx,
                (int)w, (int)h, (enum AVPixelFormat)src->format,
                (int)w, (int)h, AV_PIX_FMT_NV12,
                SWS_BILINEAR, NULL, NULL, NULL
            );
            if (!dec->sws_ctx) {
                write_err(err_buf, err_buf_len, "sws_getCachedContext failed for NV12", 0);
                return -1;
            }
            uint8_t *dst_data[4] = { dst, dst + y_bytes, NULL, NULL };
            int dst_linesize[4] = { (int)y_stride, (int)y_stride, 0, 0 };
            sws_scale(dec->sws_ctx, (const uint8_t *const *)src->data, src->linesize,
                      0, (int)h, dst_data, dst_linesize);
        }

        out->kind = BGV_FRAME_CPU;
        out->backend_kind = dec->backend_kind;
        out->width = w;
        out->height = h;
        out->cpu_format = BGV_CPU_NV12;
        out->nb_cpu_planes = 2;
        out->cpu_planes[0].offset = 0;
        out->cpu_planes[0].stride = y_stride;
        out->cpu_planes[0].rows = h;
        out->cpu_planes[1].offset = y_bytes;
        out->cpu_planes[1].stride = y_stride;
        out->cpu_planes[1].rows = uv_rows;
        out->cpu_total_bytes = total;
        return 0;
    }

    /* RGBA8 or BGRA8 packed */
    uint32_t stride = w * 4;
    size_t total = (size_t)stride * (size_t)h;
    uint8_t *dst = alloc_fn(alloc_user_data, total);
    if (!dst) {
        write_err(err_buf, err_buf_len, "buffer allocation returned null", 0);
        return -1;
    }

    if ((enum AVPixelFormat)src->format == dst_fmt && src->data[0]) {
        for (uint32_t r = 0; r < h; r++) {
            memcpy(dst + (size_t)r * stride,
                   src->data[0] + (size_t)r * (size_t)src->linesize[0],
                   stride);
        }
    } else {
        dec->sws_ctx = sws_getCachedContext(
            dec->sws_ctx,
            (int)w, (int)h, (enum AVPixelFormat)src->format,
            (int)w, (int)h, dst_fmt,
            SWS_BILINEAR, NULL, NULL, NULL
        );
        if (!dec->sws_ctx) {
            write_err(err_buf, err_buf_len, "sws_getCachedContext failed for RGBA/BGRA", 0);
            return -1;
        }
        uint8_t *dst_data[4] = { dst, NULL, NULL, NULL };
        int dst_linesize[4] = { (int)stride, 0, 0, 0 };
        sws_scale(dec->sws_ctx, (const uint8_t *const *)src->data, src->linesize,
                  0, (int)h, dst_data, dst_linesize);
    }

    out->kind = BGV_FRAME_CPU;
    out->backend_kind = dec->backend_kind;
    out->width = w;
    out->height = h;
    out->cpu_format = dec->cpu_format;
    out->nb_cpu_planes = 1;
    out->cpu_planes[0].offset = 0;
    out->cpu_planes[0].stride = stride;
    out->cpu_planes[0].rows = h;
    out->cpu_total_bytes = total;
    return 0;
}

int bgv_decode_next(
    BgvDecoder *dec,
    BgvFrameOut *out_frame,
    BgvAllocBufferFn alloc_fn,
    void *alloc_user_data,
    char *err_buf,
    size_t err_buf_len
) {
    if (!dec || !out_frame) {
        write_err(err_buf, err_buf_len, "invalid decoder state", 0);
        return BGV_STEP_ERROR;
    }
    if (dec->eof_reached) {
        return BGV_STEP_EOF;
    }
    memset(out_frame, 0, sizeof(*out_frame));

    while (1) {
        av_frame_unref(dec->frame);
        int ret = avcodec_receive_frame(dec->codec_ctx, dec->frame);
        if (ret == 0) {
            int64_t ts = dec->frame->best_effort_timestamp;
            if (ts == AV_NOPTS_VALUE) ts = dec->frame->pts;
            if (ts != AV_NOPTS_VALUE) {
                int64_t rel_ts = ts - dec->start_time;
                if (rel_ts < 0) rel_ts = 0;
                out_frame->pts_ms = av_rescale_q(rel_ts, dec->time_base, (AVRational){1, 1000});
            } else {
                out_frame->pts_ms = 0;
            }
            if (dec->frame->duration > 0) {
                out_frame->duration_ms = av_rescale_q(dec->frame->duration, dec->time_base, (AVRational){1, 1000});
            } else if (dec->fps > 0.0) {
                out_frame->duration_ms = (int64_t)(1000.0 / dec->fps);
            } else {
                out_frame->duration_ms = 33;
            }

            if ((enum AVPixelFormat)dec->frame->format == AV_PIX_FMT_VAAPI && dec->allow_dmabuf) {
                if (export_dmabuf(dec, dec->frame, out_frame) == 0) {
                    dec->decoded_any_hw_frame = 1;
                    av_frame_unref(dec->frame);
                    return BGV_STEP_FRAME;
                }
                /* Driver cannot export DRM_PRIME; fall back cleanly to VAAPI upload */
                dec->allow_dmabuf = 0;
                dec->backend_kind = BGV_BACKEND_VAAPI_UPLOAD;
            }

            AVFrame *src_frame = dec->frame;
            const AVPixFmtDescriptor *desc = av_pix_fmt_desc_get((enum AVPixelFormat)dec->frame->format);
            if (desc && (desc->flags & AV_PIX_FMT_FLAG_HWACCEL)) {
                av_frame_unref(dec->sw_frame);
                ret = av_hwframe_transfer_data(dec->sw_frame, dec->frame, 0);
                if (ret < 0) {
                    write_err(err_buf, err_buf_len, "av_hwframe_transfer_data failed", ret);
                    av_frame_unref(dec->frame);
                    return (dec->hw_device_type != AV_HWDEVICE_TYPE_NONE && !dec->decoded_any_hw_frame)
                        ? BGV_STEP_HW_ERROR
                        : BGV_STEP_ERROR;
                }
                dec->decoded_any_hw_frame = 1;
                src_frame = dec->sw_frame;
            }

            ret = export_cpu_frame(dec, src_frame, out_frame, alloc_fn, alloc_user_data, err_buf, err_buf_len);
            av_frame_unref(dec->sw_frame);
            av_frame_unref(dec->frame);
            return ret == 0 ? BGV_STEP_FRAME : BGV_STEP_ERROR;
        } else if (ret == AVERROR_EOF) {
            dec->eof_reached = 1;
            return BGV_STEP_EOF;
        } else if (ret != AVERROR(EAGAIN)) {
            write_err(err_buf, err_buf_len, "avcodec_receive_frame failed", ret);
            return (dec->hw_device_type != AV_HWDEVICE_TYPE_NONE && !dec->decoded_any_hw_frame)
                ? BGV_STEP_HW_ERROR
                : BGV_STEP_ERROR;
        }

        if (dec->draining) {
            /* Should hit AVERROR_EOF on receive_frame; avoid spinning */
            dec->eof_reached = 1;
            return BGV_STEP_EOF;
        }

        /* Read next packet for our video stream */
        while (1) {
            av_packet_unref(dec->pkt);
            ret = av_read_frame(dec->fmt_ctx, dec->pkt);
            if (ret == AVERROR_EOF) {
                dec->draining = 1;
                ret = avcodec_send_packet(dec->codec_ctx, NULL);
                if (ret < 0 && ret != AVERROR_EOF) {
                    write_err(err_buf, err_buf_len, "avcodec_send_packet(drain) failed", ret);
                    return BGV_STEP_ERROR;
                }
                break;
            } else if (ret < 0) {
                write_err(err_buf, err_buf_len, "av_read_frame failed", ret);
                return BGV_STEP_ERROR;
            }

            if (dec->pkt->stream_index != dec->stream_index) {
                continue;
            }

            ret = avcodec_send_packet(dec->codec_ctx, dec->pkt);
            av_packet_unref(dec->pkt);
            if (ret < 0 && ret != AVERROR(EAGAIN)) {
                write_err(err_buf, err_buf_len, "avcodec_send_packet failed", ret);
                return (dec->hw_device_type != AV_HWDEVICE_TYPE_NONE && !dec->decoded_any_hw_frame)
                    ? BGV_STEP_HW_ERROR
                    : BGV_STEP_ERROR;
            }
            break;
        }
    }
}

int bgv_seek_ms(
    BgvDecoder *dec,
    int64_t target_ms,
    char *err_buf,
    size_t err_buf_len
) {
    if (!dec || !dec->fmt_ctx || !dec->codec_ctx) {
        write_err(err_buf, err_buf_len, "invalid decoder state", 0);
        return -1;
    }
    if (target_ms < 0) target_ms = 0;
    int64_t rel_ts = av_rescale_q(target_ms, (AVRational){1, 1000}, dec->time_base);
    int64_t ts = dec->start_time + rel_ts;

    int ret = av_seek_frame(dec->fmt_ctx, dec->stream_index, ts, AVSEEK_FLAG_BACKWARD);
    if (ret < 0) {
        write_err(err_buf, err_buf_len, "av_seek_frame failed", ret);
        return -1;
    }
    avcodec_flush_buffers(dec->codec_ctx);
    dec->draining = 0;
    dec->eof_reached = 0;
    return 0;
}

void bgv_close(BgvDecoder *dec) {
    if (!dec) {
        return;
    }
    if (dec->sws_ctx) {
        sws_freeContext(dec->sws_ctx);
    }
    if (dec->drm_frame) {
        av_frame_free(&dec->drm_frame);
    }
    if (dec->sw_frame) {
        av_frame_free(&dec->sw_frame);
    }
    if (dec->frame) {
        av_frame_free(&dec->frame);
    }
    if (dec->pkt) {
        av_packet_free(&dec->pkt);
    }
    if (dec->codec_ctx) {
        avcodec_free_context(&dec->codec_ctx);
    }
    if (dec->hw_device_ctx) {
        av_buffer_unref(&dec->hw_device_ctx);
    }
    if (dec->fmt_ctx) {
        avformat_close_input(&dec->fmt_ctx);
    }
    free(dec);
}
