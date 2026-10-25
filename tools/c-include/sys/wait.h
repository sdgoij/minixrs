/* Minimal sys/wait.h for the minix OS. The wait status is
 * w_exitcode-encoded, as POSIX expects: the WIF* macros and
 * WEXITSTATUS decode it directly. */
#ifndef _SYS_WAIT_H
#define _SYS_WAIT_H

#include <signal.h>
#include <sys/types.h>
#include <sys/resource.h>

#ifdef __cplusplus
extern "C" {
#endif

#define WNOHANG 1
#define WUNTRACED 2
#define WCONTINUED 8

#define WIFEXITED(s) (((s) & 0x7f) == 0)
#define WIFSIGNALED(s) (((s) & 0x7f) != 0 && ((s) & 0x7f) != 0x7f)
#define WIFSTOPPED(s) (((s) & 0xff) == 0x7f)
#define WEXITSTATUS(s) (((s) >> 8) & 0xff)
#define WTERMSIG(s) ((s) & 0x7f)
#define WSTOPSIG(s) WEXITSTATUS(s)
#define WCOREDUMP(s) ((s) & 0x80)
/* No continued status exists yet: PM has no stop/continue encoding (see the
 * note above), so a child is never reported as having continued. */
#define WIFCONTINUED(s) (0)

pid_t wait(int *status);
pid_t waitpid(pid_t pid, int *status, int options);
pid_t wait4(pid_t pid, int *status, int options, struct rusage *usage);

/* waitid(2). Only WEXITED is backed by PM: there is no stop/continue
 * encoding, and a reported child is always reaped, so WSTOPPED, WCONTINUED
 * and WNOWAIT are refused with EINVAL. */
#define WEXITED 0x00000004
#define WSTOPPED 0x00000002
#define WNOWAIT 0x01000000

#define P_ALL 0
#define P_PID 1
#define P_PGID 2

#define CLD_EXITED 1
#define CLD_KILLED 2
#define CLD_DUMPED 3
#define CLD_TRAPPED 4
#define CLD_STOPPED 5
#define CLD_CONTINUED 6

int waitid(int idtype, int id, siginfo_t *infop, int options);

#ifdef __cplusplus
}
#endif

#endif
