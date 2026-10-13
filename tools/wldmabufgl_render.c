/* The GL half of /bin/wldmabufgl (.notes/wayland.md §6.10, stage 3d-b-4d): render a GLES2
 * triangle into a GBM buffer object on the render node and hand back the object's dma-buf
 * descriptor, so the buffer the compositor scans out *is* the buffer GL drew into — no CPU
 * readback, which is what separates 4d from 3d-a's `glReadPixels` into a wl_shm pool.
 *
 * Mesa here is built with no window-system platforms (`-Dplatforms=`), so there is no
 * `EGL_PLATFORM_GBM_KHR` display and no EGL surface over a BO. The route from the BO to a
 * texture is `gbm_bo_get_fd` -> `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` ->
 * `glEGLImageTargetTexture2DOES`, then an FBO over that texture. `tools/egl_bo_probe.c`
 * measured both halves on the port's node: the image binds and renders, and the pixels are
 * in the BO's own memory (readable through `gbm_bo_map`), which is what lets the fb server
 * scan the same frames out with no copy.
 *
 * The buffer is the whole output (1024x768, XRGB8888, linear stride) because the compositor
 * scan-out path is taken only for a full-output, cursor-free commit. The driver choice is
 * the same as `/bin/wlgl`'s: a render node means surfaceless EGL takes `virtio_gpu`
 * (virgl); without one it is forced onto `softpipe`.
 *
 * A dynamic image is required (Mesa's DSOs); `tools/build-wldmabufgl.py` links this object
 * into `/bin/wldmabufgl`. The protocol half lives in `crates/userland`'s `wlclient` module,
 * which calls `wldmabufgl_render` and never touches GL.
 */
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#include <gbm.h>

#define NODE "/dev/dri/renderD128"
#define WIDTH 1024
#define HEIGHT 768

static const char *vertex_src =
    "attribute vec2 pos;\n"
    "void main() { gl_Position = vec4(pos, 0.0, 1.0); }\n";

static const char *fragment_src =
    "precision mediump float;\n"
    "void main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

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
        printf("wldmabufgl: shader compile failed: %s\n", log);
        exit(1);
    }
    return sh;
}

/* Render the triangle into `bo`, then return nothing; the caller exports. Fills the FBO,
 * not a pbuffer, so the pixels land in the BO. */
static void draw_into(struct gbm_bo *bo, int bo_fd, unsigned stride)
{
    setenv("LIBGL_DRIVERS_PATH", "/lib", 1);
    setenv("EGL_PLATFORM", "surfaceless", 1);

    EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (dpy == EGL_NO_DISPLAY) {
        printf("wldmabufgl: eglGetDisplay returned no display\n");
        exit(1);
    }
    EGLint major = 0, minor = 0;
    if (!eglInitialize(dpy, &major, &minor)) {
        printf("wldmabufgl: eglInitialize failed\n");
        exit(1);
    }
    if (!eglBindAPI(EGL_OPENGL_ES_API)) {
        printf("wldmabufgl: eglBindAPI failed\n");
        exit(1);
    }

    const EGLint config_attrs[] = {
        EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
        EGL_NONE
    };
    EGLConfig config;
    EGLint num_config = 0;
    if (!eglChooseConfig(dpy, config_attrs, &config, 1, &num_config) || num_config < 1) {
        printf("wldmabufgl: eglChooseConfig failed\n");
        exit(1);
    }
    const EGLint context_attrs[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
    EGLContext ctx = eglCreateContext(dpy, config, EGL_NO_CONTEXT, context_attrs);
    if (ctx == EGL_NO_CONTEXT) {
        printf("wldmabufgl: eglCreateContext failed\n");
        exit(1);
    }
    /* A context needs *a* surface current; the render target is the FBO, so this one is a stub. */
    const EGLint pbuffer_attrs[] = { EGL_WIDTH, 16, EGL_HEIGHT, 16, EGL_NONE };
    EGLSurface surf = eglCreatePbufferSurface(dpy, config, pbuffer_attrs);
    if (surf == EGL_NO_SURFACE || !eglMakeCurrent(dpy, surf, surf, ctx)) {
        printf("wldmabufgl: eglMakeCurrent failed\n");
        exit(1);
    }
    printf("wldmabufgl: GL_RENDERER=%s\n", (const char *)glGetString(GL_RENDERER));

    PFNEGLCREATEIMAGEKHRPROC create_image =
        (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_target =
        (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    if (!create_image || !image_target) {
        printf("wldmabufgl: no EGLImage entry points\n");
        exit(1);
    }

    /* The BO's descriptor, back as an EGLImage: linear, plane 0 only — what gbm and the
     * linux-dmabuf commit agree on. */
    const EGLint img_attrs[] = {
        EGL_WIDTH, WIDTH,
        EGL_HEIGHT, HEIGHT,
        EGL_LINUX_DRM_FOURCC_EXT, (EGLint)0x34325258 /* DRM_FORMAT_XRGB8888 */,
        EGL_DMA_BUF_PLANE0_FD_EXT, bo_fd,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)stride,
        EGL_NONE
    };
    EGLImageKHR img = create_image(dpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL,
                                   img_attrs);
    if (img == EGL_NO_IMAGE_KHR) {
        printf("wldmabufgl: eglCreateImageKHR failed 0x%x\n", eglGetError());
        exit(1);
    }
    GLuint tex = 0;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    image_target(GL_TEXTURE_2D, img);
    if (glGetError() != GL_NO_ERROR) {
        printf("wldmabufgl: the image is not a texture\n");
        exit(1);
    }

    GLuint fbo = 0;
    glGenFramebuffers(1, &fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
        printf("wldmabufgl: the framebuffer is incomplete\n");
        exit(1);
    }

    GLuint prog = glCreateProgram();
    glAttachShader(prog, compile(GL_VERTEX_SHADER, vertex_src));
    glAttachShader(prog, compile(GL_FRAGMENT_SHADER, fragment_src));
    glBindAttribLocation(prog, 0, "pos");
    glLinkProgram(prog);
    GLint linked = 0;
    glGetProgramiv(prog, GL_LINK_STATUS, &linked);
    if (!linked) {
        printf("wldmabufgl: program link failed\n");
        exit(1);
    }
    glUseProgram(prog);

    glViewport(0, 0, WIDTH, HEIGHT);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    static const GLfloat verts[] = {
        -0.5f, -0.5f,
         0.5f, -0.5f,
         0.0f,  0.5f,
    };
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, verts);
    glEnableVertexAttribArray(0);
    glDrawArrays(GL_TRIANGLES, 0, 3);
    glFinish();
    /* One pixel, read back for its *effect*, not its value: on this Mesa the FBO render resolves
     * into the `EGLImage`-backed texture only on a read, and the transfer in `wldmabufgl_render`
     * reads that texture. `glFlush` does not do it (`tools/egl_bo_probe.c` measures both). One
     * pixel is not the frame; a whole-frame `glReadPixels` is exactly what 4d exists to avoid. */
    unsigned char resolved[4];
    glReadPixels(WIDTH / 2, HEIGHT / 2, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, resolved);
    (void)bo;
}

/* Render the triangle into a new full-output GBM object on the render node and return its
 * exported dma-buf descriptor: 0 on success, writing the descriptor to `*out_fd` and the
 * geometry to `*out_w`/`*out_h`/`*out_stride`; non-zero on failure, after printing why. The
 * BO, the gbm device and the node descriptor are deliberately not torn down: the object has
 * to outlive the commit, and the process ends after the compositor releases the buffer. */
int wldmabufgl_render(int *out_fd, unsigned *out_w, unsigned *out_h, unsigned *out_stride)
{
    int node = open(NODE, O_RDWR | O_CLOEXEC);
    if (node < 0) {
        setenv("LIBGL_ALWAYS_SOFTWARE", "1", 1);
    }
    struct gbm_device *dev = gbm_create_device(node);
    if (dev == NULL) {
        printf("wldmabufgl: gbm_create_device failed\n");
        return 1;
    }
    /* LINEAR because the linux-dmabuf commit advertises the linear modifier, and RENDERING
     * because it is a GL target. */
    struct gbm_bo *bo = gbm_bo_create(dev, WIDTH, HEIGHT, GBM_FORMAT_XRGB8888,
                                      GBM_BO_USE_RENDERING | GBM_BO_USE_LINEAR);
    if (bo == NULL) {
        printf("wldmabufgl: gbm_bo_create failed\n");
        return 1;
    }
    unsigned stride = gbm_bo_get_stride(bo);
    int bo_fd = gbm_bo_get_fd(bo);
    if (bo_fd < 0) {
        printf("wldmabufgl: gbm_bo_get_fd failed\n");
        return 1;
    }

    draw_into(bo, bo_fd, stride);

    /* The one transfer this path lives with on this host (`.notes/wayland.md` §5, 4d's deviation):
     * the port's virgl renders into a *host-side* texture, and the object's guest pages — the pages
     * the compositor imports and the fb server scans out — only hold the frame after a
     * `TRANSFER_FROM_HOST_3D`. `dri_gbm`'s `gbm_bo_map` is that transfer (it reads the host texture
     * back into the object), so `tools/egl_bo_probe.c` measures the pages black before it and the
     * triangle after. Everything past this point — export, import, scan-out — copies nothing; the
     * copy is here, once. */
    uint32_t mstride = 0;
    void *mbo = NULL;
    if (gbm_bo_map(bo, 0, 0, WIDTH, HEIGHT, GBM_BO_TRANSFER_READ, &mstride, &mbo) == NULL) {
        printf("wldmabufgl: the transfer back into the object failed\n");
        return 1;
    }
    gbm_bo_unmap(bo, mbo);

    *out_fd = bo_fd;
    *out_w = WIDTH;
    *out_h = HEIGHT;
    *out_stride = stride;
    return 0;
}
