/* <libgen.h>: POSIX `basename`/`dirname`.
 *
 * These are the POSIX forms — they may modify their argument — which is what a
 * caller that included `<libgen.h>` asked for, as opposed to the GNU forms in
 * `<string.h>` that do not. libdrm's `xf86drm.c` calls `basename` on a
 * `realpath` result. Only the declarations live here; the definitions are the
 * libc's. */
#ifndef _LIBGEN_H
#define _LIBGEN_H

#ifdef __cplusplus
extern "C" {
#endif

char *basename(char *path);
char *dirname(char *path);

#ifdef __cplusplus
}
#endif

#endif
