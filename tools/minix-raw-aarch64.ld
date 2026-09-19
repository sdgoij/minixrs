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

    /* The 64 KiB boot stack, which grows down from `__stack_top`. It is reserved
     * *between* the image and `__kernel_end` on purpose: `kmain` starts the
     * physical allocator at `__kernel_end`, so a `__kernel_end` below the stack
     * would hand the loader pages inside the running stack, and the loader's own
     * writes would then clobber its frame. (Not hypothetical: a kernel image
     * grown by a few KiB did exactly that to this arch's boot, silently, as an
     * endless loop in `load_and_prepare_proc`. `PORTING_PLAN.md` finding 19.) */
    . = ALIGN(16);
    . = . + 65536;
    __stack_top = .;
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
        *(.comment)
    }
}
