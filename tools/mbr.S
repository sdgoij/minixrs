// MBR bootloader — loaded at 0x7C00 by BIOS.
// Loads stage2 from sectors 1-48 to 0x1000, then jumps there.

.code16
.section .text, "ax"
.global _start

_start:
    // Set DS = 0 for absolute addressing
    xorw %ax, %ax
    movw %ax, %ds

    // Save boot drive IMMEDIATELY (before clobbering DX)
    movb %dl, (0x0500)

    // Load stage2 (sectors 1-48) to 0x1000 via INT 13h AH=42h
    movw $dap, %si
    movb $0x42, %ah
    movb (0x0500), %dl
    int $0x13
    jc error

    // Jump to stage2
    ljmp $0x0000, $0x1000

// Disk Address Packet
.align 4
dap:
    .byte 0x10
    .byte 0
    .word 48
    .word 0x1000
    .word 0x0000
    .quad 1

error:
    movw $0x3F8, %dx
    movb $'!', %al
    outb %al, %dx
    cli
    hlt
    jmp error

// Pad to 510 bytes, then boot signature
.space 510 - (. - _start)
.word 0xAA55
