/* langinfo.h for the minix OS: the C locale's character-encoding query.
 *
 * libc++'s localization support reaches `nl_langinfo_l(CODESET, loc)` through
 * its BSD fallback path. The port has a single C locale, so every item reports
 * the same thing and only `CODESET` is meaningful.
 */
#ifndef _LANGINFO_H
#define _LANGINFO_H

#include <locale.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef int nl_item;

/* The one item libc++ asks for. The number matches glibc's; it is opaque to a
 * caller and this implementation accepts it by name. */
#define CODESET 14

char *nl_langinfo(nl_item item);
char *nl_langinfo_l(nl_item item, locale_t loc);

#ifdef __cplusplus
}
#endif

#endif
