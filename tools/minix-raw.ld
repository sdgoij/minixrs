/* Kernel linker script — linked at 0x200000 (loaded via -device loader) */
ENTRY(_start)

BASE_ADDRESS = 0x200000;

SECTIONS
{
    . = BASE_ADDRESS;

    .multiboot : {
        KEEP(*(.multiboot))
    }

    .text : {
        . = ALIGN(16);
        KEEP(*(.text.entry.64))
        KEEP(*(.text.entry))
        *(.text .text.*)
    }

    .text.kmain : {
        . = ALIGN(16);
        KEEP(*(.text.kmain))
    }

    .rodata : {
        . = ALIGN(16);
        *(.rodata .rodata.*)
    }

    .data : {
            . = ALIGN(16);
            *(.data .data.*)
            *(.got .got.*)
            *(.got.plt .got.plt.*)
            *(.plt .plt.*)
        }

        .page_tables : {
            *(.page_tables)
        }

        .bss : {
                . = ALIGN(8);
                __bss_start = .;
                *(.bss.boot_stack)
                *(.bss .bss.*)
                *(.sbss .sbss.*)
                . = ALIGN(8);
                __bss_end = .;
            }

    .initramfs : {
        __initramfs_start = .;
        *(.initramfs)
        __initramfs_end = .;
    }

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
    }
}
