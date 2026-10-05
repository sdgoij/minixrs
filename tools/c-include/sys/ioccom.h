/* The BSD spelling of <sys/ioctl.h>'s request macros.
 *
 * libdrm's `drm.h` picks its branch on `__linux__`: the Linux branch wants
 * `<linux/types.h>` and `<asm/ioctl.h>`, which this port has not got, so the
 * target (which defines neither `__linux__` nor a BSD) takes the "one of the
 * BSDs" branch instead — and that branch includes `<sys/ioccom.h>` for `_IOC`
 * and the `_IO*` family. The port's `<sys/ioctl.h>` defines exactly those, so
 * this is an alias for it rather than a second copy of the macros. */
#ifndef _SYS_IOCOM_H
#define _SYS_IOCOM_H

#include <sys/ioctl.h>

/* The direction bits named by the same BSD branch's `DRM_IOC_*` macros. The
 * names are BSD's, but the *values* are this port's (see <sys/ioctl.h>: `_IOR`
 * is 0x80000000, `_IOW` 0x40000000), so `DRM_IOCTL_*` computes the numbers the
 * port's `virtgpu` driver answers (`crates/drivers/src/video/drm.rs`), which
 * follow the Linux uapi. */
#define IOC_VOID 0x00000000u
#define IOC_OUT 0x80000000u
#define IOC_IN 0x40000000u
#define IOC_INOUT (IOC_IN | IOC_OUT)

#endif
