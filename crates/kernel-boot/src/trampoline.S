// ELF32 multiboot trampoline — transitions to 64-bit long mode and jumps
// to the 64-bit kernel at kernel_entry (0x200450).

.section .text, "ax"
.code32
.global _start

// Multiboot header
.align 4
multiboot_header:
.long 0x1BADB002
.long 0
.long -0x1BADB002

_start:
    // Save multiboot args to memory (registers get clobbered below)
    movl %eax, magic_save
    movl %ebx, info_save

    // Check CPUID
    pushfl
    popl %eax
    movl %eax, %ecx
    xorl $0x200000, %eax
    pushl %eax
    popfl
    pushfl
    popl %eax
    xorl %ecx, %eax
    jz halt

    // Check extended CPUID
    movl $0x80000000, %eax
    cpuid
    cmpl $0x80000001, %eax
    jb halt

    // Check long mode
    movl $0x80000001, %eax
    cpuid
    testl $0x20000000, %edx
    jz halt

    // PML4[0] = &pdp | 0x07 (Present|Write|User)
    movl $pdp, %ebx
    orl  $0x07, %ebx
    movl %ebx, pml4

    // PDP[0] = &pd | 0x07 (Present|Write|User)
    movl $pd, %ebx
    orl  $0x07, %ebx
    movl %ebx, pdp

    // Fill PD with 2 MB huge-page entries (identity map)
    movl $pd, %edi
    xorl %eax, %eax
    movl $512, %ecx
0:  movl %eax, %ebx
    shll $21, %ebx
    orl  $0x87, %ebx    // Present|Write|User|Huge (2MB page)
    movl %ebx, (%edi, %eax, 8)
    incl %eax
    cmpl %ecx, %eax
    jb 0b

    // Enable PAE (bit 5) + PGE (bit 7)
    movl %cr4, %eax
    orl  $0xA0, %eax
    movl %eax, %cr4

    // Load PML4
    movl $pml4, %eax
    movl %eax, %cr3

    // Enable long mode
    movl $0xC0000080, %ecx
    rdmsr
    orl  $0x100, %eax
    wrmsr

    // Enable paging
    movl %cr0, %eax
    orl  $0x80000000, %eax
    movl %eax, %cr0
    jmp 0f
0:
    // Load 64-bit GDT and far-jump
    lgdt gdt_ptr
    pushl $0x08
    leal 1f, %eax
    pushl %eax
    lret

.code64
1:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss

    // Set up a temporary stack (32KB in trampoline BSS)
    lea trampoline_stack(%rip), %rsp
    add $32768, %rsp

    // Jump to kernel (edi=magic, esi=info_ptr from saved values)
    mov magic_save(%rip), %edi
    mov info_save(%rip), %esi
    movq $kernel_entry, %rax
    jmp *%rax

halt:
    cli
    hlt
    jmp halt

// Page tables and stack in BSS
.section .bss, "aw", @nobits
.align 4096
pml4: .space 4096
pdp:  .space 4096
pd:   .space 4096
trampoline_stack: .space 32768

// 64-bit GDT and saved multiboot args
.section .data, "aw"
.align 8
magic_save: .space 8
info_save:  .space 8
gdt_start:
.quad 0x0000000000000000         // null descriptor (0x00)
.quad 0x00AF9A0000000000         // kernel code (0x08): 64-bit, DPL=0
.quad 0x00CF920000000000         // kernel data (0x10): DPL=0
.quad 0x00AFFA000000FFFF         // user code (0x1B): 64-bit, DPL=3, G=1, L=1, limit=4GB
.quad 0x001FF2000000FFFF         // user data (0x23): DPL=3, G=1, limit=4GB
gdt_end:

gdt_ptr:
.short gdt_end - gdt_start - 1    // limit = 5*8-1 = 39
.long gdt_start                    // base = address of gdt_start
.short 0

// Kernel entry — kmain address (overridden by -DKMAIN from Justfile)
#ifdef KMAIN
.set kernel_entry, KMAIN
#else
.set kernel_entry, 0x2004B0
#endif
