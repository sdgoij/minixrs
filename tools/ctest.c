/* C smoke test for the minix libc: errno, malloc family, stdio, strings,
 * pthreads (1:1 kernel threads) with per-thread errno.
 * Built by tools/build-c-hello.py and embedded as /bin/ctest.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <fcntl.h>
#include <pthread.h>

static int counter = 0;
static pthread_mutex_t counter_lock = PTHREAD_MUTEX_INITIALIZER;

static void *worker(void *arg) {
    long id = (long)arg;
    /* per-thread errno: each thread must see its own slot */
    errno = 100 + (int)id;
    int my_errno = errno;
    pthread_mutex_lock(&counter_lock);
    counter++;
    int c = counter;
    pthread_mutex_unlock(&counter_lock);
    printf("  worker %ld: errno=%d tid=%lu counter=%d\n", id, my_errno,
           pthread_self(), c);
    return (void *)(id * 7);
}

int main(int argc, char **argv) {
    printf("ctest: argc=%d argv0=%s\n", argc, argv[0]);

    /* malloc family */
    int *p = malloc(10 * sizeof(int));
    if (!p) {
        puts("malloc failed");
        return 1;
    }
    for (int i = 0; i < 10; i++) p[i] = i * i;
    printf("heap: p[9]=%d p=%p\n", p[9], (void *)p);

    p = realloc(p, 20 * sizeof(int));
    if (!p) {
        puts("realloc failed");
        return 1;
    }
    p[19] = 42;
    printf("realloc: p[9]=%d p[19]=%d\n", p[9], p[19]);
    free(p);

    int *z = calloc(4, sizeof(int));
    printf("calloc: zero=%d\n", z[3]);
    free(z);

    /* strings */
    printf("strings: strlen=%zu cmp=%d chr=%s\n", strlen("hello"),
           strcmp("abc", "abd"), strchr("minix", 'n'));
    char buf[16];
    strcpy(buf, "copied");
    printf("strcpy=%s %s\n", buf, strcmp(buf, "copied") == 0 ? "ok" : "FAIL");

    /* errno + open of a missing file */
    errno = 0;
    int fd = open("/nonexistent", O_RDONLY);
    printf("open: fd=%d errno=%d %s\n", fd, errno,
           errno == ENOENT ? "enoent-ok" : "FAIL");

    /* numeric formatting */
    printf("fmt: %d %u %x %X %ld %zu %p %c %%\n", -42, 300u, 0xbeef,
           0xbeef, -7L, (size_t)3, (void *)p, 'Q');
    printf("%05d|%-5d|\n", 42, 42);

    /* pthreads: 4 threads, per-thread errno, mutex-protected counter */
    {
        pthread_t th[4];
        for (long i = 0; i < 4; i++) {
            if (pthread_create(&th[i], NULL, worker, (void *)i) != 0) {
                printf("pthread_create(%ld) failed errno=%d\n", i, errno);
                return 1;
            }
        }
        errno = 0;
        for (int i = 0; i < 4; i++) {
            void *ret = NULL;
            if (pthread_join(th[i], &ret) != 0) {
                printf("pthread_join(%d) failed errno=%d\n", i, errno);
                return 1;
            }
            printf("  joined %d ret=%ld\n", i, (long)ret);
        }
        printf("pthread: counter=%d (expected 4) main_errno=%d %s\n", counter,
               errno, counter == 4 && errno == 0 ? "ok" : "FAIL");
    }

    puts("ctest done");
    return 0;
}
