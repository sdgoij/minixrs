/* The request-macro shim libdrm's `drm.h` needs on this target.
 *
 * `drm.h` picks its branch on `__linux__`: the Linux branch wants
 * `<linux/types.h>` and `<asm/ioctl.h>`, which this port has not got, so the
 * target (which defines neither `__linux__` nor a BSD) takes the "one of the
 * BSDs" branch, which includes `<sys/ioccom.h>`. Everything below exists so that
 * libdrm's `DRM_IOCTL_*` numbers are the ones the port's DRM render node answers
 * (`crates/drivers/src/video/drm.rs`, the Linux uapi).
 *
 * `drmCommandRead`/`Write`/`WriteRead` (`xf86drm.c`) build a request from
 * `DRM_IOC(DRM_IOC_READ|WRITE, ...)`, which `xf86drm.h` maps onto these `IOC_*`
 * direction names — so their values carry Linux's bits (read is 0x80000000). */
#ifndef _SYS_IOCOM_H
#define _SYS_IOCOM_H

#include <sys/ioctl.h>

#define IOC_VOID 0x00000000u
#define IOC_OUT 0x80000000u
#define IOC_IN 0x40000000u
#define IOC_INOUT (IOC_IN | IOC_OUT)

/* `drm.h` builds its `DRM_IOCTL_*` from the `_IO*` family instead, which
 * <sys/ioctl.h> supplies in NetBSD's encoding (its `_IOR` is 0x40000000) — the
 * form the tty and socket requests the rest of the tree serves need, so that
 * header is left alone. But the DRM requests are the Linux numbers, with the
 * read direction in bit 31 and the write direction in bit 30. `drm.h` is the
 * only thing that includes this header, so the family is redefined here. */
#undef _IO
#undef _IOR
#undef _IOW
#undef _IOWR

#define _IO(t, nr) _IOC(0x00000000u, t, nr, 0)
#define _IOR(t, nr, s) _IOC(0x80000000u, t, nr, sizeof(s))
#define _IOW(t, nr, s) _IOC(0x40000000u, t, nr, sizeof(s))
#define _IOWR(t, nr, s) _IOC(0xc0000000u, t, nr, sizeof(s))

#endif
