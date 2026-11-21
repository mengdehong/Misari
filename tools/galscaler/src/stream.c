#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl32.h>
#include <GLES2/gl2ext.h>
#include <drm_fourcc.h>
#include <libavcodec/avcodec.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_vaapi.h>
#include <libavutil/opt.h>
#include <libswscale/swscale.h>
#include <opus.h>
#include <rtc/rtc.h>
#include <stdlib.h>
#include <unistd.h>
#include <va/va_drmcommon.h>

static const char cname[] = "galscaler";
static const char h264_profile[] = "profile-level-id=42e02a;packetization-mode=1;level-asymmetry-allowed=1";

int gs_track(int peer, const char *mid, int payload, bool video, uint8_t delay_extension)
{
    uint32_t ssrc = video ? 0x47410001 : 0x47410002;
    rtcTrackInit media = {
        .direction = RTC_DIRECTION_SENDONLY,
        .codec = video ? RTC_CODEC_H264 : RTC_CODEC_OPUS,
        .payloadType = payload, .ssrc = ssrc, .mid = mid, .name = cname,
        .msid = cname, .trackId = video ? "game-video" : "game-audio",
        .profile = video ? h264_profile : "minptime=10;useinbandfec=1;stereo=1",
    };
    int track;
    if (video && delay_extension) {
        char sdp[1024];
        int length = snprintf(sdp, sizeof(sdp),
            "m=video 9 UDP/TLS/RTP/SAVPF %d\r\n"
            "a=mid:%s\r\na=sendonly\r\n"
            "a=rtpmap:%d H264/90000\r\na=fmtp:%d %s\r\n"
            "a=rtcp-fb:%d nack\r\na=rtcp-fb:%d nack pli\r\na=rtcp-fb:%d goog-remb\r\n"
            "a=extmap:%u/sendonly http://www.webrtc.org/experiments/rtp-hdrext/playout-delay\r\n"
            "a=ssrc:%u cname:%s\r\na=ssrc:%u msid:%s %s\r\na=msid:%s %s\r\n",
            payload, mid, payload, payload, h264_profile, payload, payload, payload,
            delay_extension, ssrc, cname, ssrc, cname, media.trackId, cname, media.trackId);
        if (length < 0 || (size_t)length >= sizeof(sdp)) return RTC_ERR_INVALID;
        track = rtcAddTrack(peer, sdp);
    } else {
        track = rtcAddTrackEx(peer, &media);
    }
    if (track < 0) return track;
    rtcPacketizerInit rtp = {
        .ssrc = ssrc, .cname = cname, .payloadType = payload,
        .clockRate = video ? 90000 : 48000, .maxFragmentSize = 1150,
        .nalSeparator = RTC_NAL_SEPARATOR_START_SEQUENCE,
        .playoutDelayId = delay_extension,
        .playoutDelayMin = 0, .playoutDelayMax = 5, // RTP units are 10 ms.
    };
    int err = video ? rtcSetH264Packetizer(track, &rtp) : rtcSetOpusPacketizer(track, &rtp);
    if (err >= 0) err = rtcChainRtcpSrReporter(track);
    if (err >= 0 && video) err = rtcChainRtcpNackResponder(track, 512);
    if (err < 0) {
        rtcDeleteTrack(track);
        return err;
    }
    return track;
}

int gs_peer(void)
{
    rtcConfiguration config = {
        .disableAutoNegotiation = true,
        .maxMessageSize = 1024,
    };
    return rtcCreatePeerConnection(&config);
}

int gs_controls(int peer)
{
    rtcDataChannelInit init = { .negotiated = true, .manualStream = true, .stream = 0 };
    return rtcCreateDataChannelEx(peer, "controls", &init);
}

static AVBufferRef *va_frames(const char *device, int width, int height)
{
    AVBufferRef *hw = NULL;
    if (av_hwdevice_ctx_create(&hw, AV_HWDEVICE_TYPE_VAAPI, device, NULL, 0) < 0)
        return NULL;
    AVBufferRef *ref = av_hwframe_ctx_alloc(hw);
    av_buffer_unref(&hw);
    if (!ref) return NULL;
    AVHWFramesContext *frames = (AVHWFramesContext *)ref->data;
    frames->format = AV_PIX_FMT_VAAPI;
    frames->sw_format = AV_PIX_FMT_NV12;
    frames->width = width;
    frames->height = height;
    if (av_hwframe_ctx_init(ref) < 0) av_buffer_unref(&ref);
    return ref;
}

// Each slot keeps an encoder surface and two GL framebuffer views of its
// DMA-BUF. Import the planes once and keep the driver-provided modifier.
struct gs_nv12_target {
    GLuint texture[2];
    GLuint framebuffer[2];
};

void gs_nv12_target_free(struct gs_nv12_target *target)
{
    if (!target) return;
    glDeleteFramebuffers(2, target->framebuffer);
    glDeleteTextures(2, target->texture);
    free(target);
}

struct gs_nv12_target *gs_nv12_target_new(const AVFrame *frame)
{
    AVHWFramesContext *frames = (AVHWFramesContext *)frame->hw_frames_ctx->data;
    AVVAAPIDeviceContext *device = frames->device_ctx->hwctx;
    VADRMPRIMESurfaceDescriptor desc = {0};
    if (vaExportSurfaceHandle(device->display, (VASurfaceID)(uintptr_t)frame->data[3],
            VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
            VA_EXPORT_SURFACE_WRITE_ONLY | VA_EXPORT_SURFACE_SEPARATE_LAYERS, &desc) != VA_STATUS_SUCCESS)
        return NULL;

    static const uint32_t formats[] = {DRM_FORMAT_R8, DRM_FORMAT_GR88};
    PFNGLEGLIMAGETARGETTEXTURE2DOESPROC bind =
        (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    EGLDisplay display = eglGetCurrentDisplay();
    struct gs_nv12_target *target = calloc(1, sizeof(*target));
    bool ok = target && bind && desc.fourcc == VA_FOURCC_NV12 && desc.num_layers == 2
        && desc.width >= (uint32_t)frame->width && desc.height >= (uint32_t)frame->height;
    if (ok) {
        glGenTextures(2, target->texture);
        glGenFramebuffers(2, target->framebuffer);
    }
    for (int i = 0; ok && i < 2; i++) {
        unsigned object = desc.layers[i].object_index[0];
        if (desc.layers[i].drm_format != formats[i] || desc.layers[i].num_planes != 1
            || object >= desc.num_objects) {
            ok = false;
            break;
        }
        uint64_t modifier = desc.objects[object].drm_format_modifier;
        // The allocation can be padded. Import the full planes, and draw
        // only the visible rectangle that is supplied to the codec.
        EGLAttrib attrs[] = {
            EGL_WIDTH, desc.width >> i, EGL_HEIGHT, desc.height >> i,
            EGL_LINUX_DRM_FOURCC_EXT, formats[i],
            EGL_DMA_BUF_PLANE0_FD_EXT, desc.objects[object].fd,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT, desc.layers[i].offset[0],
            EGL_DMA_BUF_PLANE0_PITCH_EXT, desc.layers[i].pitch[0],
            modifier == DRM_FORMAT_MOD_INVALID ? EGL_NONE : EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
            (uint32_t)modifier, EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, (uint32_t)(modifier >> 32),
            EGL_NONE,
        };
        EGLImage image = eglCreateImage(display, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, attrs);
        if (image == EGL_NO_IMAGE) {
            ok = false;
            break;
        }
        glBindTexture(GL_TEXTURE_2D, target->texture[i]);
        bind(GL_TEXTURE_2D, image);
        eglDestroyImage(display, image);
        glBindFramebuffer(GL_FRAMEBUFFER, target->framebuffer[i]);
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, target->texture[i], 0);
        ok = glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE;
    }
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glBindTexture(GL_TEXTURE_2D, 0);
    for (unsigned i = 0; i < desc.num_objects; i++) close(desc.objects[i].fd);
    if (!ok) {
        gs_nv12_target_free(target);
        target = NULL;
    }
    return target;
}

AVFrame *gs_va_surface_new(const char *device, int width, int height, const AVFrame *other)
{
    AVBufferRef *frames = other ? av_buffer_ref(other->hw_frames_ctx) : va_frames(device, width, height);
    if (!frames) return NULL;
    AVFrame *frame = av_frame_alloc();
    if (frame && av_hwframe_get_buffer(frames, frame, 0) < 0) av_frame_free(&frame);
    av_buffer_unref(&frames);
    return frame;
}

void gs_va_surface_free(AVFrame *frame)
{
    av_frame_free(&frame);
}

const void *gs_va_surface_pool(const AVFrame *frame)
{
    return frame->hw_frames_ctx->data;
}

struct gs_nv12 {
    GLuint program, sampler;
    GLint chroma;
};

static GLuint compile_shader(GLenum type, const char *source)
{
    GLuint shader = glCreateShader(type);
    glShaderSource(shader, 1, &source, NULL);
    glCompileShader(shader);
    GLint ok;
    glGetShaderiv(shader, GL_COMPILE_STATUS, &ok);
    if (ok) return shader;
    GLchar log[512];
    glGetShaderInfoLog(shader, sizeof(log), NULL, log);
    fprintf(stderr, "GalScaler NV12 shader: %s\n", log);
    glDeleteShader(shader);
    return 0;
}

struct gs_nv12 *gs_nv12_new(void)
{
    GLuint vertex = compile_shader(GL_VERTEX_SHADER,
        "#version 300 es\n"
        "out vec2 uv;\n"
        "void main(){\n"
        "  uv = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2);\n"
        "  gl_Position = vec4(uv * 2.0 - 1.0, 0.0, 1.0);\n"
        "}\n");
    GLuint fragment = compile_shader(GL_FRAGMENT_SHADER,
        "#version 300 es\n"
        "precision highp float;\n"
        "in vec2 uv; uniform sampler2D rgb; uniform bool chroma; out vec4 color;\n"
        "void main(){\n"
        "  vec3 v = texture(rgb, uv).rgb;\n"
        "  float y = dot(v, vec3(0.2126, 0.7152, 0.0722));\n"
        "  vec2 cbcr = vec2((v.b-y)/1.8556, (v.r-y)/1.5748);\n"
        "  color = chroma ? vec4((128.0 + 224.0*cbcr)/255.0, 0.0, 1.0)\n"
        "                 : vec4((16.0 + 219.0*y)/255.0, 0.0, 0.0, 1.0);\n"
        "}\n");
    GLuint program = glCreateProgram();
    GLint ok = GL_FALSE;
    if (vertex && fragment) {
        glAttachShader(program, vertex);
        glAttachShader(program, fragment);
        glLinkProgram(program);
        glGetProgramiv(program, GL_LINK_STATUS, &ok);
    }
    glDeleteShader(vertex);
    glDeleteShader(fragment);
    struct gs_nv12 *converter = ok ? calloc(1, sizeof(*converter)) : NULL;
    if (!converter) {
        glDeleteProgram(program);
        return NULL;
    }
    converter->program = program;
    converter->chroma = glGetUniformLocation(program, "chroma");
    glGenSamplers(1, &converter->sampler);
    glSamplerParameteri(converter->sampler, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glSamplerParameteri(converter->sampler, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glSamplerParameteri(converter->sampler, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glSamplerParameteri(converter->sampler, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    glDisable(GL_DITHER);
    return converter;
}

void gs_nv12_free(struct gs_nv12 *converter)
{
    glDeleteSamplers(1, &converter->sampler);
    glDeleteProgram(converter->program);
    free(converter);
}

bool gs_nv12_draw(struct gs_nv12 *converter, const struct gs_nv12_target *target,
                  GLuint rgb, int width, int height)
{
    glUseProgram(converter->program);
    glBindSampler(0, converter->sampler);
    glBindTexture(GL_TEXTURE_2D, rgb);
    for (int i = 0; i < 2; i++) {
        glBindFramebuffer(GL_FRAMEBUFFER, target->framebuffer[i]);
        glViewport(0, 0, width >> i, height >> i);
        glUniform1i(converter->chroma, i);
        glDrawArrays(GL_TRIANGLES, 0, 3);
    }
    return glGetError() == GL_NO_ERROR;
}

void gs_nv12_submit(bool finish)
{
    if (finish) glFinish();
    else glFlush();
}

struct gs_encoder {
    AVCodecContext *codec;
    AVFrame *cpu, *surface;
    AVPacket *packet;
    struct SwsContext *scale;
    int64_t frame_index;
};

void gs_encoder_free(struct gs_encoder *e)
{
    if (!e) return;
    sws_freeContext(e->scale);
    av_packet_free(&e->packet);
    av_frame_free(&e->surface);
    av_frame_free(&e->cpu);
    avcodec_free_context(&e->codec);
    free(e);
}

static struct gs_encoder *encoder(const char *name, const char *device, const AVFrame *input, int w, int h, int fps)
{
    const AVCodec *codec = avcodec_find_encoder_by_name(name);
    if (!codec) return NULL;
    struct gs_encoder *e = calloc(1, sizeof(*e));
    if (!e) return NULL;
    e->codec = avcodec_alloc_context3(codec);
    e->packet = av_packet_alloc();
    if (!e->codec || !e->packet) goto fail;

    AVCodecContext *c = e->codec;
    c->width = w;
    c->height = h;
    c->time_base = (AVRational){1, 90000};
    c->framerate = (AVRational){fps, 1};
    c->gop_size = fps;
    c->max_b_frames = 0;
    c->bit_rate = av_clip64((int64_t)w * h * fps / 10, 600000, 12000000);
    c->rc_max_rate = c->bit_rate;
    c->rc_buffer_size = c->bit_rate / 2;
    c->flags |= AV_CODEC_FLAG_LOW_DELAY | AV_CODEC_FLAG_CLOSED_GOP;
    c->color_range = AVCOL_RANGE_MPEG;
    c->color_primaries = AVCOL_PRI_BT709;
    c->color_trc = AVCOL_TRC_BT709;
    c->colorspace = AVCOL_SPC_BT709;
    c->pix_fmt = (device || input) ? AV_PIX_FMT_VAAPI : AV_PIX_FMT_NV12;
    if (c->pix_fmt == AV_PIX_FMT_VAAPI) {
        c->hw_frames_ctx = input ? av_buffer_ref(input->hw_frames_ctx) : va_frames(device, w, h);
        if (!c->hw_frames_ctx) goto fail;
        if (!input) {
            e->surface = av_frame_alloc();
            if (!e->surface) goto fail;
        }
        av_opt_set(c->priv_data, "profile", "constrained_baseline", 0);
        av_opt_set(c->priv_data, "level", "4.2", 0);
        av_opt_set(c->priv_data, "async_depth", "1", 0);
        av_opt_set(c->priv_data, "rc_mode", "VBR", 0);
    } else {
        av_opt_set(c->priv_data, "profile", "baseline", 0);
        av_opt_set(c->priv_data, "level", "4.2", 0);
        av_opt_set(c->priv_data, "preset", "p1", 0);
        av_opt_set(c->priv_data, "tune", "ull", 0);
        av_opt_set(c->priv_data, "zerolatency", "1", 0);
        av_opt_set(c->priv_data, "forced-idr", "1", 0);
    }
    if (avcodec_open2(c, codec, NULL) < 0) goto fail;
    if (input) {
        e->surface = av_frame_alloc();
        if (!e->surface) goto fail;
        fprintf(stderr, "GalScaler stream: %s %dx%d @ %d fps (DMA-BUF NV12)\n", name, w, h, fps);
        return e;
    }
    e->cpu = av_frame_alloc();
    if (!e->cpu) goto fail;
    e->cpu->format = AV_PIX_FMT_NV12;
    e->cpu->width = w;
    e->cpu->height = h;
    if (av_frame_get_buffer(e->cpu, 0) < 0) goto fail;
    e->scale = sws_getContext(w, h, AV_PIX_FMT_RGBA, w, h, AV_PIX_FMT_NV12,
                            SWS_BILINEAR, NULL, NULL, NULL);
    if (!e->scale) goto fail;
    const int *bt709 = sws_getCoefficients(SWS_CS_ITU709);
    if (sws_setColorspaceDetails(e->scale, bt709, 1, bt709, 0, 0, 1 << 16, 1 << 16) < 0)
        goto fail;
    fprintf(stderr, "GalScaler stream: %s %dx%d @ %d fps (RGBA readback)\n", name, w, h, fps);
    return e;
fail:
    gs_encoder_free(e);
    return NULL;
}

struct gs_encoder *gs_encoder_new(const char *device, const AVFrame *input, int w, int h, int fps)
{
    if (input) return encoder("h264_vaapi", NULL, input, w, h, fps);
    struct gs_encoder *e = device ? encoder("h264_vaapi", device, NULL, w, h, fps) : NULL;
    return e ? e : encoder("h264_nvenc", NULL, NULL, w, h, fps);
}

static int send_video(struct gs_encoder *e, int track, AVFrame *frame, int64_t timestamp,
                      bool keyframe)
{
    frame->pts = timestamp;
    bool new_gop = e->frame_index++ % e->codec->gop_size == 0;
    frame->pict_type = keyframe || new_gop
        ? AV_PICTURE_TYPE_I : AV_PICTURE_TYPE_NONE;
    int err = avcodec_send_frame(e->codec, frame);
    if (err < 0) return err;
    while ((err = avcodec_receive_packet(e->codec, e->packet)) >= 0) {
        rtcSetTrackRtpTimestamp(track, (uint32_t)e->packet->pts);
        rtcSendMessage(track, (const char *)e->packet->data, e->packet->size);
        av_packet_unref(e->packet);
    }
    return err == AVERROR(EAGAIN) ? 0 : err;
}

int gs_encode_va(struct gs_encoder *e, int track, const AVFrame *surface, int64_t timestamp,
                 bool keyframe)
{
    int err = av_frame_ref(e->surface, surface);
    if (err < 0) return err;
    err = send_video(e, track, e->surface, timestamp, keyframe);
    av_frame_unref(e->surface);
    return err;
}

int gs_encode_video(struct gs_encoder *e, int track, const uint8_t *rgba, int64_t timestamp,
                    bool keyframe)
{
    int err = av_frame_make_writable(e->cpu);
    if (err < 0) return err;
    const uint8_t *source[4] = { rgba };
    const int strides[4] = { e->cpu->width * 4 };
    if (sws_scale(e->scale, source, strides, 0, e->cpu->height,
                  e->cpu->data, e->cpu->linesize) != e->cpu->height)
        return AVERROR(EINVAL);

    AVFrame *frame = e->cpu;
    if (e->surface) {
        av_frame_unref(e->surface);
        err = av_hwframe_get_buffer(e->codec->hw_frames_ctx, e->surface, 0);
        if (err < 0) return err;
        err = av_hwframe_transfer_data(e->surface, frame, 0);
        if (err < 0) return err;
        frame = e->surface;
    }
    return send_video(e, track, frame, timestamp, keyframe);
}

OpusEncoder *gs_audio_new(void)
{
    int err;
    OpusEncoder *encoder = opus_encoder_create(48000, 2, OPUS_APPLICATION_RESTRICTED_LOWDELAY, &err);
    if (err != OPUS_OK) return NULL;
    opus_encoder_ctl(encoder, OPUS_SET_BITRATE(96000));
    return encoder;
}

void gs_encode_audio(OpusEncoder *encoder, int track, const int16_t *pcm, uint32_t timestamp)
{
    unsigned char packet[1024];
    int bytes = opus_encode(encoder, pcm, 960, packet, sizeof(packet));
    if (bytes > 0) {
        rtcSetTrackRtpTimestamp(track, timestamp);
        rtcSendMessage(track, (const char *)packet, bytes);
    }
}
