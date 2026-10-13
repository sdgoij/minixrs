/* The 4d probe (`.notes/wayland.md` §6.10, stage 3d-b-4d, and §5 row 26): can a GBM buffer
 * object on the render node be turned into a GLES2 render target, and does the render reach
 * the object's own pages — the ones the compositor imports and the fb server scans out?
 *
 * `/bin/gbmtest` (3d-b-3) proved a GBM BO exports; `/bin/wlgl` (3d-a) proved GLES2 runs on the
 * node. What this measures is the join: binding the BO as a texture through an `EGLImage` and
 * rendering into it.
 *
 * Mesa is configured `-Dplatforms=` (empty), so there is no `EGL_PLATFORM_GBM_KHR` display —
 * the only route from a BO to a texture is `EGL_LINUX_DMA_BUF_EXT` over the BO's descriptor.
 *
 * The probe reads the object's pages twice, both the way `wlserver` reads them
 * (`PRIME_FD_TO_HANDLE` + `VIRTGPU_MAP` + `mmap`): once right after `glFinish` — measured
 * BLACK, because on this host virgl renders into a host-side texture — and once after Mesa's
 * `gbm_bo_map`. `glReadPixels` shows the triangle too. The second read is the whole point: it
 * is what says the frame reaches the scanned-out pages only via a transfer, which is the
 * defence for the client-side transfer `/bin/wldmabufgl` does (§5 row 26).
 *
 * It is a dev-host probe: it needs the GL host the virgl gates need. Runs as a normal dynamic
 * minix image (built by `tools/build-eglprobe.py`).
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include <gbm.h>
#include <xf86drm.h>
#include <virtgpu_drm.h>

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>

#define NODE "/dev/dri/renderD128"

/* `DRM_FORMAT_XRGB8888`, from `drm_fourcc.h`. Declared rather than included: the value is
 * part of the kernel ABI, and the probe's whole point is the ioctl path, not the header. */
#define DRM_FORMAT_XRGB8888 0x34325258u

#define WIDTH 1024
#define HEIGHT 768

static int
has_ext(const char *exts, const char *want)
{
   size_t n = strlen(want);
   const char *p = exts;
   while (p && *p) {
      const char *end = strchr(p, ' ');
      size_t len = end ? (size_t)(end - p) : strlen(p);
      if (len == n && memcmp(p, want, n) == 0)
         return 1;
      p = end ? end + 1 : NULL;
   }
   return 0;
}

int
main(void)
{
   int fd = open(NODE, O_RDWR | O_CLOEXEC);
   if (fd < 0) {
      printf("eglbo: open %s failed\n", NODE);
      return 1;
   }
   struct gbm_device *dev = gbm_create_device(fd);
   if (dev == NULL) {
      printf("eglbo: gbm_create_device failed\n");
      return 1;
   }
   struct gbm_bo *bo = gbm_bo_create(dev, WIDTH, HEIGHT, GBM_FORMAT_XRGB8888,
                                     GBM_BO_USE_RENDERING | GBM_BO_USE_LINEAR);
   if (bo == NULL) {
      printf("eglbo: gbm_bo_create failed\n");
      return 1;
   }
   uint32_t stride = gbm_bo_get_stride(bo);
   int bo_fd = gbm_bo_get_fd(bo);
   printf("eglbo: bo stride=%u fd=%d\n", stride, bo_fd);
   if (bo_fd < 0) {
      printf("eglbo: gbm_bo_get_fd failed\n");
      return 1;
   }

   /* The client extension string is available before a display is. */
   const char *cext = eglQueryString(EGL_NO_DISPLAY, EGL_EXTENSIONS);
   printf("eglbo: client dmabuf_import=%d\n",
          cext && has_ext(cext, "EGL_EXT_image_dma_buf_import"));

   setenv("LIBGL_DRIVERS_PATH", "/lib", 1);
   setenv("EGL_PLATFORM", "surfaceless", 1);

   EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
   if (dpy == EGL_NO_DISPLAY) {
      printf("eglbo: eglGetDisplay failed\n");
      return 1;
   }
   EGLint major = 0, minor = 0;
   if (!eglInitialize(dpy, &major, &minor)) {
      printf("eglbo: eglInitialize failed\n");
      return 1;
   }
   const char *dext = eglQueryString(dpy, EGL_EXTENSIONS);
   printf("eglbo: display dmabuf_import=%d khr_image=%d modifiers=%d\n",
          dext && has_ext(dext, "EGL_EXT_image_dma_buf_import"),
          dext && has_ext(dext, "EGL_KHR_image_base"),
          dext && has_ext(dext, "EGL_EXT_image_dma_buf_import_modifiers"));

   PFNEGLCREATEIMAGEKHRPROC create_image =
       (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
   PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_target =
       (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress(
           "glEGLImageTargetTexture2DOES");
   printf("eglbo: procs create=%d target=%d\n", create_image != NULL,
          image_target != NULL);
   if (!create_image || !image_target) {
      printf("eglbo: FAIL (no EGLImage entry points)\n");
      return 1;
   }

   if (!eglBindAPI(EGL_OPENGL_ES_API)) {
      printf("eglbo: eglBindAPI failed\n");
      return 1;
   }
   const EGLint config_attrs[] = {
       EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
       EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
       EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
       EGL_NONE};
   EGLConfig config;
   EGLint num_config = 0;
   if (!eglChooseConfig(dpy, config_attrs, &config, 1, &num_config) ||
       num_config < 1) {
      printf("eglbo: eglChooseConfig failed\n");
      return 1;
   }
   const EGLint context_attrs[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
   EGLContext ctx = eglCreateContext(dpy, config, EGL_NO_CONTEXT, context_attrs);
   if (ctx == EGL_NO_CONTEXT) {
      printf("eglbo: eglCreateContext failed\n");
      return 1;
   }
   const EGLint pbuffer_attrs[] = {EGL_WIDTH, 16, EGL_HEIGHT, 16, EGL_NONE};
   EGLSurface surf = eglCreatePbufferSurface(dpy, config, pbuffer_attrs);
   if (surf == EGL_NO_SURFACE || !eglMakeCurrent(dpy, surf, surf, ctx)) {
      printf("eglbo: make-current failed\n");
      return 1;
   }
   printf("eglbo: GL_RENDERER=%s\n", (const char *)glGetString(GL_RENDERER));

   /* The join under test: the BO's descriptor becomes an EGLImage, which becomes a texture.
    * Linear modifier, plane 0 only — what `gbm_bo_create` yields and linux-dmabuf advertises. */
   const EGLint img_attrs[] = {
       EGL_WIDTH, WIDTH,
       EGL_HEIGHT, HEIGHT,
       EGL_LINUX_DRM_FOURCC_EXT, (EGLint)DRM_FORMAT_XRGB8888,
       EGL_DMA_BUF_PLANE0_FD_EXT, bo_fd,
       EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
       EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)stride,
       EGL_NONE};
   EGLImageKHR img = create_image(dpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL,
                                  img_attrs);
   if (img == EGL_NO_IMAGE_KHR) {
      printf("eglbo: eglCreateImageKHR(dmabuf) failed 0x%x\n", eglGetError());
      printf("eglbo: FAIL (no dma-buf image)\n");
      return 1;
   }
   printf("eglbo: image=1\n");

   GLuint tex = 0;
   glGenTextures(1, &tex);
   glBindTexture(GL_TEXTURE_2D, tex);
   image_target(GL_TEXTURE_2D, img);
   GLenum e = glGetError();
   printf("eglbo: image_target err=0x%x\n", e);
   if (e != GL_NO_ERROR) {
      printf("eglbo: FAIL (image is not a texture)\n");
      return 1;
   }

   GLuint fbo = 0;
   glGenFramebuffers(1, &fbo);
   glBindFramebuffer(GL_FRAMEBUFFER, fbo);
   glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
   if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
      printf("eglbo: FAIL (fbo incomplete 0x%x)\n", glCheckFramebufferStatus(GL_FRAMEBUFFER));
      return 1;
   }
   glViewport(0, 0, WIDTH, HEIGHT);
   glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
   glClear(GL_COLOR_BUFFER_BIT);
   /* One big red quad, so a readback at the centre is unambiguous. */
   glEnable(GL_SCISSOR_TEST);
   glScissor(0, 0, WIDTH, HEIGHT);
   glClearColor(1.0f, 0.0f, 0.0f, 1.0f);
   glClear(GL_COLOR_BUFFER_BIT);
   glDisable(GL_SCISSOR_TEST);
   glFinish();
   GLenum ferr = glGetError();
   printf("eglbo: render err=0x%x\n", ferr);

   /* The raw frames exactly as `wlserver` (PRIME_FD_TO_HANDLE + VIRTGPU_MAP + mmap) sees them,
    * read *before* `glReadPixels` and before Mesa's map: either could transfer the host texture
    * into the pages and mask whether the render landed there in place. A red raw centre is
    * in-place rendering; black means the content only appears after a later operation. */
   {
      unsigned handle = 0;
      int rawfd = open(NODE, O_RDWR);
      if (rawfd >= 0 && drmPrimeFDToHandle(rawfd, bo_fd, &handle) == 0) {
         struct drm_virtgpu_map m = {0, handle, 0};
         if (drmIoctl(rawfd, DRM_IOCTL_VIRTGPU_MAP, &m) == 0) {
            void *rp = mmap(NULL, (size_t)WIDTH * HEIGHT * 4, PROT_READ, MAP_SHARED, rawfd,
                            (off_t)m.offset);
            if (rp != MAP_FAILED) {
               uint32_t c = *(uint32_t *)((char *)rp + (HEIGHT / 2) * (WIDTH * 4) +
                                          (WIDTH / 2) * 4);
               printf("eglbo: raw(before readback) centre=%08x\n", c);
               munmap(rp, (size_t)WIDTH * HEIGHT * 4);
            } else {
               printf("eglbo: raw mmap failed\n");
            }
         } else {
            printf("eglbo: raw VIRTGPU_MAP failed\n");
         }
      } else {
         printf("eglbo: raw import failed\n");
      }
   }

   unsigned char px[4] = {0, 0, 0, 0};
   glReadPixels(WIDTH / 2, HEIGHT / 2, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
   printf("eglbo: centre=%02x%02x%02x%02x\n", px[0], px[1], px[2], px[3]);

   uint32_t mstride = 0;
   void *mbo = NULL;
   void *mapped = gbm_bo_map(bo, 0, 0, WIDTH, HEIGHT, GBM_BO_TRANSFER_READ, &mstride, &mbo);
   if (mapped == NULL) {
      printf("eglbo: bo_map failed errno=%d\n", errno);
   } else {
      uint32_t world = *(uint32_t *)((char *)mapped +
                                     (HEIGHT / 2) * mstride + (WIDTH / 2) * 4);
      printf("eglbo: mapped stride=%u centre=%08x\n", mstride, world);
      gbm_bo_unmap(bo, mbo);
   }

   /* The pages again, after Mesa's map: if the map transferred into the object's own backing these
    * are now the triangle; if it only read into a staging buffer they are still black. */
   {
      unsigned handle = 0;
      int rawfd = open(NODE, O_RDWR);
      if (rawfd >= 0 && drmPrimeFDToHandle(rawfd, bo_fd, &handle) == 0) {
         struct drm_virtgpu_map m = {0, handle, 0};
         if (drmIoctl(rawfd, DRM_IOCTL_VIRTGPU_MAP, &m) == 0) {
            void *rp = mmap(NULL, (size_t)WIDTH * HEIGHT * 4, PROT_READ, MAP_SHARED, rawfd,
                            (off_t)m.offset);
            if (rp != MAP_FAILED) {
               uint32_t c = *(uint32_t *)((char *)rp + (HEIGHT / 2) * (WIDTH * 4) +
                                          (WIDTH / 2) * 4);
               printf("eglbo: raw(after map) centre=%08x\n", c);
               munmap(rp, (size_t)WIDTH * HEIGHT * 4);
            }
         }
      }
   }

   if (ferr == GL_NO_ERROR && px[0] == 0xff && px[1] == 0x00 && px[2] == 0x00)
      printf("eglbo: PASS\n");
   else
      printf("eglbo: FAIL (render did not land in the image)\n");

   close(bo_fd);
   gbm_bo_destroy(bo);
   gbm_device_destroy(dev);
   close(fd);
   return 0;
}
