/* The second shared object.
 *
 * It exists so the loader's base allocator is exercised: two objects cannot both
 * be mapped at one base, and both are resolved by name from the main program. Its
 * own `RELATIVE` fixup is the same shape as the first object's.
 *
 * Freestanding — see `libdyn.c`.
 */

static const char message[] = "dynlink-2-ok";

const char *const dyn_second_pointer = message;

const char *dyn_second(void);

const char *dyn_second(void) {
    return dyn_second_pointer;
}
