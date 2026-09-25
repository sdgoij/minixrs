/* The shared objects for the dynamic-linking gates.
 *
 * Two of them, and their names are the point: the loader assigns each `DT_NEEDED`
 * a base, so two objects cannot both live at one base, and both are found by name
 * from the main program.
 *
 * Three things here are the loader's business:
 *
 * - `message` and `data` are `static`, so the initialiser of the global that
 *   points at `data` is a *link-time* address: it is an `R_X86_64_RELATIVE` fixup,
 *   and the loader has to add this object's base to it. `dyn_data` reads it, so a
 *   fixup that did not happen is a wild pointer rather than a wrong number —
 *   which is what makes the printed value evidence about the fixup.
 * - `dyn_message` and `dyn_data` are the `JUMP_SLOT`s the main program's PLT
 *   resolves by name.
 *
 * The main program reaches the data through `dyn_data` rather than referring to
 * `dyn_pointer` directly: a non-PIE executable's reference to a variable defined
 * in a shared object is an `R_X86_64_COPY` (the linker reserves the space in the
 * executable and the loader copies the initial value into it), and that is a
 * Phase 2 item — see `reloc_action`.
 *
 * The strings live only here, so a program that prints them has proved the loader
 * bound its calls to this object: the same bytes must not be present in the
 * executable that calls it (that absence is the gate's negative check).
 *
 * Freestanding — no headers, no libc: the object makes no syscalls, so it needs
 * neither.
 */

static const char message[] = "dynlink-ok";
static const char data[] = "dynlink-data";

const char *const dyn_pointer = data;

const char *dyn_message(void);
const char *dyn_data(void);

const char *dyn_message(void) {
    return message;
}

const char *dyn_data(void) {
    return dyn_pointer;
}
