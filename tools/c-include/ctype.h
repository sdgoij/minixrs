/* Minimal ctype.h for the minix OS — ASCII-only. */
#ifndef _CTYPE_H
#define _CTYPE_H

/* Classic BSD ctype bit classes; libc++'s default rune table
 * (ctype_base under _LIBCPP_PROVIDES_DEFAULT_RUNE_TABLE) composes its
 * masks from these. */
#define _U 0x01 /* upper */
#define _L 0x02 /* lower */
#define _N 0x04 /* digit */
#define _S 0x08 /* space */
#define _P 0x10 /* punct */
#define _C 0x20 /* cntrl */
#define _X 0x40 /* xdigit */
#define _B 0x80 /* blank */

#ifdef __cplusplus
extern "C" {
#endif

int isalnum(int c);
int isalpha(int c);
int isblank(int c);
int iscntrl(int c);
int isdigit(int c);
int isgraph(int c);
int islower(int c);
int isprint(int c);
int ispunct(int c);
int isspace(int c);
int isupper(int c);
int isxdigit(int c);
int tolower(int c);
int toupper(int c);

#ifdef __cplusplus
}
#endif

#endif
