/* C smoke test for the Minix fork toolchain + minix-libc.
 *
 * Freestanding: no headers — the syscall surface comes from
 * `crates/minix-libc` (C ABI wrappers over minix-std), resolved at link
 * time. Compiled with clang for a freestanding x86_64 target and linked
 * with the fork's stage1 rustc as the driver (see `build-c-hello.py`).
 */

typedef unsigned long size_t;
typedef long ssize_t;

extern ssize_t write(int fd, const void *buf, size_t count);
extern void exit(int status);

int main(int argc, char **argv) {
    const char msg[] = "hello from C\n";
    (void)argc;
    (void)argv;
    write(1, msg, sizeof(msg) - 1);
    exit(0);
    return 0; /* not reached */
}
