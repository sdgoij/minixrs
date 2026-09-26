/* A shared object with a thread-local, which the loader gives its own module.
 *
 * `xkbcommon`, `libinput` and Mesa all keep `__thread` state, so this is the shape a library
 * the C graphics stack brings has: a `PT_TLS` in an object that is loaded *after* `libc.so`,
 * which means a module of its own at a slot the layout reserved.
 *
 * The counter starts at 7 rather than 0, and that is the assertion: a thread's copy is made
 * from *this object's* init image, so a `dlsym`'d read before anything writes it must say 7.
 * Zero would mean the slot was never initialised, or that the copy came from nowhere; the
 * wrong number would mean the storage belonged to another module.
 *
 * `dynopen tls` is what checks it, together with `errno` in the same run — a thread-local in
 * `libc.so`, which is the *other* module, and which must not move when this one is loaded.
 *
 * Freestanding — see `libdyn.c`.
 */

static __thread int tls_counter = 7;

int tls_read(void) {
    return tls_counter;
}

int tls_bump(void) {
    return ++tls_counter;
}
