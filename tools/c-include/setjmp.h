/* Minimal setjmp.h for the minix OS — SysV x86_64 (rbx, rbp, r12-r15,
 * rsp, rip). */
#ifndef _SETJMP_H
#define _SETJMP_H

#ifdef __cplusplus
extern "C" {
#endif

typedef long jmp_buf[8];

int setjmp(jmp_buf env);
void longjmp(jmp_buf env, int val);
#define _setjmp setjmp
#define _longjmp longjmp

/* POSIX puts the signal-safe jump pair in <setjmp.h>, and a program that
 * includes only this header reaches for it there (bash's posixjmp.h does).
 * The port has no mask-restoring variant, so both forms are the plain pair. */
typedef jmp_buf sigjmp_buf;
#define sigsetjmp(env, savemask) setjmp(env)
#define siglongjmp(env, val) longjmp(env, val)

#ifdef __cplusplus
}
#endif

#endif
