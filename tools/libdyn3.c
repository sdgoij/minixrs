/* The third shared object.
 *
 * Its job is not resolution but *room*. An address space holds `MAX_REGIONS` regions
 * (`crates/servers/src/vm/region.rs`), a shared object costs one per `PT_LOAD` — four for
 * an object linked the way this one is — and a dynamically linked program has spent 8 on
 * its two images plus the stack and the heap before the loader maps anything. With 16
 * regions the third object was the one that did not fit: the loader's `mmap` of its
 * first segment was refused and the program did not run at all. `dyn_third` returning
 * its string is therefore the evidence that a program can carry a library of its own,
 * and the gate fails if the budget ever goes back to two.
 *
 * Nothing depends on this object and it depends on nothing, which is what keeps that
 * measurement about the address space rather than about resolution.
 *
 * The string lives only here, like the other objects': the executable must not contain
 * it, and that absence is what makes the printed line evidence about the loader.
 *
 * Freestanding — see `libdyn.c`.
 */

static const char third[] = "dynlink-3-ok";

const char *dyn_third(void) {
    return third;
}
