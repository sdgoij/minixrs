/* The EGL bootstrap 4d needs, written in C: a *GLES 3* context on the surfaceless platform.
 *
 * smithay cannot give us one. Its configless `EGLContext::new` pins
 * `EGL_CONTEXT_CLIENT_VERSION = 2`, and its config'd path asks the display for `WINDOW_BIT`
 * configs, which a surfaceless display does not have (`NoAvailablePixelFormat`) — while the
 * device-display path asks for `STREAM_BIT_KHR` and gets `BadAttribute`. `ExportMem` for the
 * GLES renderer needs GLES 3 (`glReadBuffer`, `PIXEL_PACK_BUFFER`), so the context is created
 * here with the port's proven surfaceless recipe (`tools/gl_triangle.c`) and handed back as raw
 * handles for `EGLDisplay::from_raw`/`EGLContext::from_raw` to wrap.
 *
 * Returns 0, or a negative code naming the step that failed, with `eglGetError()` in `error`.
 * Compiling this needs only the Mesa EGL/GLES headers and the port's include set; the link
 * resolves `egl*` through `libEGL.so.1` and `glGetString` through `libGLESv2.so.2` (the driver
 * that actually ran is the one fact a bare `PASS` line would hide).
 */
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>

struct glgate_egl {
	EGLDisplay display;
	EGLConfig config;
	EGLContext context;
	EGLSurface surface;
	/* `eglGetError()` from the step that failed, for the caller to print. */
	int error;
	/* Diagnostics, so a failed step reports what each path answered without the C side
	 * needing stdio (a gate prints from Rust). */
	int have_platform_display;
	int platform_error;
	int env_error;
	int egl_major;
	int egl_minor;
	/* `glGetString` of the current context: the vendor, driver and GL version that ran. */
	char gl_vendor[64];
	char gl_renderer[64];
	char gl_version[64];
	/* Whether the display advertises the two extensions dmabuf import needs: the plain import
	 * (`EGL_EXT_image_dma_buf_import`) and its format/modifier query, which is the only source
	 * of the format list a `zwp_linux_dmabuf_v1` global can advertise. */
	int have_dmabuf_import;
	int have_dmabuf_modifiers;
};

static void copy_gl_string(char *dst, size_t len, GLenum name)
{
	const unsigned char *src = glGetString(name);
	size_t i;

	if (src == NULL)
		src = (const unsigned char *)"unknown";
	for (i = 0; i + 1 < len && src[i] != '\0'; i++)
		dst[i] = (char)src[i];
	dst[i] = '\0';
}

/* The surfaceless platform by its own extension first, and by `EGL_PLATFORM` second.
 *
 * The environment variable is what `tools/gl_triangle.c` uses, but it is read once, at the
 * first EGL call — so a process that has already touched EGL (smithay's loader does, through
 * `dlopen`) may not get it in time. The extension call has no such ordering. */
static EGLDisplay open_surfaceless(struct glgate_egl *out)
{
	PFNEGLGETPLATFORMDISPLAYEXTPROC get_platform_display =
		(PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
	EGLDisplay dpy;

	out->have_platform_display = get_platform_display != NULL;
	if (get_platform_display != NULL) {
		dpy = get_platform_display(EGL_PLATFORM_SURFACELESS_MESA,
		                           (void *)EGL_DEFAULT_DISPLAY, NULL);
		out->platform_error = eglGetError();
		if (dpy != EGL_NO_DISPLAY)
			return dpy;
	}

	setenv("EGL_PLATFORM", "surfaceless", 1);
	dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
	out->env_error = eglGetError();
	return dpy;
}

int glgate_egl_bootstrap(struct glgate_egl *out, int width, int height)
{
	EGLint major = 0, minor = 0, num = 0;
	EGLConfig config;
	EGLContext ctx;
	EGLSurface surf;
	EGLDisplay dpy;

	memset(out, 0, sizeof(*out));

	dpy = open_surfaceless(out);
	if (dpy == EGL_NO_DISPLAY) {
		out->error = eglGetError();
		return -1;
	}

	if (!eglInitialize(dpy, &major, &minor)) {
		out->error = eglGetError();
		return -2;
	}

	{
		const char *ext = eglQueryString(dpy, EGL_EXTENSIONS);
		out->have_dmabuf_import = ext != NULL && strstr(ext, "EGL_EXT_image_dma_buf_import") != NULL;
		out->have_dmabuf_modifiers = ext != NULL &&
		                              strstr(ext, "EGL_EXT_image_dma_buf_import_modifiers") != NULL;
	}

	out->egl_major = (int)major;
	out->egl_minor = (int)minor;

	if (!eglBindAPI(EGL_OPENGL_ES_API)) {
		out->error = eglGetError();
		return -3;
	}

	/* `EGL_OPENGL_ES3_BIT` as the *config* filter: Mesa refuses to create an ES3 context on
	 * a config that does not advertise ES3, so the config has to be asked for by that bit. */
	{
		const EGLint config_attrs[] = {
			EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
			EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT,
			EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8,
			EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
			EGL_NONE
		};
		if (!eglChooseConfig(dpy, config_attrs, &config, 1, &num) || num < 1) {
			out->error = eglGetError();
			return -4;
		}
	}

	{
		const EGLint context_attrs[] = { EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE };
		ctx = eglCreateContext(dpy, config, EGL_NO_CONTEXT, context_attrs);
		if (ctx == EGL_NO_CONTEXT) {
			out->error = eglGetError();
			return -5;
		}
	}

	{
		const EGLint pbuffer_attrs[] = {
			EGL_WIDTH, width, EGL_HEIGHT, height, EGL_NONE
		};
		surf = eglCreatePbufferSurface(dpy, config, pbuffer_attrs);
		if (surf == EGL_NO_SURFACE) {
			out->error = eglGetError();
			return -6;
		}
	}

	if (!eglMakeCurrent(dpy, surf, surf, ctx)) {
		out->error = eglGetError();
		return -7;
	}

	out->display = dpy;
	out->config = config;
	out->context = ctx;
	out->surface = surf;

	copy_gl_string(out->gl_vendor, sizeof(out->gl_vendor), GL_VENDOR);
	copy_gl_string(out->gl_renderer, sizeof(out->gl_renderer), GL_RENDERER);
	copy_gl_string(out->gl_version, sizeof(out->gl_version), GL_VERSION);
	return 0;
}
