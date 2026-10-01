/* The loader (`/libexec/ld.so`) linker script.
 *
 * The same sections as `tools/minix-user.ld`, at the loader's own base: clear of
 * the main program, the shared objects and the stack. The base cannot come from
 * `--image-base` because an explicit section address in a script wins over it.
 *
 * Overridable: the target spec passes `--defsym=LOADER_BASE=<arch loader base>`,
 * because the base is per-arch (`PHYSMAP.md` P4 — x86's userland starts at
 * 64 MiB and the shared objects fill 80..128 MiB, so its loader is at 128 MiB).
 * A bare `rustc` invocation that does not go through the spec still gets this
 * default.
 */
ENTRY(_start)

PROVIDE(LOADER_BASE = 0x04000000);

SECTIONS
{
    . = LOADER_BASE;

    .text : ALIGN(0x1000) {
        KEEP(*(.text._start))
        *(.text .text.*)
    }

    .rodata : ALIGN(0x1000) {
        *(.rodata.str*)
        *(.rodata .rodata.*)
    }

    .data : ALIGN(0x1000) {
        *(.data .data.*)
        *(.got .got.*)
        *(.got.plt .got.plt.*)
    }

    .init_array : ALIGN(8) {
        __init_array_start = .;
        KEEP(*(.init_array .init_array.*))
        __init_array_end = .;
    }
    .fini_array : ALIGN(8) {
        __fini_array_start = .;
        KEEP(*(.fini_array .fini_array.*))
        __fini_array_end = .;
    }

    .tdata : ALIGN(16) {
        __tls_start = .;
        *(.tdata .tdata.*)
        __tdata_end = .;
    }
    .tbss : ALIGN(16) {
        *(.tbss .tbss.*)
        *(.tcommon)
        __tls_end = .;
    }

    .bss : ALIGN(0x1000) {
        __bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        __bss_end = .;
    }

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
    }
}
