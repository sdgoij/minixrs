/* The first shared object.
 *
 * Its own jobs, beyond printing:
 *
 * - `message` and `data` are `static`, so the initialiser of `dyn_pointer` is a
 *   *link-time* address: an R_X86_64_RELATIVE fixup the loader has to rebase.
 * - `dyn_message` is the JUMP_SLOT the main program's PLT resolves by name.
 * - `dyn_pointer` is read by the main program *as a variable*, which for a non-PIE
 *   executable is an R_X86_64_COPY: the linker reserved the space in the
 *   executable, and the loader must move this object's bytes into it — after
 *   applying the RELATIVE above, since those are the bytes it moves.
 * - it names `libdyn2.so` as its own DT_NEEDED and calls into it, which is what
 *   makes the loader follow a dependency of a dependency, load one library twice
 *   with only one mapping, and resolve a symbol across objects.
 *
 * The strings live only here and in `libdyn2.so`, so a program that prints them
 * has proved the loader bound its calls to these objects: the same bytes must not
 * be present in the executable (that absence is the gate's negative check).
 *
 * Freestanding — no headers, no libc: the object makes no syscalls, so it needs
 * neither.
 */

const char *dyn_message(void);

extern int dyn_second_ready(void);

static const char message[] = "dynlink-ok";
static const char without_dep[] = "dynlink-1-no-dep";
static const char data[] = "dynlink-data";

const char *const dyn_pointer = data;

const char *dyn_message(void) {
    /* The answer comes from `libdyn2.so`: this object only reaches it if the
     * loader loaded a dependency of a dependency, resolved the name across
     * objects, and ran that object's initialiser. */
    return dyn_second_ready() ? message : without_dep;
}
