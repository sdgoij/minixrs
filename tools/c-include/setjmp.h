/* Minimal setjmp.h for the minix OS. `jmp_buf` holds whatever the calling
 * convention requires to survive a jump, so its length is per arch and must
 * match `JmpBuf` in crates/minix-libc/src/c_setjmp.rs:
 *   x86_64  — rbx, rbp, r12-r15, rsp, rip
 *   aarch64 — x19-x28, x29 (fp), x30 (lr), sp, d8-d15
 *   riscv64 — ra, sp, s0-s11, fs0-fs11 */
#ifndef _SETJMP_H
#define _SETJMP_H

#ifdef __cplusplus
extern "C" {
#endif

#if defined(__x86_64__)
typedef long jmp_buf[8];
#elif defined(__aarch64__)
typedef long jmp_buf[21];
#elif defined(__riscv)
typedef long jmp_buf[26];
#else
#error "setjmp.h: no jmp_buf shape is defined for this target"
#endif

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
