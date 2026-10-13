/* The GL half of `/bin/wlcompose` (.notes/wayland.md §6.10, stage 3d-b-5): the compositor
 * helper's renderer, and the test peer's surface maker.
 *
 * Two entry points, both on the render node:
 *   * `wlcompose_make_surface` — a full-colour object a *client* would own, for the test peer;
 *   * `wlcompose_render` — import dma-buf surfaces as sampler textures, draw them into one
 *     frame, and read it back in a single `glReadPixels` (D16, §5 row 27).
 *
 * The one thing this measures that the in-process probe (`tools/compose_probe.c`) could not is
 * **sampling across processes**: the peer renders a surface, and the helper — a different
 * process with its own EGL context on the same render node — samples it. Both keep their
 * contexts live for the whole run: the node tracks a context per client endpoint, so the helper's
 * readback is not blocked by the peer's context.
 *
 * A dynamic image is required (Mesa's DSOs); `tools/build-wlcompose.py` links this object in.
 */
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <gbm.h>

#define NODE "/dev/dri/renderD128"
#define DRM_FORMAT_XRGB8888 0x34325258u

/* The largest frame the compositor presents. */
#define MAX_FRAME_W 1024
#define MAX_FRAME_H 768

struct wlcompose_surface {
    int fd;
    unsigned w, h, stride, x, y;
};

static const char *vs_src =
    "attribute vec2 pos;\n"
    "attribute vec2 uv;\n"
    "varying vec2 v_uv;\n"
    "void main() { v_uv = uv; gl_Position = vec4(pos, 0.0, 1.0); }\n";

static const char *fs_src =
    "precision mediump float;\n"
    "uniform sampler2D tex;\n"
    "varying vec2 v_uv;\n"
    "void main() { gl_FragColor = texture2D(tex, v_uv); }\n";

static EGLDisplay g_dpy = EGL_NO_DISPLAY;
static EGLConfig g_cfg;
static EGLContext g_ctx = EGL_NO_CONTEXT;

static GLuint compile(GLenum type, const char *src)
{
    GLuint sh = glCreateShader(type);
    glShaderSource(sh, 1, &src, NULL);
    glCompileShader(sh);
    GLint ok = 0;
    glGetShaderiv(sh, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[512];
        glGetShaderInfoLog(sh, sizeof(log), NULL, log);
        printf("wlcompose: shader compile failed: %s\n", log);
        exit(1);
    }
    return sh;
}

/* The EGL display, config and context, once. Surfaceless: the render node is the device. */
static int boot(void)
{
    if (g_ctx != EGL_NO_CONTEXT)
        return 0;
    /* Touching the render node before EGL init is how the surfaceless platform is told which
     * driver to take — the same probe `/bin/wlgl` makes (`tools/wlgl_render.c`). */
    int node = open(NODE, O_RDWR);
    if (node < 0)
        setenv("LIBGL_ALWAYS_SOFTWARE", "1", 1);
    else
        close(node);
    setenv("LIBGL_DRIVERS_PATH", "/lib", 1);
    setenv("EGL_PLATFORM", "surfaceless", 1);
    g_dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    EGLint major = 0, minor = 0;
    if (g_dpy == EGL_NO_DISPLAY || !eglInitialize(g_dpy, &major, &minor) ||
        !eglBindAPI(EGL_OPENGL_ES_API)) {
        printf("wlcompose: EGL bring-up failed\n");
        return 1;
    }
    const EGLint cfg_attrs[] = {
        EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
        EGL_NONE
    };
    EGLint n = 0;
    if (!eglChooseConfig(g_dpy, cfg_attrs, &g_cfg, 1, &n) || n < 1) {
        printf("wlcompose: eglChooseConfig failed\n");
        return 1;
    }
    const EGLint ctx_attrs[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    g_ctx = eglCreateContext(g_dpy, g_cfg, EGL_NO_CONTEXT, ctx_attrs);
    if (g_ctx == EGL_NO_CONTEXT) {
        printf("wlcompose: eglCreateContext failed\n");
        return 1;
    }
    return 0;
}

/* Bind a `w`x`h` pbuffer (the surface a context needs current) and make it current. */
static EGLSurface make_current(unsigned w, unsigned h)
{
    const EGLint pb_attrs[] = {EGL_WIDTH, (EGLint)w, EGL_HEIGHT, (EGLint)h, EGL_NONE};
    EGLSurface pb = eglCreatePbufferSurface(g_dpy, g_cfg, pb_attrs);
    if (pb == EGL_NO_SURFACE || !eglMakeCurrent(g_dpy, pb, pb, g_ctx)) {
        printf("wlcompose: eglMakeCurrent failed\n");
        return EGL_NO_SURFACE;
    }
    return pb;
}

static GLuint texture_from_fd(int bo_fd, unsigned w, unsigned h, unsigned stride)
{
    PFNEGLCREATEIMAGEKHRPROC create_image =
        (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_target =
        (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    const EGLint attrs[] = {
        EGL_WIDTH, (EGLint)w,
        EGL_HEIGHT, (EGLint)h,
        EGL_LINUX_DRM_FOURCC_EXT, (EGLint)DRM_FORMAT_XRGB8888,
        EGL_DMA_BUF_PLANE0_FD_EXT, bo_fd,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)stride,
        EGL_NONE
    };
    EGLImageKHR img = create_image(g_dpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, attrs);
    if (img == EGL_NO_IMAGE_KHR) {
        printf("wlcompose: eglCreateImageKHR failed 0x%x\n", eglGetError());
        return 0;
    }
    GLuint tex = 0;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    image_target(GL_TEXTURE_2D, img);
    return tex;
}

/* A `w`x`h` object on the render node, drawn a solid colour, as a client's surface is: the
 * descriptor is what the peer hands over and the helper samples. Returns the fd, or -1, writing
 * the object's stride to `*out_stride`. A new gbm device and context per call keeps this usable
 * before `boot()` in the peer too. */
int wlcompose_make_surface(unsigned w, unsigned h, unsigned char r, unsigned char g,
                           unsigned char b, unsigned *out_stride)
{
    if (boot())
        return -1;
    int node = open(NODE, O_RDWR | O_CLOEXEC);
    if (node < 0) {
        printf("wlcompose: open %s failed\n", NODE);
        return -1;
    }
    struct gbm_device *dev = gbm_create_device(node);
    if (dev == NULL) {
        printf("wlcompose: gbm_create_device failed\n");
        return -1;
    }
    struct gbm_bo *bo = gbm_bo_create(dev, w, h, GBM_FORMAT_XRGB8888,
                                      GBM_BO_USE_RENDERING | GBM_BO_USE_LINEAR);
    if (bo == NULL) {
        printf("wlcompose: gbm_bo_create failed\n");
        return -1;
    }
    unsigned stride = gbm_bo_get_stride(bo);
    int fd = gbm_bo_get_fd(bo);
    *out_stride = stride;
    if (fd < 0 || make_current(w, h) == EGL_NO_SURFACE)
        return -1;
    GLuint tex = texture_from_fd(fd, w, h, stride);
    if (tex == 0)
        return -1;
    GLuint fbo = 0;
    glGenFramebuffers(1, &fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    glViewport(0, 0, w, h);
    glClearColor(r / 255.0f, g / 255.0f, b / 255.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glFinish();

    /* The object's own pages are what another process imports, and on this host Mesa renders into a
     * host-side texture: they hold the fill only after a resolve (a one-pixel `glReadPixels`) and a
     * transfer (`gbm_bo_map` with `TRANSFER_READ`), exactly as 4d's client does
     * (`tools/wldmabufgl_render.c`). Measured in `tools/egl_bo_probe.c`: the pages are black before
     * the transfer and hold the frame after. */
    unsigned char resolved[4];
    glReadPixels(0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, resolved);
    uint32_t mstride = 0;
    void *mbo = NULL;
    if (gbm_bo_map(bo, 0, 0, w, h, GBM_BO_TRANSFER_READ, &mstride, &mbo) == NULL) {
        printf("wlcompose: the transfer back into the surface failed\n");
        return -1;
    }
    gbm_bo_unmap(bo, mbo);
    return fd;
}

/* Compose `n` surfaces into a `w`x`h` XRGB8888 frame in `out`. Returns 0 on success. */
int wlcompose_render(const struct wlcompose_surface *s, int n, unsigned w, unsigned h,
                     unsigned char *out)
{
    if (w == 0 || h == 0 || (size_t)w * h * 4 > (size_t)MAX_FRAME_W * MAX_FRAME_H * 4) {
        printf("wlcompose: unsupported frame size\n");
        return 1;
    }
    if (boot())
        return 1;
    if (make_current(w, h) == EGL_NO_SURFACE)
        return 1;
    /* The frame is the default framebuffer's; nothing offscreen is bound here, but the bind is
     * explicit so a later change cannot leave a stale one. */
    glBindFramebuffer(GL_FRAMEBUFFER, 0);

    GLuint prog = glCreateProgram();
    glAttachShader(prog, compile(GL_VERTEX_SHADER, vs_src));
    glAttachShader(prog, compile(GL_FRAGMENT_SHADER, fs_src));
    glBindAttribLocation(prog, 0, "pos");
    glBindAttribLocation(prog, 1, "uv");
    glLinkProgram(prog);
    GLint linked = 0;
    glGetProgramiv(prog, GL_LINK_STATUS, &linked);
    if (!linked) {
        printf("wlcompose: program link failed\n");
        return 1;
    }
    glUseProgram(prog);
    glUniform1i(glGetUniformLocation(prog, "tex"), 0);
    glActiveTexture(GL_TEXTURE0);

    glViewport(0, 0, w, h);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    static const GLfloat pos[8] = {-1, 1, 1, 1, -1, -1, 1, -1};
    static const GLfloat uv[8] = {0, 0, 1, 0, 0, 1, 1, 1};
    for (int i = 0; i < n; i++) {
        GLuint tex = texture_from_fd(s->fd, s->w, s->h, s->stride);
        if (tex == 0)
            return 1;
        float l = 2.0f * s->x / w - 1.0f;
        float r = 2.0f * (s->x + s->w) / w - 1.0f;
        float t = 1.0f - 2.0f * s->y / h;
        float b0 = 1.0f - 2.0f * (s->y + s->h) / h;
        GLfloat vp[8];
        for (int k = 0; k < 4; k++) {
            vp[k * 2] = pos[k * 2] < 0 ? l : r;
            vp[k * 2 + 1] = pos[k * 2 + 1] > 0 ? t : b0;
        }
        glBindTexture(GL_TEXTURE_2D, tex);
        glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, vp);
        glEnableVertexAttribArray(0);
        glVertexAttribPointer(1, 2, GL_FLOAT, GL_FALSE, 0, uv);
        glEnableVertexAttribArray(1);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        s++;
    }
    glFinish();

    /* RGBA from GL, XRGB8888 out (LE bytes B,G,R,X) — the order the pool and the fb use. GL's first
     * row is the bottom of the frame and the display's first row is the top, so the rows are
     * flipped here: the compositor presents this buffer uncopied (FBIO_SCANOUT_SRC), which leaves no
     * later place to flip it. */
    unsigned char *rgba = malloc((size_t)w * h * 4);
    if (rgba == NULL)
        return 1;
    glReadPixels(0, 0, w, h, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
    for (unsigned y = 0; y < h; y++) {
        const unsigned char *src = rgba + (size_t)(h - 1 - y) * w * 4;
        unsigned char *dst = out + (size_t)y * w * 4;
        for (unsigned x = 0; x < w; x++) {
            dst[x * 4 + 0] = src[x * 4 + 2];
            dst[x * 4 + 1] = src[x * 4 + 1];
            dst[x * 4 + 2] = src[x * 4 + 0];
            dst[x * 4 + 3] = 0;
        }
    }
    free(rgba);
    return 0;
}
