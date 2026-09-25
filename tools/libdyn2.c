/* The second shared object.
 *
 * It carries three of the loader's jobs at once:
 *
 * - two objects cannot share one base, so it is what exercises the loader's base
 *   allocator;
 * - the main program *and* `libdyn.so` both name it, so the loader has to load it
 *   once and not twice;
 * - it has an initialiser, and `dyn_second` reports whether that initialiser ran —
 *   so the printed value is evidence that constructors are called (and called
 *   before the object that depends on them).
 *
 * Freestanding — see `libdyn.c`.
 */

static const char before_init[] = "dynlink-2-no-ctor";
static const char after_init[] = "dynlink-2-ok";

/* A `static` pointer into this object's own data, so its initialiser is an
 * R_X86_64_RELATIVE fixup; the initialiser below then replaces it.
 *
 * `volatile` because the whole point is that the value changes *between load and
 * run*: without it the compiler folds `dyn_second` to the constructor's result and
 * drops the constructor, the RELATIVE fixup and the test along with it. */
static const char *volatile state = before_init;

__attribute__((constructor)) static void init_state(void) {
    state = after_init;
}

const char *dyn_second(void);
int dyn_second_ready(void);

const char *dyn_second(void) {
    return state;
}

/* Asked by `libdyn.so`, so that whether this object's initialiser ran is visible
 * from another object. */
int dyn_second_ready(void) {
    return state == after_init;
}
