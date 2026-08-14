/* Minimal locale.h for the minix OS — C locale only. The `*_l` variants
 * (BSD-style, what libc++'s locale fallbacks call) ignore the locale
 * argument. `locale_t` is an opaque handle; 0 is the "C" locale. */
#ifndef _LOCALE_H
#define _LOCALE_H

#include <stddef.h>
#include <time.h>
#include <wchar.h>
#include <wctype.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef void *locale_t;

struct lconv {
    char *decimal_point;
    char *thousands_sep;
    char *grouping;
    char *int_curr_symbol;
    char *currency_symbol;
    char *mon_decimal_point;
    char *mon_thousands_sep;
    char *mon_grouping;
    char *positive_sign;
    char *negative_sign;
    char int_frac_digits;
    char frac_digits;
    char p_cs_precedes;
    char p_sep_by_space;
    char n_cs_precedes;
    char n_sep_by_space;
    char p_sign_posn;
    char n_sign_posn;
    char int_p_cs_precedes;
    char int_p_sep_by_space;
    char int_n_cs_precedes;
    char int_n_sep_by_space;
    char int_p_sign_posn;
    char int_n_sign_posn;
};

#define LC_CTYPE 0
#define LC_NUMERIC 1
#define LC_TIME 2
#define LC_COLLATE 3
#define LC_MONETARY 4
#define LC_MESSAGES 5
#define LC_ALL 6

#define LC_CTYPE_MASK (1 << LC_CTYPE)
#define LC_NUMERIC_MASK (1 << LC_NUMERIC)
#define LC_TIME_MASK (1 << LC_TIME)
#define LC_COLLATE_MASK (1 << LC_COLLATE)
#define LC_MONETARY_MASK (1 << LC_MONETARY)
#define LC_MESSAGES_MASK (1 << LC_MESSAGES)
#define LC_ALL_MASK 0x7f

#define LC_GLOBAL_LOCALE ((locale_t)-1)

char *setlocale(int category, const char *locale);
struct lconv *localeconv(void);
locale_t newlocale(int category_mask, const char *locale, locale_t base);
locale_t uselocale(locale_t newloc);
void freelocale(locale_t loc);

/* *_l variants (C locale only). */
float strtof_l(const char *nptr, char **endptr, locale_t loc);
double strtod_l(const char *nptr, char **endptr, locale_t loc);
long double strtold_l(const char *nptr, char **endptr, locale_t loc);
int strcoll_l(const char *s1, const char *s2, locale_t loc);
size_t strxfrm_l(char *dest, const char *src, size_t n, locale_t loc);
int toupper_l(int c, locale_t loc);
int tolower_l(int c, locale_t loc);
int wcscoll_l(const wchar_t *s1, const wchar_t *s2, locale_t loc);
size_t wcsxfrm_l(wchar_t *dest, const wchar_t *src, size_t n, locale_t loc);
int iswctype_l(wint_t wc, wctype_t desc, locale_t loc);
int iswspace_l(wint_t wc, locale_t loc);
int iswprint_l(wint_t wc, locale_t loc);
int iswcntrl_l(wint_t wc, locale_t loc);
int iswupper_l(wint_t wc, locale_t loc);
int iswlower_l(wint_t wc, locale_t loc);
int iswalpha_l(wint_t wc, locale_t loc);
int iswblank_l(wint_t wc, locale_t loc);
int iswdigit_l(wint_t wc, locale_t loc);
int iswpunct_l(wint_t wc, locale_t loc);
int iswxdigit_l(wint_t wc, locale_t loc);
wint_t towupper_l(wint_t wc, locale_t loc);
wint_t towlower_l(wint_t wc, locale_t loc);
size_t strftime_l(char *s, size_t max, const char *format, const struct tm *tm, locale_t loc);

#ifdef __cplusplus
}
#endif

#endif
