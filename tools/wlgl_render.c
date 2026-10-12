/* The GL half of /bin/wlgl (.notes/wayland.md §6.10 stage 3d-a): an unmodified-Mesa
 * GLES2 triangle rendered through surfaceless EGL and read back into the caller's
 * buffer.
 *
 * The buffer is the wl_shm pool the client shares with /sbin/wlserver, so the
 * readback *is* the frame the compositor presents — this file knows nothing about
 * Wayland, and the protocol half lives in `crates/userland`'s `wlclient` module.
 *
 * The pixels come out in the XRGB8888 order both the pool and the framebuffer use
 * (little-endian B,G,R,X), so GL's RGBA is swizzled on the way out.
 *
 * Which driver it lands on is decided by whether the guest has a render node: with
 * one, surfaceless EGL takes the `virtio_gpu` driver (virgl); without, it is forced
 * onto `softpipe`. The probe is the same call Mesa makes, so the driver choice does
 * not depend on the shell passing an environment variable.
 *
 * A dynamic image is required — a static minix program has no loader to bring Mesa's
 * DSOs in with (`tools/build-wlgl.py` links this object into `/bin/wlgl`).
 */
#include <EGL/egl.h>
#include <GLES2/gl2.h>

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

static const char *vertex_src =
    "attribute vec2 pos;\n"
    "void main() { gl_Position = vec4(pos, 0.0, 1.0); }\n";

static const char *fragment_src =
    "precision mediump float;\n"
    "void main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

/* The readback buffer, sized for the largest surface the clients use. */
static unsigned char rgba[256 * 256 * 4];

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
        printf("wlgl: shader compile failed: %s\n", log);
        exit(1);
    }
    return sh;
}

/* Render a red triangle on black into `dst`, `w`x`h`, XRGB8888. Returns 0. */
int wlgl_render(unsigned char *dst, int w, int h)
{
    if (w <= 0 || h <= 0 || (long)w * h * 4 > (long)sizeof(rgba)) {
        printf("wlgl: surface is not a supported size\n");
        return 1;
    }

    int node = open("/dev/dri/renderD128", O_RDWR);
    if (node < 0)
        setenv("LIBGL_ALWAYS_SOFTWARE", "1", 1);
    else
        close(node);
    setenv("LIBGL_DRIVERS_PATH", "/lib", 1);
    setenv("EGL_PLATFORM", "surfaceless", 1);

    EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (dpy == EGL_NO_DISPLAY) {
        printf("wlgl: eglGetDisplay returned no display\n");
        return 1;
    }
    EGLint major = 0, minor = 0;
    if (!eglInitialize(dpy, &major, &minor)) {
        printf("wlgl: eglInitialize failed\n");
        return 1;
    }
    if (!eglBindAPI(EGL_OPENGL_ES_API)) {
        printf("wlgl: eglBindAPI failed\n");
        return 1;
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
        printf("wlgl: eglChooseConfig failed\n");
        return 1;
    }

    const EGLint context_attrs[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
    EGLContext ctx = eglCreateContext(dpy, config, EGL_NO_CONTEXT, context_attrs);
    if (ctx == EGL_NO_CONTEXT) {
        printf("wlgl: eglCreateContext failed\n");
        return 1;
    }

    const EGLint pbuffer_attrs[] = { EGL_WIDTH, w, EGL_HEIGHT, h, EGL_NONE };
    EGLSurface surf = eglCreatePbufferSurface(dpy, config, pbuffer_attrs);
    if (surf == EGL_NO_SURFACE) {
        printf("wlgl: eglCreatePbufferSurface failed\n");
        return 1;
    }
    if (!eglMakeCurrent(dpy, surf, surf, ctx)) {
        printf("wlgl: eglMakeCurrent failed\n");
        return 1;
    }
    printf("wlgl: GL_RENDERER=%s\n", (const char *)glGetString(GL_RENDERER));

    GLuint prog = glCreateProgram();
    glAttachShader(prog, compile(GL_VERTEX_SHADER, vertex_src));
    glAttachShader(prog, compile(GL_FRAGMENT_SHADER, fragment_src));
    glBindAttribLocation(prog, 0, "pos");
    glLinkProgram(prog);
    GLint linked = 0;
    glGetProgramiv(prog, GL_LINK_STATUS, &linked);
    if (!linked) {
        printf("wlgl: program link failed\n");
        return 1;
    }
    glUseProgram(prog);

    glViewport(0, 0, w, h);
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

    glReadPixels(0, 0, w, h, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
    /* GL hands back RGBA; the pool wants B,G,R,X. */
    for (int i = 0; i < w * h; i++) {
        dst[i * 4 + 0] = rgba[i * 4 + 2];
        dst[i * 4 + 1] = rgba[i * 4 + 1];
        dst[i * 4 + 2] = rgba[i * 4 + 0];
        dst[i * 4 + 3] = 0;
    }
    return 0;
}
