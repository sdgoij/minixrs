// Stage 2 bootloader — loaded at 0x1000 by MBR.
// Reads kernel from disk via INT 13h AH=02h (CHS mode), transitions to
// 64-bit long mode, jumps to kmain.
//
// Uses CHS instead of LBA (AH=42h) to avoid DAP structure issues with
// large kernels. CHS divides the read into 64-sector chunks automatically.

.code16
.section .text, "ax"
.global _start

.org 0x1000
_start:
    xorw %ax, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw $0x6000, %sp

    // Read boot drive from low memory (saved by MBR at 0x0500)
    movb (0x0500), %dl
    pushw %dx                   // save drive number

    // Read kernel from disk in 64-sector chunks via INT 13h AH=02h (CHS).
    // Buffer at physical 0x10000 = ES=0x1000, BX=0
    movw $0x1000, %ax
    movw %ax, %es               // ES = 0x1000
    xorw %bx, %bx               // BX = 0
    movw ker_sectors, %cx       // total sectors
    movw $4096, %si             // current LBA

1:
    pushw %cx
    pushw %bx
    pushw %si

    // Chunk size = min(64, remaining sectors)
    movw $64, %di
    cmpw %di, %cx
    jae 2f
    movw %cx, %di
2:

    // Convert LBA (SI) to CHS: C=abs/(H*S), H=(abs/S)%H, S=abs%S+1
    // Using H=16, S=63
    movw %si, %ax               // ax = LBA
    xorw %dx, %dx
    movw $1008, %cx             // 16*63
    divw %cx                    // ax = cylinder, dx = remainder
    pushw %ax                   // save cylinder

    movw %dx, %ax               // ax = remainder
    xorw %dx, %dx
    movw $63, %cx
    divw %cx                    // ax = head, dx = sector - 1

    movb %al, %dh               // DH = head
    addw $1, %dx                 // DL = sector (1-based), keep DH
    movb %dl, %cl               // CL = sector number (low 6 bits)

    popw %ax                    // ax = cylinder
    movb %al, %ch               // CH = cylinder low 8 bits
    andb $3, %ah
    shlb $6, %ah
    orb  %ah, %cl               // CL bits 7-6 = cylinder high

    movw %di, %ax               // AL = sectors to read
    movb $0x02, %ah             // AH = 02h
    movb (0x0500), %dl          // reload drive from saved memory

    int $0x13
    jc disk_error

    // Restore registers from stack (reverse of push order)
    popw %si
    popw %bx
    popw %cx

    // Advance buffer segment (chunk * 512 / 16 = chunk * 32)
    movw %di, %ax
    shlw $5, %ax
    pushw %dx
    movw %es, %dx
    addw %ax, %dx
    movw %dx, %es
    popw %dx
    xorw %bx, %bx               // BX = 0 (keep at segment start)

    addw %di, %si               // advance LBA

    subw %di, %cx
    jnz 1b

    // Enter protected mode
    cli
    lgdt gdt_ptr

    movl %cr0, %eax
    orl  $0x01, %eax
    movl %eax, %cr0

    ljmp $0x08, $pmode32

// ── 32-bit protected mode ──
.code32
pmode32:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %fs
    movw %ax, %gs
    movw %ax, %ss

    movl $stack_top, %esp

    // Copy kernel from buffer (0x10000) to target (0x200000)
    movl $0x10000, %esi
    movl $0x200000, %edi
    movl ker_sectors, %ecx
    shll $9, %ecx
    cld
    rep movsb

    // Debug: print 'C' after copy to 0x200000
    // ── Page table setup ──
    leal pml4, %edi
    xorl %eax, %eax
    movl $3072, %ecx
    rep stosl

    // PML4[0] = phys(pdp) | 0x07
    leal pdp, %eax
    orl  $7, %eax
    leal pml4, %edi
    movl %eax, (%edi)

    // PDP[0] = phys(pd) | 0x07
    leal pd, %eax
    orl  $7, %eax
    leal pdp, %edi
    movl %eax, (%edi)

    // Fill PD (512 × 2MB huge pages = 1GB identity, User-accessible)
    leal pd, %edi
    xorl %eax, %eax
    movl $512, %ecx
1:  movl %eax, %ebx
    shll $21, %ebx
    orl  $0x87, %ebx
    movl %ebx, (%edi, %eax, 8)
    incl %eax
    cmpl %ecx, %eax
    jb 1b

    movl %cr4, %eax
    orl  $0xA0, %eax
    movl %eax, %cr4

    leal pml4, %eax
    movl %eax, %cr3

    movl $0xC0000080, %ecx
    rdmsr
    orl  $0x100, %eax
    wrmsr

    movl %cr0, %eax
    orl  $0x80000000, %eax
    movl %eax, %cr0

    lgdt gdt64_ptr
    ljmp $0x18, $lmode64

// ── 64-bit long mode ──
.code64
lmode64:
    movw $0x20, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %fs
    movw %ax, %gs
    movw %ax, %ss

    // Debug: print '$' right before kernel jump
    movw $0x3F8, %dx
    movb $'$', %al
    outb %al, %dx

    // Set a stable kernel stack at a safe address (8 MB, well above
    // kernel binary which ends around 0x25E000 + BSS).
    movq $0x800000, %rsp

    movl $0x2BADB002, %edi
    xorl %esi, %esi
    movq ker_entry, %rax
    jmp  *%rax

// ── Error handler ──
.code16
disk_error:
    movw $0x3F8, %dx
    movb $'!', %al
    outb %al, %dx
    cli
    hlt
    jmp disk_error

// ============================================================================
// DATA
// ============================================================================

.align 4
dap_kernel:
    .byte 0x10
    .byte 0
    .word 9                  // sectors — patched by mkimg.rs (unused in CHS mode)
    .word 0x0000
    .word 0x1000
    .quad 4096

// ── 32-bit GDT ──
.align 8
gdt_start:
    .quad 0x0000000000000000
    .quad 0x00CF9A000000FFFF
    .quad 0x00CF92000000FFFF
gdt_end:
gdt_ptr:
    .word gdt_end - gdt_start - 1
    .long gdt_start

// ── 64-bit GDT ──
.align 8
gdt64_start:
    .quad 0x0000000000000000
    .quad 0x00CF9A000000FFFF
    .quad 0x00CF92000000FFFF
    .quad 0x00AF9A0000000000
    .quad 0x00CF92000000FFFF
gdt64_end:
gdt64_ptr:
    .word gdt64_end - gdt64_start - 1
    .long gdt64_start

.align 8
ker_entry:
    .quad 0x2002B0
ker_sectors:
    .quad 9

// ============================================================================
// PAGE TABLES
// ============================================================================

.org 0x2000
pml4:   .space 4096

.org 0x3000
pdp:    .space 4096

.org 0x4000
pd:     .space 4096

.org 0x5000
    .space 4096
stack_top:
