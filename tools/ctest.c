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
#include <dirent.h>
#include <pthread.h>
#include <sys/stat.h>

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

    /* scanf family: the field rules are the fiddly part, so every edge is
     * checked -- a `0` that is not octal, `0x` with no hex digit, an `e` with
     * no exponent digits, a scanset, a width, suppression, and %n. */
    {
        int bad = 0;
        int a = 0, b = 0, n = -1;
        char w[16];
        unsigned hex = 0;
        double d = 0.0;

#define CHECK(cond)                                                          \
        do {                                                                 \
            if (!(cond)) {                                                   \
                printf("  scanf check %d FAIL\n", __LINE__);                  \
                bad++;                                                       \
            }                                                                \
        } while (0)

        CHECK(sscanf("42 hello", "%d %s", &a, w) == 2);
        CHECK(a == 42 && strcmp(w, "hello") == 0);
        CHECK(sscanf("0x1f", "%i", &a) == 1 && a == 31);
        CHECK(sscanf("08", "%i", &a) == 1 && a == 0); /* the field is the 0 */
        CHECK(sscanf("0xg", "%x", &hex) == 1 && hex == 0);
        CHECK(sscanf("hello!", "%3s", w) == 1 && strcmp(w, "hel") == 0);
        CHECK(sscanf("abc123", "%[a-c]", w) == 1 && strcmp(w, "abc") == 0);
        CHECK(sscanf("7 8", "%*d %d", &b) == 1 && b == 8);
        CHECK(sscanf("1e", "%lf", &d) == 1 && d == 1.0);
        CHECK(sscanf("2.5e2", "%lf", &d) == 1 && d == 250.0);
        CHECK(sscanf("12ab", "%d%n", &a, &n) == 1 && a == 12 && n == 2);
        CHECK(sscanf("abc", "%d", &a) == 0);
        CHECK(sscanf("", "%d", &a) == EOF);

        /* the same rules over a stream, which goes through the FILE path */
        FILE *sf = fopen("ctest-scan.txt", "w");
        CHECK(sf != NULL);
        if (sf) {
            CHECK(fprintf(sf, "7 1f\n") == 5);
            fclose(sf);
            sf = fopen("ctest-scan.txt", "r");
            CHECK(sf != NULL);
            if (sf) {
                unsigned v1 = 0, v2 = 0;
                CHECK(fscanf(sf, "%u %x", &v1, &v2) == 2);
                CHECK(v1 == 7 && v2 == 0x1f);
                fclose(sf);
            }
        }
#undef CHECK
        printf("scanf: %s\n", bad == 0 ? "ok" : "FAIL");
    }

    /* mkfifo: the VFS mknod path, reached the way a C program reaches it. */
    {
        const char *name = "ctest-fifo";
        struct stat st;
        unlink(name);
        int made = mkfifo(name, 0600);
        int statted = made == 0 ? stat(name, &st) : -1;
        int ok = made == 0 && statted == 0 && S_ISFIFO(st.st_mode) != 0;
        printf("mkfifo: %s\n", ok ? "ok" : "FAIL");
        if (ok)
            unlink(name);
    }

    /* cwd: `getcwd` is a stub (it returns ENOSYS), and bash reports on it
       at startup. Print the parts a `..` walk needs — stat's dev/ino for "."
       and "..", and readdir's d_ino — so the one that is wrong is visible
       rather than inferred from an empty path. */
    {
        char cwdbuf[64];
        struct stat dot;
        struct stat dotdot;
        int s_dot = stat(".", &dot);
        int s_dotdot = stat("..", &dotdot);
        DIR *d = opendir("..");
        struct dirent *e = d ? readdir(d) : NULL;

        printf("cwd: strerror(ENOSYS)=%s\n", strerror(ENOSYS));
        errno = 0;
        char *got = getcwd(cwdbuf, sizeof cwdbuf);
        printf("cwd: getcwd=%s errno=%d\n", got ? got : "(null)", errno);
        /* The allocate form, which is the one bash asks for. */
        char *heap = getcwd(NULL, 0);
        printf("cwd: getcwd(NULL,0)=%s\n", heap ? heap : "(null)");
        free(heap);
        printf("cwd: stat(\".\")=%d dev=%lu ino=%lu\n", s_dot,
               (unsigned long)dot.st_dev, (unsigned long)dot.st_ino);
        printf("cwd: stat(\"..\")=%d dev=%lu ino=%lu\n", s_dotdot,
               (unsigned long)dotdot.st_dev, (unsigned long)dotdot.st_ino);
        printf("cwd: readdir(\"..\") first=%s\n", e ? e->d_name : "(none)");
        if (e)
            printf("cwd: readdir ino=%lu\n", (unsigned long)e->d_ino);
        if (d)
            closedir(d);
    }

    /* environ + getenv, exercised through a re-exec with an explicit
       environment: this shell has no `export`, so the only way ctest can be
       handed a variable is to exec itself with one — which is also the path
       bash takes when it runs a command with an environment. */
    if (argc >= 2 && strcmp(argv[1], "re-exec") == 0) {
        char *value = getenv("CTESTENV");
        printf("getenv: %s\n", value ? value : "unset");
        return 0;
    } else {
        char *envp[2] = { (char *)"CTESTENV=hello", NULL };
        char *child_argv[3] = { argv[0], (char *)"re-exec", NULL };
        execve(argv[0], child_argv, envp);
        printf("getenv: execve failed errno=%d\n", errno);
    }

    puts("ctest done");
    return 0;
}
