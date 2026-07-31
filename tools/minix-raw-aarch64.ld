/* AArch64 kernel linker script — loaded at 0x40080000 (QEMU virt, 512KB into RAM) */
ENTRY(_start)

BASE_ADDRESS = 0x40000000;

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
    }

    .bss : ALIGN(8) {
        __bss_start = .;
        *(.bss .bss.*)
        __bss_end = .;
    }

    .initramfs : ALIGN(8) {
        __initramfs_start = .;
        KEEP(*(.initramfs))
        __initramfs_end = .;
    }

    .minixfs : ALIGN(8) {
        __minixfs_start = .;
        KEEP(*(.minixfs))
        __minixfs_end = .;
    }

    __kernel_end = .;

    . = . + 65536;
    __stack_top = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
        *(.comment)
    }
}
