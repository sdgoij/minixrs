/* pthreads for the minix libc — 1:1 kernel threads (THREADS.md Slice 3).
 * pthread_t is an opaque handle; pthread_self() returns 0 on the main
 * thread. The C heap is not thread-safe yet — threads should not malloc
 * concurrently. */
#ifndef _PTHREAD_H
#define _PTHREAD_H

#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned long pthread_t;
typedef struct pthread_attr_t { int __x; } pthread_attr_t;

typedef struct pthread_mutexattr_t { int type; } pthread_mutexattr_t;
#define PTHREAD_MUTEX_NORMAL 0
#define PTHREAD_MUTEX_RECURSIVE 1
#define PTHREAD_MUTEX_ERRORCHECK 2
#define PTHREAD_MUTEX_DEFAULT PTHREAD_MUTEX_NORMAL

typedef struct pthread_mutex_t {
    unsigned int state;
    int kind;
    int owner;
    int count;
} pthread_mutex_t;
#define PTHREAD_MUTEX_INITIALIZER {0}

typedef struct pthread_cond_t { unsigned int seq; } pthread_cond_t;
#define PTHREAD_COND_INITIALIZER {0}
typedef struct pthread_condattr_t { int clock; } pthread_condattr_t;

typedef unsigned int pthread_key_t;
typedef int pthread_once_t;
#define PTHREAD_ONCE_INIT 0

int pthread_create(pthread_t *thread, const pthread_attr_t *attr,
                   void *(*start_routine)(void *), void *arg);
int pthread_join(pthread_t thread, void **retval);
void pthread_exit(void *retval);
pthread_t pthread_self(void);
int pthread_equal(pthread_t a, pthread_t b);
int pthread_detach(pthread_t thread);

int pthread_mutexattr_init(pthread_mutexattr_t *attr);
int pthread_mutexattr_destroy(pthread_mutexattr_t *attr);
int pthread_mutexattr_settype(pthread_mutexattr_t *attr, int type);
int pthread_mutexattr_gettype(const pthread_mutexattr_t *attr, int *type);

int pthread_mutex_init(pthread_mutex_t *mutex, const void *attr);
int pthread_mutex_destroy(pthread_mutex_t *mutex);
int pthread_mutex_lock(pthread_mutex_t *mutex);
int pthread_mutex_trylock(pthread_mutex_t *mutex);
int pthread_mutex_timedlock(pthread_mutex_t *mutex, const struct timespec *abstime);
int pthread_mutex_unlock(pthread_mutex_t *mutex);

int pthread_cond_init(pthread_cond_t *cond, const void *attr);
int pthread_cond_destroy(pthread_cond_t *cond);
int pthread_cond_wait(pthread_cond_t *cond, pthread_mutex_t *mutex);
int pthread_cond_timedwait(pthread_cond_t *cond, pthread_mutex_t *mutex,
                           const struct timespec *abstime);
int pthread_cond_signal(pthread_cond_t *cond);
int pthread_cond_broadcast(pthread_cond_t *cond);
int pthread_condattr_init(pthread_condattr_t *attr);
int pthread_condattr_destroy(pthread_condattr_t *attr);
int pthread_condattr_setclock(pthread_condattr_t *attr, clockid_t clock_id);
int pthread_condattr_getclock(const pthread_condattr_t *attr, clockid_t *clock_id);

/* Barriers: a mutex and a condition variable, with a generation counter. The
 * last thread to arrive bumps the generation and wakes everyone; only it gets
 * PTHREAD_BARRIER_SERIAL_THREAD. */
typedef struct pthread_barrier_t {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    unsigned int count;
    unsigned int waiting;
    unsigned int generation;
} pthread_barrier_t;
typedef struct pthread_barrierattr_t { int __x; } pthread_barrierattr_t;
#define PTHREAD_BARRIER_SERIAL_THREAD (-1)

int pthread_barrier_init(pthread_barrier_t *barrier,
                         const pthread_barrierattr_t *attr, unsigned int count);
int pthread_barrier_destroy(pthread_barrier_t *barrier);
int pthread_barrier_wait(pthread_barrier_t *barrier);

/* Reader/writer locks: a mutex, a condition variable, and the reader/writer
 * counts. The same mutex/cond types the rest of this header uses, so a lock
 * needs no separate init beyond zeroing. */
typedef struct pthread_rwlock_t {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    int readers;
    int writer;
} pthread_rwlock_t;
typedef struct pthread_rwlockattr_t { int __x; } pthread_rwlockattr_t;
#define PTHREAD_RWLOCK_INITIALIZER {0}

int pthread_rwlock_init(pthread_rwlock_t *rwlock, const pthread_rwlockattr_t *attr);
int pthread_rwlock_destroy(pthread_rwlock_t *rwlock);
int pthread_rwlock_rdlock(pthread_rwlock_t *rwlock);
int pthread_rwlock_tryrdlock(pthread_rwlock_t *rwlock);
int pthread_rwlock_wrlock(pthread_rwlock_t *rwlock);
int pthread_rwlock_trywrlock(pthread_rwlock_t *rwlock);
int pthread_rwlock_unlock(pthread_rwlock_t *rwlock);

/* Advisory thread naming. PM has no thread-name facility, so this accepts the
 * request and does nothing; callers (Mesa included) ignore the result. */
int pthread_setname_np(pthread_t thread, const char *name);

int pthread_once(pthread_once_t *once_control, void (*init_routine)(void));

int pthread_key_create(pthread_key_t *key, void (*destructor)(void *));
int pthread_key_delete(pthread_key_t key);
int pthread_setspecific(pthread_key_t key, const void *value);
void *pthread_getspecific(pthread_key_t key);

/* Send a signal to another thread of this process. A thread is a kernel
 * thread here (THREADS.md), so this is PM's signal delivery aimed at the
 * thread's tid. */
int pthread_kill(pthread_t thread, int sig);

#ifdef __cplusplus
}
#endif

#endif
