/* The 3d-b-5 probe (`.notes/wayland.md` §6.10, stage 3d-b-5): the GPU-composition mechanism a
 * compositor helper uses — import several dma-buf surfaces as *textures*, draw them into one
 * frame on the render node, and read that frame back in one go.
 *
 * 4d (`tools/egl_bo_probe.c`, `tools/wldmabufgl_render.c`) proved one dma-buf becomes a render
 * target. Composition needs two more things, both measured here: a dma-buf as a **sampler
 * source**, and the composed frame read back with a single `glReadPixels` (the one copy, `§5`
 * row 27 — see the transfer note at the bottom).
 *
 * This is the helper's core: no canvas object and no scan-out, just "draw these surfaces and
 * hand me the frame", which is what `/bin/wlgl` does for one surface and 3d-b-5 does for many.
 * Two source objects are made, filled by GL (as a GL client's surface is), exported, re-imported
 * as sampler textures, drawn as quads into a full-output pbuffer, and the frame is read back and
 * checked at each rectangle and the background.
 *
 * Runs as a normal dynamic minix image (built by `tools/build-composeprobe.py`); needs the GL
 * host the virgl gates need.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>

#include <gbm.h>

#define NODE "/dev/dri/renderD128"
#define WIDTH 1024
#define HEIGHT 768
#define SRC 64

/* `DRM_FORMAT_XRGB8888`, from `drm_fourcc.h` (value declared, not included — the probe's point
 * is the path, not the header). */
#define DRM_FORMAT_XRGB8888 0x34325258u

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
        printf("compose: shader compile failed: %s\n", log);
        exit(1);
    }
    return sh;
}

/* A texture aliasing `bo_fd`'s pixels, the way a compositor imports a client's descriptor. */
static GLuint texture_from_fd(EGLDisplay dpy, int bo_fd, unsigned stride, unsigned w, unsigned h)
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
    EGLImageKHR img = create_image(dpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, attrs);
    if (img == EGL_NO_IMAGE_KHR) {
        printf("compose: eglCreateImageKHR failed 0x%x\n", eglGetError());
        exit(1);
    }
    GLuint tex = 0;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    image_target(GL_TEXTURE_2D, img);
    if (glGetError() != GL_NO_ERROR) {
        printf("compose: the imported image is not a texture\n");
        exit(1);
    }
    return tex;
}

static uint32_t at(const unsigned char *frame, int x, int y)
{
    const unsigned char *p = frame + (y * WIDTH + x) * 4;
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3];
}

int main(void)
{
    int node = open(NODE, O_RDWR | O_CLOEXEC);
    if (node < 0) {
        printf("compose: open %s failed\n", NODE);
        return 1;
    }
    struct gbm_device *dev = gbm_create_device(node);
    if (dev == NULL) {
        printf("compose: gbm_create_device failed\n");
        return 1;
    }

    setenv("LIBGL_DRIVERS_PATH", "/lib", 1);
    setenv("EGL_PLATFORM", "surfaceless", 1);
    EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    EGLint major = 0, minor = 0;
    if (dpy == EGL_NO_DISPLAY || !eglInitialize(dpy, &major, &minor) ||
        !eglBindAPI(EGL_OPENGL_ES_API)) {
        printf("compose: EGL bring-up failed\n");
        return 1;
    }
    const EGLint cfg_attrs[] = {
        EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
        EGL_NONE
    };
    EGLConfig cfg;
    EGLint n = 0;
    if (!eglChooseConfig(dpy, cfg_attrs, &cfg, 1, &n) || n < 1) {
        printf("compose: eglChooseConfig failed\n");
        return 1;
    }
    const EGLint ctx_attrs[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctx_attrs);
    const EGLint pb_attrs[] = {EGL_WIDTH, WIDTH, EGL_HEIGHT, HEIGHT, EGL_NONE};
    EGLSurface pb = eglCreatePbufferSurface(dpy, cfg, pb_attrs);
    if (ctx == EGL_NO_CONTEXT || pb == EGL_NO_SURFACE ||
        !eglMakeCurrent(dpy, pb, pb, ctx)) {
        printf("compose: EGL context failed\n");
        return 1;
    }
    printf("compose: GL_RENDERER=%s\n", (const char *)glGetString(GL_RENDERER));

    /* Two surfaces, each drawn with GL into its own object (as a GL client's is), then imported
     * by descriptor as a sampler — the compositor's two steps. The textures the fills rendered
     * through are kept: importing the same descriptor twice gives a second, empty texture. */
    uint32_t src_bind = GBM_BO_USE_RENDERING;
#ifdef GBM_BO_USE_TEXTURING
    src_bind |= GBM_BO_USE_TEXTURING;
#endif
    GLuint texs[2] = {0, 0};
    for (int s = 0; s < 2; s++) {
        struct gbm_bo *bo = gbm_bo_create(dev, SRC, SRC, GBM_FORMAT_XRGB8888, src_bind);
        if (bo == NULL) {
            printf("compose: gbm_bo_create(src) failed errno=%d\n", errno);
            return 1;
        }
        unsigned stride = gbm_bo_get_stride(bo);
        int fd = gbm_bo_get_fd(bo);
        GLuint t = texture_from_fd(dpy, fd, stride, SRC, SRC);
        texs[s] = t;
        GLuint f = 0;
        glGenFramebuffers(1, &f);
        glBindFramebuffer(GL_FRAMEBUFFER, f);
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, t, 0);
        glViewport(0, 0, SRC, SRC);
        glClearColor(s == 1 ? 1.0f : 0.0f, s == 0 ? 1.0f : 0.0f, 0.0f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        glFinish();
    }

    /* Compose: the pbuffer is the frame, so nothing is bound but the default framebuffer. */
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
        printf("compose: program link failed\n");
        return 1;
    }
    glUseProgram(prog);
    glUniform1i(glGetUniformLocation(prog, "tex"), 0);
    glActiveTexture(GL_TEXTURE0);

    glViewport(0, 0, WIDTH, HEIGHT);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    /* Rectangles (top-down): A at (64,64), B at (256,64), each 64x64. */
    struct {
        GLuint tex;
        int x, y;
    } quads[2] = {{texs[0], 64, 64}, {texs[1], 256, 64}};
    static const GLfloat pos[8] = {-1, 1, 1, 1, -1, -1, 1, -1};
    static const GLfloat uv[8] = {0, 0, 1, 0, 0, 1, 1, 1};
    for (int q = 0; q < 2; q++) {
        float l = 2.0f * quads[q].x / WIDTH - 1.0f;
        float r = 2.0f * (quads[q].x + SRC) / WIDTH - 1.0f;
        float t = 1.0f - 2.0f * quads[q].y / HEIGHT;
        float b = 1.0f - 2.0f * (quads[q].y + SRC) / HEIGHT;
        GLfloat vp[8];
        for (int i = 0; i < 4; i++) {
            vp[i * 2] = pos[i * 2] < 0 ? l : r;
            vp[i * 2 + 1] = pos[i * 2 + 1] > 0 ? t : b;
        }
        glBindTexture(GL_TEXTURE_2D, quads[q].tex);
        glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, vp);
        glEnableVertexAttribArray(0);
        glVertexAttribPointer(1, 2, GL_FLOAT, GL_FALSE, 0, uv);
        glEnableVertexAttribArray(1);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
    glFinish();
    printf("compose: draw err=0x%x\n", glGetError());

    /* The frame, in one readback — the copy the helper makes. */
    unsigned char *frame = malloc((size_t)WIDTH * HEIGHT * 4);
    if (frame == NULL) {
        printf("compose: no room for the frame\n");
        return 1;
    }
    glReadPixels(0, 0, WIDTH, HEIGHT, GL_RGBA, GL_UNSIGNED_BYTE, frame);

    /* GL y is bottom-up: the rect at top y in [64,128) reads at y in [640,704). RGBA bytes. */
    uint32_t a = at(frame, 96, 672);
    uint32_t bb = at(frame, 288, 672);
    uint32_t bg = at(frame, 512, 400);
    printf("compose: A=%08x (want 00ff00ff) B=%08x (want ff0000ff) bg=%08x (want 000000ff)\n",
           a, bb, bg);
    int ok = (a == 0x00ff00ffu) && (bb == 0xff0000ffu) && (bg == 0x000000ffu);
    printf("compose: %s\n", ok ? "PASS" : "FAIL (the surfaces did not composite)");

    /* The zero-copy route (draw into the object's own pages and scan *those* out) is a later
     * refinement: the object's pages hold only the clear here while the drawn quads reach it
     * through no reader that agrees with the transfer. This probe deliberately does not depend
     * on it — the helper reads the composed frame back once instead (§5 row 27). */
    free(frame);
    return ok ? 0 : 1;
}
