/* pthreads for the minix libc — 1:1 kernel threads (THREADS.md Slice 3).
 * pthread_t is an opaque handle; pthread_self() returns 0 on the main
 * thread. The C heap is not thread-safe yet — threads should not malloc
 * concurrently. */
#ifndef _PTHREAD_H
#define _PTHREAD_H

typedef unsigned long pthread_t;
typedef struct pthread_attr_t { int __x; } pthread_attr_t;
typedef struct pthread_mutex_t { unsigned int state; } pthread_mutex_t;
#define PTHREAD_MUTEX_INITIALIZER {0}

int pthread_create(pthread_t *thread, const pthread_attr_t *attr,
                   void *(*start_routine)(void *), void *arg);
int pthread_join(pthread_t thread, void **retval);
void pthread_exit(void *retval);
pthread_t pthread_self(void);
int pthread_equal(pthread_t a, pthread_t b);
int pthread_detach(pthread_t thread);
int pthread_mutex_init(pthread_mutex_t *mutex, const void *attr);
int pthread_mutex_destroy(pthread_mutex_t *mutex);
int pthread_mutex_lock(pthread_mutex_t *mutex);
int pthread_mutex_unlock(pthread_mutex_t *mutex);

#endif
