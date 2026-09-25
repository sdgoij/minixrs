/* Loader (`/libexec/ld.so`) linker script.
 *
 * The same sections as `tools/minix-user.ld`, at the loader's own base: clear of
 * the main program (0x01000000), the shared objects (0x02000000), the stack
 * (0x0FE00000) and the heap (0x3FE00000). The base cannot come from
 * `--image-base` because an explicit section address in a script wins over it.
 */
ENTRY(_start)

BASE_ADDRESS = 0x04000000;

SECTIONS
{
    . = BASE_ADDRESS;

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
