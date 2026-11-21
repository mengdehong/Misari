#include <EGL/egl.h>
#include <GLES3/gl32.h>
#include <libplacebo/opengl.h>
#include <libplacebo/renderer.h>
#include <libplacebo/shaders/custom.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>

// This module runs only on its own EGL context, sharing textures with Smithay.
// GL fences transfer ownership of each frame without GPU-to-CPU copies.
struct gs_gpu {
    pl_log log;
    pl_opengl gl;
    pl_renderer renderer;
    const struct pl_hook *hooks[3];
    int num_hooks;
    pl_tex input, output, intermediate[2];
    GLuint input_id, output_id;
    int iw, ih, ow, oh;
    int passes, neural_passes, upscale_steps;
    double gpu_ms;
    GLuint queries[4];
    bool pending[4];
    int query_index;
    void (*query_result)(GLuint, GLenum, GLuint64 *);
};

static void pass_info(void *priv, const struct pl_render_info *info)
{
    struct gs_gpu *g = priv;
    g->passes++;
    const char *name = info->pass->shader->description;
    if (name && ((strstr(name, "CuNNy") && !strstr(name, "RGB")) || strstr(name, "Anime4K")))
        g->neural_passes++;
}

void gs_gpu_destroy(struct gs_gpu *g)
{
    if (!g) return;
    if (g->query_result) glDeleteQueries(4, g->queries);
    if (g->renderer) pl_renderer_destroy(&g->renderer);
    if (g->gl) {
        pl_tex_destroy(g->gl->gpu, &g->input);
        pl_tex_destroy(g->gl->gpu, &g->output);
        for (int i = 0; i < 2; i++)
            pl_tex_destroy(g->gl->gpu, &g->intermediate[i]);
        for (int i = 0; i < g->num_hooks; i++)
            pl_mpv_user_shader_destroy(&g->hooks[i]);
        pl_opengl_destroy(&g->gl);
    }
    pl_log_destroy(&g->log);
    free(g);
}

struct gs_gpu *gs_gpu_create(const char *const *shaders, int count)
{
    struct gs_gpu *g = calloc(1, sizeof(*g));
    if (!g) return NULL;
    g->gpu_ms = -1;
    if (strstr((const char *)glGetString(GL_EXTENSIONS), "GL_EXT_disjoint_timer_query")) {
        g->query_result = (void (*)(GLuint, GLenum, GLuint64 *))eglGetProcAddress("glGetQueryObjectui64vEXT");
        if (g->query_result) glGenQueries(4, g->queries);
    }
    g->log = pl_log_create(PL_API_VER, pl_log_params(
        .log_cb = pl_log_color, .log_level = getenv("GALSCALER_GPU_DEBUG") ? PL_LOG_DEBUG : PL_LOG_WARN));
    fprintf(stderr, "GalScaler GPU: %s; %s; hooks=%d\n", glGetString(GL_RENDERER), glGetString(GL_VERSION), count);
    g->gl = pl_opengl_create(g->log, pl_opengl_params(
        .get_proc_addr = (pl_voidfunc_t (*)(const char *)) eglGetProcAddress,
        .egl_display = eglGetCurrentDisplay(), .egl_context = eglGetCurrentContext()));
    if (!g->gl) goto error;
    g->renderer = pl_renderer_create(g->log, g->gl->gpu);
    if (!g->renderer || count > 3) goto error;
    for (int i = 0; i < count; i++) {
        g->hooks[i] = pl_mpv_user_shader_parse(g->gl->gpu, shaders[i], strlen(shaders[i]));
        if (!g->hooks[i]) goto error;
        g->num_hooks++;
    }
    return g;
error:
    gs_gpu_destroy(g);
    return NULL;
}

static struct pl_frame frame(pl_tex texture)
{
    return (struct pl_frame) {
        .num_planes = 1,
        // Smithay's offscreen projection and libplacebo both address logical
        // row zero at texture row zero. Keep the CNN's vertical kernel intact.
        .planes[0] = { .texture = texture, .components = 4,
                      .component_mapping = { 0, 1, 2, 3 } },
        .repr = pl_color_repr_rgb,
        .color = pl_color_space_srgb,
    };
}

static pl_tex wrap(struct gs_gpu *g, GLuint texture, int w, int h)
{
    return pl_opengl_wrap(g->gl->gpu, pl_opengl_wrap_params(
        .texture = texture, .target = GL_TEXTURE_2D, .iformat = GL_RGBA8,
        .width = w, .height = h));
}

bool gs_gpu_render(struct gs_gpu *g, GLuint input, int iw, int ih,
                   GLuint output, int ow, int oh, GLsync acquire, GLsync *release)
{
    glWaitSync(acquire, 0, GL_TIMEOUT_IGNORED);
    glDeleteSync(acquire);
    if (input != g->input_id || iw != g->iw || ih != g->ih) {
        pl_tex_destroy(g->gl->gpu, &g->input);
        g->input = wrap(g, input, iw, ih);
        g->input_id = input; g->iw = iw; g->ih = ih;
    }
    if (output != g->output_id || ow != g->ow || oh != g->oh) {
        pl_tex_destroy(g->gl->gpu, &g->output);
        g->output = wrap(g, output, ow, oh);
        g->output_id = output; g->ow = ow; g->oh = oh;
    }
    if (!g->input || !g->output) return false;
    struct pl_frame source = frame(g->input);
    struct pl_render_params params = pl_render_default_params;
    params.upscaler = &pl_filter_bilinear;
    params.downscaler = &pl_filter_mitchell;
    params.hooks = g->hooks;
    params.num_hooks = g->num_hooks;
    params.info_callback = pass_info;
    params.info_priv = g;
    g->passes = g->neural_passes = g->upscale_steps = 0;
    int query = g->query_index;
    bool timing = g->query_result != NULL;
    if (timing && g->pending[query]) {
        GLuint available = 0;
        glGetQueryObjectuiv(g->queries[query], GL_QUERY_RESULT_AVAILABLE, &available);
        if (available) {
            GLuint64 elapsed = 0;
            GLint disjoint = 0;
            g->query_result(g->queries[query], GL_QUERY_RESULT, &elapsed);
            glGetIntegerv(0x8FBB, &disjoint); // GL_GPU_DISJOINT_EXT
            g->gpu_ms = disjoint ? -1 : elapsed / 1e6;
            g->pending[query] = false;
        } else { timing = false; }
    }
    if (timing) glBeginQuery(0x88BF, g->queries[query]); // GL_TIME_ELAPSED_EXT
    bool ok = true;
    int w = iw, h = ih;
    while (ok && g->num_hooks && (w < ow || h < oh)) {
        w *= 2; h *= 2;
        pl_tex texture = g->output;
        if (w != ow || h != oh) {
            pl_tex *temp = &g->intermediate[g->upscale_steps % 2];
            ok = pl_tex_recreate(g->gl->gpu, temp, pl_tex_params(
                .w = w, .h = h,
                .format = pl_find_fmt(g->gl->gpu, PL_FMT_FLOAT, 4, 16, 0,
                                     PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_LINEAR),
                .sampleable = true, .renderable = true));
            if (!ok) break;
            texture = *temp;
        }
        struct pl_frame target = frame(texture);
        int before = g->neural_passes;
        ok = pl_render_image(g->renderer, &source, &target, &params);
        ok = ok && g->neural_passes > before;
        g->upscale_steps++;
        source = target;
    }
    if (ok && source.planes[0].texture != g->output) {
        struct pl_frame target = frame(g->output);
        // CNNs have already covered the target. Only resample the final image.
        params.hooks = NULL;
        params.num_hooks = 0;
        ok = pl_render_image(g->renderer, &source, &target, &params);
    }
    if (timing) {
        glEndQuery(0x88BF);
        g->pending[query] = true;
        g->query_index = (query + 1) % 4;
    }
    struct pl_render_errors errors = pl_renderer_get_errors(g->renderer);
    // A failed CNN must be reported, rather than silently showing a basic scaler.
    if (!ok || errors.errors || errors.num_disabled_hooks) return false;
    *release = glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE, 0);
    glFlush();
    return true;
}

void gs_gpu_stats(struct gs_gpu *g, int *passes, int *neural_passes, int *upscale_steps, double *gpu_ms)
{
    *passes = g->passes;
    *neural_passes = g->neural_passes;
    *upscale_steps = g->upscale_steps;
    *gpu_ms = g->gpu_ms;
}
