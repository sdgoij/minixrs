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

#include "doomtype.h"
#include "i_system.h"
#include "g_game.h"

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

/*
 * Wasm's `call_indirect` checks the callee's signature, where a native call
 * ignores a return value it does not use. The engine registers its demo
 * checker with `I_AtExit((atexit_func_t) G_CheckDemoStatus, true)`, casting a
 * `boolean(void)` to the `void(void)` hook type; nothing happens on a native
 * ABI, but the cast is a different wasm function type and the first indirect
 * call through it, when `I_Quit` walks the hook list, traps with "function
 * signature mismatch" and takes the whole guest down.
 *
 * `I_AtExit` itself is called with one signature everywhere, so wrap it and
 * hand the real one a nullary trampoline in place of that single mismatch. The
 * wrapper is linked only for wasm (the `--wrap` flag is in the module's link
 * line), so the native builds keep the engine's own registration.
 */
static void g_check_demo_status_hook(void) {
    (void)G_CheckDemoStatus();
}

void __real_I_AtExit(atexit_func_t func, boolean run_if_error);

void __wrap_I_AtExit(atexit_func_t func, boolean run_if_error) {
    if ((void *)func == (void *)G_CheckDemoStatus) {
        func = g_check_demo_status_hook;
    }
    __real_I_AtExit(func, run_if_error);
}
