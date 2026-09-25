/* The Phase 0 shared object.
 *
 * The string below lives only in this file, so a program that prints it has
 * proved the loader bound its call to this object: the same bytes must not be
 * present in the executable that calls it (that absence is the gate's negative
 * check).
 *
 * Freestanding — no headers, no libc: the object makes no syscalls, so it needs
 * neither.
 */

const char *dyn_message(void);
int dyn_answer(void);

const char *dyn_message(void) {
    return "dynlink-ok";
}

int dyn_answer(void) {
    return 42;
}
