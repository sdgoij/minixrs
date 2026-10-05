/* A GLES2 triangle through surfaceless EGL on the software rasteriser.
 *
 * This is §6.10's 3c-2 client: it opens no DRM node, renders into a pbuffer with
 * `softpipe`, reads the framebuffer back and checks the triangle landed. It is
 * the first thing to run on top of the Mesa DSOs (`just build-mesa`), so it sets
 * the software/loader environment itself (setenv, before EGL init) and prints
 * one `pass`/`fail` line the boot scenario can match.
 *
 * The triangle is red on black: the centroid's readback must be red and a corner
 * must be black. A renderer that did nothing, or drew nothing, fails both. */
#include <EGL/egl.h>
#include <GLES2/gl2.h>

#include <stdio.h>
#include <stdlib.h>

#define W 64
#define H 64

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
        printf("gltriangle: shader compile failed: %s\n", log);
        exit(1);
    }
    return sh;
}

int main(void)
{
    setenv("LIBGL_ALWAYS_SOFTWARE", "1", 1);
    setenv("LIBGL_DRIVERS_PATH", "/lib", 1);
    setenv("EGL_PLATFORM", "surfaceless", 1);

    EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (dpy == EGL_NO_DISPLAY) {
        printf("gltriangle: eglGetDisplay returned no display\n");
        return 1;
    }
    EGLint major = 0, minor = 0;
    if (!eglInitialize(dpy, &major, &minor)) {
        printf("gltriangle: eglInitialize failed (0x%04x)\n", eglGetError());
        return 1;
    }
    printf("gltriangle: EGL %d.%d vendor=%s\n", major, minor,
           eglQueryString(dpy, EGL_VENDOR));

    if (!eglBindAPI(EGL_OPENGL_ES_API)) {
        printf("gltriangle: eglBindAPI failed\n");
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
        printf("gltriangle: eglChooseConfig failed\n");
        return 1;
    }

    const EGLint context_attrs[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
    EGLContext ctx = eglCreateContext(dpy, config, EGL_NO_CONTEXT, context_attrs);
    if (ctx == EGL_NO_CONTEXT) {
        printf("gltriangle: eglCreateContext failed (0x%04x)\n", eglGetError());
        return 1;
    }

    const EGLint pbuffer_attrs[] = { EGL_WIDTH, W, EGL_HEIGHT, H, EGL_NONE };
    EGLSurface surf = eglCreatePbufferSurface(dpy, config, pbuffer_attrs);
    if (surf == EGL_NO_SURFACE) {
        printf("gltriangle: eglCreatePbufferSurface failed (0x%04x)\n", eglGetError());
        return 1;
    }

    if (!eglMakeCurrent(dpy, surf, surf, ctx)) {
        printf("gltriangle: eglMakeCurrent failed (0x%04x)\n", eglGetError());
        return 1;
    }
    printf("gltriangle: GL_VERSION=%s\n", (const char *)glGetString(GL_VERSION));
    printf("gltriangle: GL_RENDERER=%s\n", (const char *)glGetString(GL_RENDERER));

    GLuint prog = glCreateProgram();
    glAttachShader(prog, compile(GL_VERTEX_SHADER, vertex_src));
    glAttachShader(prog, compile(GL_FRAGMENT_SHADER, fragment_src));
    glBindAttribLocation(prog, 0, "pos");
    glLinkProgram(prog);
    GLint linked = 0;
    glGetProgramiv(prog, GL_LINK_STATUS, &linked);
    if (!linked) {
        printf("gltriangle: program link failed\n");
        return 1;
    }
    glUseProgram(prog);

    glViewport(0, 0, W, H);
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

    unsigned char px[W * H * 4];
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    const unsigned char *center = &px[((H / 2) * W + (W / 2)) * 4];
    const unsigned char *corner = &px[0];

    int center_red = center[0] > 200 && center[1] < 50 && center[2] < 50;
    int corner_black = corner[0] < 50 && corner[1] < 50 && corner[2] < 50;
    printf("gltriangle: center=%u,%u,%u,%u corner=%u,%u,%u,%u\n",
           center[0], center[1], center[2], center[3],
           corner[0], corner[1], corner[2], corner[3]);

    if (center_red && corner_black) {
        printf("gltriangle: pass\n");
        return 0;
    }
    printf("gltriangle: fail\n");
    return 1;
}
