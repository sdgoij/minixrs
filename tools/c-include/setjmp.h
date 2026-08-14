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

#ifdef __cplusplus
}
#endif

#endif
