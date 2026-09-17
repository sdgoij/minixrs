/* Trampoline linker script — single loadable segment at 0x100000 */
ENTRY(_start)

SECTIONS
{
    . = 0x100000;

    .text : {
        *(.text.entry)
        *(.text)
    }

    .data : {
        *(.data)
    }

    .bss : {
        __bss_start = .;
        *(.bss)
        __bss_end = .;
    }

    /* The kernel image (see `tools/mkboot.rs`, which links a stub that
       `.incbin`s it) becomes its own loadable segment here, so this ELF boots
       on its own: QEMU's multiboot loader places the kernel exactly where
       `-device loader,file=<stem>.bin,addr=0x200000` used to put it. */
    . = 0x200000;

    .kernel : {
        KEEP(*(.kernel))
    }
}
