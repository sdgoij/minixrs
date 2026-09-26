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
 *   with only one mapping, and resolve a symbol across objects. The dependency is
 *   named three ways — by soname and by two paths to the same file — so the
 *   "twice" is the loader's own de-duplication rather than the linker's.
 * - it exports the counter `libdyn2.so`'s initialiser bumps, which is how the gate
 *   sees that one mapping of that file ran one initialiser.
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

/* How many initialisers have run, counted here rather than in the objects that run
 * them, because *this* object is the one the executable names exactly once: a count
 * kept in `libdyn2.so` would be per-mapping and each copy would still say 1.
 *
 * `dynhello` reads it through `dyn_init_count()`, so the number it prints is how many
 * times `libdyn2.so`'s constructor ran — one per mapping of that file. A loader that
 * mapped it twice, under another `DT_NEEDED` spelling or twice over, shows up as 2.
 *
 * The count travels through a call and not an exported variable on purpose: `dynhello`
 * is non-PIE, so reading an exported variable there is an `R_X86_64_COPY` of this
 * object's storage, and a copy is a second place the count could live. A call is bound
 * to this object's own variable. */
static int init_count;

void dyn_init_bump(void) {
    init_count++;
}

int dyn_init_count(void) {
    return init_count;
}

const char *dyn_message(void) {
    /* The answer comes from `libdyn2.so`: this object only reaches it if the
     * loader loaded a dependency of a dependency, resolved the name across
     * objects, and ran that object's initialiser. */
    return dyn_second_ready() ? message : without_dep;
}
