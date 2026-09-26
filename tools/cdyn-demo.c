/* A program of the kind a user writes, to exercise `just cdyn`.
 *
 * `printf`, `open`, `strerror` and `errno` all come from the shared C library, so the
 * line only appears if the loader ran, resolved those symbols against `/lib/libc.so`,
 * and gave the program the thread-local storage `errno` lives in. Nothing in this file
 * computes the message the scenario asserts on, which is why the gate can refuse a
 * program that contains it.
 *
 * Unlike `tools/dynclib.c` it is not a program the image ships and carries none of that
 * one's probe scaffolding (no constructor, no hold mode): it is what a user's first
 * program against `libc.so` looks like.
 *
 * Freestanding, like `tools/hello.c`: the declarations below must match `minix-libc`.
 */

typedef int c_int;

extern int printf(const char *fmt, ...);
extern c_int open(const char *path, c_int flags, c_int mode);
extern char *strerror(c_int errnum);
extern c_int *__errno_location(void);

int main(void) {
    /* A path that cannot exist, so the failure is what sets `errno`. */
    if (open("/no-such-file", 0, 0) >= 0) {
        printf("cdyn-bad open succeeded\n");
        return 1;
    }
    printf("cdyn-ok %s\n", strerror(*__errno_location()));
    return 0;
}
