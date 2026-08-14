/* Minimal signal.h for the minix OS. */
#ifndef _SIGNAL_H
#define _SIGNAL_H

#include <setjmp.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned long sigset_t[2];

typedef void (*sighandler_t)(int);

#define SIG_DFL ((sighandler_t)0)
#define SIG_IGN ((sighandler_t)1)
#define SIG_ERR ((sighandler_t)-1)

#define SIGHUP 1
#define SIGINT 2
#define SIGQUIT 3
#define SIGILL 4
#define SIGTRAP 5
#define SIGABRT 6
#define SIGBUS 7
#define SIGFPE 8
#define SIGKILL 9
#define SIGUSR1 10
#define SIGSEGV 11
#define SIGUSR2 12
#define SIGPIPE 13
#define SIGALRM 14
#define SIGTERM 15
#define SIGCHLD 20
#define SIGWINCH 28
#define SIGSYS 31

#define SIG_BLOCK 0
#define SIG_UNBLOCK 1
#define SIG_SETMASK 2

/* Signal disposition flags (compile-time only; the minix kernel's signal
 * delivery does not interpret SA_* yet). */
#define SA_NOCLDSTOP 0x00000001
#define SA_NOCLDWAIT 0x00000002
#define SA_SIGINFO 0x00000004
#define SA_ONSTACK 0x08000000
#define SA_RESTART 0x10000000
#define SA_NODEFER 0x40000000
#define SA_RESETHAND 0x80000000

/* Minimal siginfo_t — the fields LLVM's signal handlers read. */
typedef struct {
    int si_signo;
    int si_errno;
    int si_code;
    union {
        int si_pid;
        void *si_addr;
    };
} siginfo_t;

/* ABI match for minix-libc's sigaction: handler u64@0, mask 16 bytes@8,
 * flags i32@24 (28 bytes). The handler/sigaction union keeps the ABI. */
struct sigaction {
    union {
        sighandler_t sa_handler;
        void (*sa_sigaction)(int, siginfo_t *, void *);
    } __sa_union;
    sigset_t sa_mask;
    int sa_flags;
    void (*sa_restorer)(void);
};
#define sa_handler __sa_union.sa_handler
#define sa_sigaction __sa_union.sa_sigaction

sighandler_t signal(int signum, sighandler_t handler);
int sigaction(int signum, const struct sigaction *act, struct sigaction *oldact);
int sigprocmask(int how, const sigset_t *set, sigset_t *oldset);
int raise(int sig);
int kill(pid_t pid, int sig);
char *strsignal(int sig);

int sigemptyset(sigset_t *set);
int sigfillset(sigset_t *set);
int sigaddset(sigset_t *set, int signum);
int sigdelset(sigset_t *set, int signum);
int sigismember(const sigset_t *set, int signum);

typedef jmp_buf sigjmp_buf;
#define sigsetjmp(env, savemask) setjmp(env)
#define siglongjmp(env, val) longjmp(env, val)

#ifdef __cplusplus
}
#endif

#endif
