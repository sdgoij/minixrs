/* Minimal wctype.h for the minix OS — wint_t comes via wchar.h. */
#ifndef _WCTYPE_H
#define _WCTYPE_H

#include <wchar.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned int wctype_t;
typedef unsigned int wctrans_t;

int iswalnum(wint_t c);
int iswalpha(wint_t c);
int iswblank(wint_t c);
int iswcntrl(wint_t c);
int iswdigit(wint_t c);
int iswgraph(wint_t c);
int iswlower(wint_t c);
int iswprint(wint_t c);
int iswpunct(wint_t c);
int iswspace(wint_t c);
int iswupper(wint_t c);
int iswxdigit(wint_t c);
wint_t towlower(wint_t c);
wint_t towupper(wint_t c);

wctype_t wctype(const char *name);
wctrans_t wctrans(const char *name);
int iswctype(wint_t wc, wctype_t desc);
wint_t towctrans(wint_t wc, wctrans_t desc);

#ifdef __cplusplus
}
#endif

#endif
