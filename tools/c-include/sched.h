/* Minimal sched.h for the minix OS — declared for ABI compatibility; the
 * scheduler interfaces are not supported. */
#ifndef _SCHED_H
#define _SCHED_H

#ifdef __cplusplus
extern "C" {
#endif

/* Yield the CPU to another runnable thread (kernel `SYS_thread_yield`). The rest
 * of the scheduler interface (`sched_setscheduler`, priorities, ...) is not
 * supported. */
int sched_yield(void);

#ifdef __cplusplus
}
#endif

#endif
