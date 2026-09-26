/* A shared object with a thread-local, which the loader refuses to `dlopen`.
 *
 * One thread-local module is all it places, and it lays that out at startup, before any
 * thread exists. An object loaded later that brings its own `PT_TLS` would need every
 * thread's block re-laid out, so the load is refused by name rather than mapped and silently
 * wrong (`crates/ldso/src/rtld.rs::load_now`, `DYNAMIC_LINKING.md` D8).
 *
 * `dynopen tls` is what checks the refusal: the load has to fail, and `dlerror` has to say
 * which limit was hit. This is the object that would work if multi-module TLS landed, which
 * is the gap §6.9 names for the C graphics stack.
 *
 * Freestanding — see `libdyn.c`.
 */

static __thread int tls_counter;

int tls_bump(void) {
    return ++tls_counter;
}
