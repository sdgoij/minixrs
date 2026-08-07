/* C smoke test for the minix libc: errno, malloc family, stdio, strings.
 * Built by tools/build-c-hello.py and embedded as /bin/ctest.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <fcntl.h>

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

    puts("ctest done");
    return 0;
}
