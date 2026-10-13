/* /bin/gbmtest — a GBM buffer object made on the render node, exported to a dma-buf and
 * imported back through the same node (`.notes/wayland.md` §6.10, stage 3d-b-3).
 *
 * GBM is how a compositor — and Mesa's virgl winsys — allocates a buffer object on a DRM
 * device without an EGL surface. What this proves is the whole of gbm's create + export on
 * the port's render node: `gbm_bo_create` makes the node allocate a resource,
 * `gbm_bo_get_fd` exports it (`DRM_IOCTL_PRIME_HANDLE_TO_FD`), and the descriptor it returns
 * is one the *same node* accepts back (`DRM_IOCTL_PRIME_FD_TO_HANDLE`). The import is what
 * makes it more than a number: only a real dma-buf over a node object turns into a handle.
 */

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <unistd.h>

#include <gbm.h>
#include <xf86drm.h>

#define NODE "/dev/dri/renderD128"

int
main(void)
{
   int fd = open(NODE, O_RDWR | O_CLOEXEC);
   if (fd < 0) {
      printf("gbmtest: open %s failed\n", NODE);
      return 1;
   }

   struct gbm_device *dev = gbm_create_device(fd);
   if (dev == NULL) {
      printf("gbmtest: gbm_create_device failed\n");
      return 1;
   }

   struct gbm_bo *bo = gbm_bo_create(dev, 64, 64, GBM_FORMAT_XRGB8888,
                                     GBM_BO_USE_RENDERING);
   if (bo == NULL) {
      printf("gbmtest: gbm_bo_create failed\n");
      gbm_device_destroy(dev);
      return 1;
   }

   uint32_t w = gbm_bo_get_width(bo);
   uint32_t h = gbm_bo_get_height(bo);
   uint32_t stride = gbm_bo_get_stride(bo);

   /* The export: a dma-buf descriptor over the node's object. Negative is "this BO has no
    * dma-buf" — a software BO, or a node without PRIME. */
   int bo_fd = gbm_bo_get_fd(bo);
   int import = -1;
   uint32_t handle = 0;
   if (bo_fd >= 0)
      import = drmPrimeFDToHandle(fd, bo_fd, &handle);

   printf("gbmtest: dev=1 bo=1 w=%u h=%u import=%d\n", w, h, import);
   /* stride, handle and fd are values, not claims: they must not share the verdict's line, which
    * the smoke scenario matches as a whole line ending at `import`. */
   printf("gbmtest: stride=%u handle=%u fd=%d\n", stride, handle, bo_fd);

   if (bo_fd >= 0)
      close(bo_fd);
   gbm_bo_destroy(bo);
   gbm_device_destroy(dev);
   close(fd);
   return 0;
}
