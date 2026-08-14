/* Minimal sys/wait.h for the minix OS. The wait status is the raw exit
 * status PM replies (0-255); the WIF* macros decode it POSIX-style, so
 * only the success case (status 0) is exact — non-zero exits report as
 * signals until PM encodes w_exitcode-style statuses. */
#ifndef _SYS_WAIT_H
#define _SYS_WAIT_H

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

pid_t wait(int *status);
pid_t waitpid(pid_t pid, int *status, int options);
pid_t wait4(pid_t pid, int *status, int options, struct rusage *usage);

#ifdef __cplusplus
}
#endif

#endif
