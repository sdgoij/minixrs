/* Userland linker script — each output section page-aligned */
ENTRY(_start)

BASE_ADDRESS = 0x01000000;

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

    /* Thread-local storage: the runtime copies `__tls_start..__tdata_end`
       (init image) into a per-thread block and zeroes up to `__tls_end`;
       the thread pointer (FS base / tpidr_el0 / tp) points at the block. */
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
