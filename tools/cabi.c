/* The C libc-ABI probe.
 *
 * Compiled against `tools/c-include` and linked against the port's `minix-libc`,
 * so it meets both sides of every ABI it checks — which the Rust userland
 * binaries cannot, because they go through `minix-std` and never touch these
 * symbols or these struct layouts.
 *
 * Three phases, each one a place where a header and an implementation can drift
 * apart without any compiler noticing:
 *
 *   1. `sockaddr_in`: build one from the header, bind it, read it back out of
 *      getsockname, and check the family and port survived. `getsockopt` with
 *      `SO_TYPE` runs in the same phase: the stack discovers a socket's family
 *      from the ioctls it answers, so the type must come back as the one the
 *      socket was created with.
 *   2. `pipe2(O_NONBLOCK)`: the flag has to reach the open file description the
 *      pipe's read path consults (VFS `F_SETFL`), so `F_GETFL` must report it
 *      and a read on the empty pipe must answer EAGAIN rather than block.
 *   3. `mmap(MAP_ANONYMOUS)`: the pointer must be usable and `munmap` must take
 *      it back.
 *
 * The last line is printed only after all three passed, so it is what a gate
 * asserts; each failure prints its own diagnostic instead.
 */
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <netinet/in.h>

static int check_sockaddr(void)
{
    int s = socket(AF_INET, SOCK_STREAM, 0);
    if (s < 0) {
        printf("cabi: socket failed errno=%d\n", errno);
        return 1;
    }

    struct sockaddr_in sin;
    memset(&sin, 0, sizeof sin);
    sin.sin_family = AF_INET;
    sin.sin_port = htons(23456);
    sin.sin_addr.s_addr = htonl(INADDR_ANY);

    if (bind(s, (struct sockaddr *)&sin, sizeof sin) != 0) {
        printf("cabi: bind failed errno=%d\n", errno);
        close(s);
        return 1;
    }

    struct sockaddr_in got;
    memset(&got, 0, sizeof got);
    socklen_t len = sizeof got;
    if (getsockname(s, (struct sockaddr *)&got, &len) != 0) {
        printf("cabi: getsockname failed errno=%d\n", errno);
        close(s);
        return 1;
    }
    /* The one socket option the stack can answer: the family each socket
       responds to is discoverable from its ioctls, so SO_TYPE must come back as
       the type the socket was created with. */
    int kind = 0;
    socklen_t kind_len = sizeof kind;
    if (getsockopt(s, SOL_SOCKET, SO_TYPE, &kind, &kind_len) != 0 ||
        kind != SOCK_STREAM) {
        printf("cabi: getsockopt SO_TYPE kind=%d errno=%d\n", kind, errno);
        close(s);
        return 1;
    }
    close(s);

    unsigned port = ntohs(got.sin_port);
    unsigned addr = ntohl(got.sin_addr.s_addr);
    if (got.sin_family != AF_INET || port != 23456 || addr != 0) {
        printf("cabi: sockaddr family=%d port=%u addr=0x%x\n", (int)got.sin_family,
               port, addr);
        return 1;
    }
    printf("cabi: sockaddr family=%d port=%u type=%d ok\n", (int)got.sin_family,
           port, kind);
    return 0;
}

static int check_pipe2(void)
{
    int p[2];
    if (pipe2(p, O_NONBLOCK) != 0) {
        printf("cabi: pipe2 failed errno=%d\n", errno);
        return 1;
    }
    int fl = fcntl(p[0], F_GETFL, 0);
    char c;
    errno = 0;
    ssize_t n = read(p[0], &c, 1);
    int e = errno;
    close(p[0]);
    close(p[1]);
    if (fl < 0 || (fl & O_NONBLOCK) == 0 || n != -1 || e != EAGAIN) {
        printf("cabi: pipe2 fl=%d read=%ld errno=%d\n", fl, (long)n, e);
        return 1;
    }
    printf("cabi: pipe2 O_NONBLOCK ok\n");
    return 0;
}

static int check_mmap(void)
{
    size_t len = 4096;
    char *p = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS,
                   -1, 0);
    if (p == MAP_FAILED) {
        printf("cabi: mmap failed errno=%d\n", errno);
        return 1;
    }
    /* Anonymous memory must start zeroed and then be writable and readable. */
    if (p[0] != 0 || p[len - 1] != 0) {
        printf("cabi: mmap not zero-filled\n");
        munmap(p, len);
        return 1;
    }
    p[0] = 0x5A;
    p[len - 1] = 0xA5;
    if (p[0] != 0x5A || p[len - 1] != (char)0xA5) {
        printf("cabi: mmap write did not read back\n");
        munmap(p, len);
        return 1;
    }
    if (munmap(p, len) != 0) {
        printf("cabi: munmap failed errno=%d\n", errno);
        return 1;
    }
    printf("cabi: anon mmap ok\n");
    return 0;
}

int main(void)
{
    if (check_sockaddr() != 0)
        return 1;
    if (check_pipe2() != 0)
        return 1;
    if (check_mmap() != 0)
        return 1;
    printf("cabi: socket layout, pipe2 flags and anon mmap ok\n");
    return 0;
}
