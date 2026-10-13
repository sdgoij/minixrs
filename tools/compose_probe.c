/* The 3d-b-5 probe (`.notes/wayland.md` §6.10, stage 3d-b-5): the GPU-composition mechanism a
 * compositor helper needs — import several dma-buf surfaces as *textures* and draw them into a
 * shared canvas on the render node, then read the canvas back.
 *
 * 4d (`tools/egl_bo_probe.c`, `tools/wldmabufgl_render.c`) proved one dma-buf becomes a render
 * target. Composition needs the other half: a dma-buf as a **sampler source**. This probe makes
 * two source objects, fills them on the CPU (as `/bin/wldmabuf` does), exports each to a
 * descriptor, imports each descriptor back as an `EGLImage` texture, and draws both as textured
 * quads into a full-output canvas. It then reads the canvas the way the compositor would
 * (resolve, transfer, then the object's pages) and checks both rectangles and the background.
 *
 * A sampler source needs `PIPE_BIND_SAMPLER_VIEW` on the object, which `GBM_BO_USE_RENDERING`
 * alone may not ask for — that is one of the things this measures, so the result line says which
 * bind the BO was made with.
 *
 * Runs as a normal dynamic minix image (built by `tools/build-composeprobe.py`); needs the GL
 * host the virgl gates need.
 */
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include <gbm.h>
#include <virtgpu_drm.h>
#include <xf86drm.h>

#define NODE "/dev/dri/renderD128"
#define WIDTH 1024
#define HEIGHT 768
#define SRC 64
#define PITCH (WIDTH * 4)

/* `DRM_FORMAT_XRGB8888`, from `drm_fourcc.h` (value declared, not included — the probe's point
 * is the ioctl path, not the header). */
#define DRM_FORMAT_XRGB8888 0x34325258u

#define GREEN 0x0000FF00u /* LE bytes B,G,R,X = 00,FF,00,00 */
#define RED 0x00FF0000u

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

/* A page of `bo`'s own memory, read the way the compositor reads a client's buffer. */
static uint32_t raw_pixel(struct gbm_bo *bo, int bo_fd, unsigned stride, unsigned x, unsigned y)
{
    (void)bo;
    unsigned handle = 0;
    int fd = open(NODE, O_RDWR);
    uint32_t px = 0xDEADBEEFu;
    if (fd < 0 || drmPrimeFDToHandle(fd, bo_fd, &handle) != 0)
        return px;
    struct drm_virtgpu_map m = {0, handle, 0};
    if (drmIoctl(fd, DRM_IOCTL_VIRTGPU_MAP, &m) != 0)
        return px;
    void *p = mmap(NULL, (size_t)PITCH * HEIGHT, PROT_READ, MAP_SHARED, fd, (off_t)m.offset);
    if (p != MAP_FAILED) {
        px = *(uint32_t *)((char *)p + y * stride + x * 4);
        munmap(p, (size_t)PITCH * HEIGHT);
    }
    close(fd);
    return px;
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
    const EGLint pb_attrs[] = {EGL_WIDTH, 16, EGL_HEIGHT, 16, EGL_NONE};
    EGLSurface pb = eglCreatePbufferSurface(dpy, cfg, pb_attrs);
    if (ctx == EGL_NO_CONTEXT || pb == EGL_NO_SURFACE ||
        !eglMakeCurrent(dpy, pb, pb, ctx)) {
        printf("compose: EGL context failed\n");
        return 1;
    }
    printf("compose: GL_RENDERER=%s\n", (const char *)glGetString(GL_RENDERER));

    /* Two source objects, filled on the CPU (the way `/bin/wldmabuf` fills one). RENDERING for
     * the import, and TEXTURING if this header knows it: sampling needs SAMPLER_VIEW, and which
     * bind the driver ends up with is what the texture import below tells us. */
    uint32_t src_bind = GBM_BO_USE_RENDERING;
#ifdef GBM_BO_USE_TEXTURING
    src_bind |= GBM_BO_USE_TEXTURING;
#endif
    int fds[2] = {-1, -1};
    unsigned sstride[2] = {0, 0};
    GLuint texs[2] = {0, 0};
    for (int s = 0; s < 2; s++) {
        struct gbm_bo *bo = gbm_bo_create(dev, SRC, SRC, GBM_FORMAT_XRGB8888, src_bind);
        if (bo == NULL) {
            printf("compose: gbm_bo_create(src) failed errno=%d\n", errno);
            return 1;
        }
        sstride[s] = gbm_bo_get_stride(bo);
        fds[s] = gbm_bo_get_fd(bo);
        /* Draw the surface's content with GL (as a GL client does), through an image over its
         * own descriptor, so the object's *host* texture holds it — what a sampler reads. A
         * CPU write to the pages (as `/bin/wldmabuf` does for a 2D object) would leave the host
         * texture black, which is the trap this probe exists to show. The texture is kept for
         * the sample below: importing the same descriptor twice gives two textures, and only
         * the rendered one holds the colour. */
        GLuint t = texture_from_fd(dpy, fds[s], sstride[s], SRC, SRC);
        texs[s] = t;
        GLuint f = 0;
        glGenFramebuffers(1, &f);
        glBindFramebuffer(GL_FRAMEBUFFER, f);
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, t, 0);
        glViewport(0, 0, SRC, SRC);
        glClearColor(s == 1 ? 1.0f : 0.0f, s == 0 ? 1.0f : 0.0f, 0.0f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        glFinish();
        unsigned char one[4];
        glReadPixels(0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, one);
        uint32_t st = 0;
        void *md = NULL;
        if (gbm_bo_map(bo, 0, 0, SRC, SRC, GBM_BO_TRANSFER_READ, &st, &md) != NULL)
            gbm_bo_unmap(bo, md);
    }

    /* The canvas: the full output, the object the compositor would scan out. */
    struct gbm_bo *canvas = gbm_bo_create(dev, WIDTH, HEIGHT, GBM_FORMAT_XRGB8888,
                                          GBM_BO_USE_RENDERING | GBM_BO_USE_LINEAR);
    if (canvas == NULL) {
        printf("compose: canvas LINEAR create failed errno=%d, trying without LINEAR\n", errno);
        canvas = gbm_bo_create(dev, WIDTH, HEIGHT, GBM_FORMAT_XRGB8888, GBM_BO_USE_RENDERING);
    }
    if (canvas == NULL) {
        printf("compose: gbm_bo_create(canvas) failed errno=%d\n", errno);
        return 1;
    }
    unsigned cstride = gbm_bo_get_stride(canvas);
    int canvas_fd = gbm_bo_get_fd(canvas);

    /* Import nothing new: the textures the surfaces were drawn through are the samplers. */
    GLuint texA = texs[0];
    GLuint texB = texs[1];

    GLuint canvas_tex = 0;
    glGenTextures(1, &canvas_tex);
    glBindTexture(GL_TEXTURE_2D, canvas_tex);
    {
        PFNEGLCREATEIMAGEKHRPROC create_image =
            (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
        PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_target =
            (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
        const EGLint cattrs[] = {
            EGL_WIDTH, WIDTH, EGL_HEIGHT, HEIGHT,
            EGL_LINUX_DRM_FOURCC_EXT, (EGLint)DRM_FORMAT_XRGB8888,
            EGL_DMA_BUF_PLANE0_FD_EXT, canvas_fd,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
            EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)cstride,
            EGL_NONE
        };
        EGLImageKHR cimg =
            create_image(dpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, cattrs);
        if (cimg == EGL_NO_IMAGE_KHR) {
            printf("compose: canvas image failed 0x%x\n", eglGetError());
            return 1;
        }
        image_target(GL_TEXTURE_2D, cimg);
    }
    GLuint fbo = 0;
    glGenFramebuffers(1, &fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, canvas_tex, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
        printf("compose: canvas FBO incomplete\n");
        return 1;
    }

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

    /* Rectangles (top-down) and their textures: A at (64,64), B at (256,64), each 64x64. */
    struct {
        GLuint tex;
        int x, y;
    } quads[2] = {{texA, 64, 64}, {texB, 256, 64}};
    for (int q = 0; q < 2; q++) {
        float l = 2.0f * quads[q].x / WIDTH - 1.0f;
        float r = 2.0f * (quads[q].x + SRC) / WIDTH - 1.0f;
        float t = 1.0f - 2.0f * quads[q].y / HEIGHT;
        float b = 1.0f - 2.0f * (quads[q].y + SRC) / HEIGHT;
        static const GLfloat pos[] = {-1, 1, 1, 1, -1, -1, 1, -1};
        GLfloat vp[8];
        for (int i = 0; i < 4; i++) {
            vp[i * 2] = pos[i * 2] < 0 ? l : r;
            vp[i * 2 + 1] = pos[i * 2 + 1] > 0 ? t : b;
        }
        static const GLfloat uv[] = {0, 0, 1, 0, 0, 1, 1, 1};
        glBindTexture(GL_TEXTURE_2D, quads[q].tex);
        glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, vp);
        glEnableVertexAttribArray(0);
        glVertexAttribPointer(1, 2, GL_FLOAT, GL_FALSE, 0, uv);
        glEnableVertexAttribArray(1);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
    glFinish();
    GLenum err = glGetError();
    printf("compose: draw err=0x%x\n", err);

    /* Through GL first: is the *composition* in the canvas at all? GL y is bottom-up, so the
     * rect at top y in [64,128) reads at y in [640,704). */
    unsigned char ga[4] = {0, 0, 0, 0}, gb[4] = {0, 0, 0, 0};
    glReadPixels(96, 672, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, ga);
    glReadPixels(288, 672, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, gb);
    printf("compose: glA=%02x%02x%02x%02x glB=%02x%02x%02x%02x\n", ga[0], ga[1], ga[2], ga[3],
           gb[0], gb[1], gb[2], gb[3]);

    /* Resolve (a one-pixel read) and transfer the canvas into its own pages, as 4d needs. */
    unsigned char one[4];
    glReadPixels(0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, one);

    uint32_t mstride = 0;
    void *cmap = NULL;
    void *cmapped = gbm_bo_map(canvas, 0, 0, WIDTH, HEIGHT, GBM_BO_TRANSFER_READ, &mstride, &cmap);
    if (cmapped != NULL) {
        uint32_t ma = *(uint32_t *)((char *)cmapped + 96 * mstride + 96 * 4);
        uint32_t mb = *(uint32_t *)((char *)cmapped + 96 * mstride + 288 * 4);
        printf("compose: mapA=%08x mapB=%08x mstride=%u\n", ma, mb, mstride);
        gbm_bo_unmap(canvas, cmap);
    }

    uint32_t a = raw_pixel(canvas, canvas_fd, cstride, 96, 96);
    uint32_t bb = raw_pixel(canvas, canvas_fd, cstride, 288, 96);
    uint32_t bg = raw_pixel(canvas, canvas_fd, cstride, 512, 400);
    printf("compose: A=%08x (want %08x) B=%08x (want %08x) bg=%08x\n", a, GREEN, bb, RED, bg);
    int ok = (a == GREEN) && (bb == RED) && (bg == 0);
    printf("compose: %s\n", ok ? "PASS" : "FAIL (the surfaces did not composite)");
    return ok ? 0 : 1;
}
