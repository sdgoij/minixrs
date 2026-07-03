/* RISC-V64 kernel linker script — loaded at 0x80200000 (after OpenSBI) */
ENTRY(_start)

BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;

    .text : ALIGN(16) {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata : ALIGN(16) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(16) {
        *(.data .data.*)
        *(.got .got.*)
        *(.got.plt .got.plt.*)
        *(.plt .plt.*)
    }

    .bss : ALIGN(8) {
        *(.bss .bss.*)
        *(.sbss .sbss.*)
    }

    .initramfs : ALIGN(8) {
        KEEP(*(.initramfs))
    }

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
        *(.comment)
    }
}

/* Provide boundary symbols unconditionally (outside any section). */
__bss_start = .;
__bss_end = .;
__initramfs_start = .;
__initramfs_end = .;
