/*
 * The wasm program contract, for Doom.
 *
 * A program module on this port exports `minix_program_main(argc, argv)`, and the
 * host writes the arguments into an area the module names with `argv_area_ptr()`
 * (tools/wasm-browser/host.js: one `argc`, the pointer array, then the strings).
 * It also names its Asyncify scratch region with `asyncify_scratch_ptr()`, which
 * the host fills in before the entry: the address of the end of the module's
 * statics, aligned, where `wasm-opt --asyncify`'s unwind stack goes.
 *
 * The module imports its memory (`--import-memory`), so the linker places the
 * static image and the 1 MiB stack inside the host's linear memory, below the
 * port's user-VA windows.
 */

#include <stdint.h>

int main(int argc, char **argv);

/* The host owns the size (512 bytes, below the module's own scratch region). */
static unsigned char argv_area[512] __attribute__((aligned(8)));

unsigned int argv_area_ptr(void) {
    return (unsigned int)(uintptr_t)argv_area;
}

/* lld's end-of-statics symbol; only its address is taken. */
extern unsigned char __heap_base;

unsigned int asyncify_scratch_ptr(void) {
    return (unsigned int)(((uintptr_t)&__heap_base) + 15u) & ~15u;
}

int minix_program_main(int argc, char **argv) {
    return main(argc, argv);
}
