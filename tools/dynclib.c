/* The dynamically linked C program: linked against `libc.so`, not the static
 * library.
 *
 * Every function it calls — `open`, `printf`, `strerror`, `__errno_location`,
 * and the `exit` its `crt0` ends on — is a symbol `libc.so` has to define and
 * the loader has to resolve before `main` runs. None of them is in this file.
 *
 * Two of them are more than code, and they are why the line below is evidence
 * of the whole chain rather than of a single call:
 *
 *   - `errno` is a `#[thread_local]` in `libc.so`, so reading it goes through
 *     the `__tls_get_addr` the loader supplies, against a block the loader laid
 *     out from that object's own `PT_TLS` and installed before anything ran;
 *   - `strerror` returns a pointer into a buffer inside `libc.so`, so the message
 *     printed here is that object's data, reached through a relocation the loader
 *     applied.
 *
 * The `ctor` field is this program's *own* constructor, and it is the third
 * thing the line covers: `crt0` walks this image's `.init_array` itself, which is
 * what a dynamically linked program used to lose — the call into the C library
 * resolved into the shared object, whose array is the object's own and empty.
 * The field is 0 unless a constructor ran, so it fails on its own if that walk
 * goes missing again. `volatile` because `-O2` otherwise folds away a static only
 * a constructor writes.
 *
 * The message is therefore deliberately *not* in this file, and
 * `just test-dynlink-x86` fails the gate if the executable contains it.
 *
 * Freestanding, like `tools/hello.c`: the declarations below must match
 * `minix-libc`.
 */

typedef int c_int;

extern int printf(const char *fmt, ...);
extern c_int open(const char *path, c_int flags, c_int mode);
extern char *strerror(c_int errnum);
extern c_int *__errno_location(void);

static volatile c_int ctor_ran;

__attribute__((constructor)) static void program_ctor(void) {
    ctor_ran = 1;
}

int main(void) {
    /* A path that cannot exist, so the failure is what sets `errno` — reading it
     * afterwards is what exercises the loader's thread-local storage. Opening
     * `/dev/null` in the same program would prove nothing about either. */
    if (open("/no-such-file", 0, 0) >= 0) {
        printf("libc-dyn-bad open succeeded\n");
        return 1;
    }
    c_int e = *__errno_location();
    printf("libc-dyn-ok errno=%d msg=%s ctor=%d\n", e, strerror(e), ctor_ran);
    return 0;
}
