/* Minimal strings.h for the minix OS — the case-insensitive comparison family
 * and the BSD legacy names. */
#ifndef _STRINGS_H
#define _STRINGS_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

int strcasecmp(const char *a, const char *b);
int strncasecmp(const char *a, const char *b, size_t n);

void bcopy(const void *src, void *dst, size_t n);
void bzero(void *dst, size_t n);
int bcmp(const void *a, const void *b, size_t n);
char *index(const char *s, int c);
char *rindex(const char *s, int c);
int ffs(int i);

#ifdef __cplusplus
}
#endif

#endif
