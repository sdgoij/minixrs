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
            __bss_start = .;
            *(.bss .bss.*)
            *(.sbss .sbss.*)
            __bss_end = .;
        }

        .initramfs : ALIGN(8) {
            __initramfs_start = .;
            KEEP(*(.initramfs))
            __initramfs_end = .;
        }

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
        *(.comment)
    }
}
