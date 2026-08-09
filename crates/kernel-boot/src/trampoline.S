// ELF32 multiboot trampoline — transitions to 64-bit long mode and jumps
// to the 64-bit kernel at kernel_entry (0x200450).

.section .text, "ax"
.code32
.global _start

// Multiboot header
.align 4
multiboot_header:
.long 0x1BADB002
.long 0x2                 // MULTIBOOT_MEMORY_INFO: request mem_upper + memory map
.long -0x1BADB004

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

    // PDP[0..31] = &pd0..pd31 | 0x07 (Present|Write|User)
    movl $pd0, %edi
    movl $pdp, %esi
    movl $32, %ebp
3:
    movl %edi, %ebx
    orl  $0x07, %ebx
    movl %ebx, (%esi)
    addl $8, %esi
    addl $4096, %edi
    decl %ebp
    jnz 3b

    // Fill 32 PDs with 2 MB huge-page entries (identity map, 0..32 GiB).
    // 64-bit PTEs: PA = (window << 30) | (index << 21). The window base
    // exceeds 32 bits above 16 GiB, so the high dword is window >> 2 plus
    // the carry out of the low-dword add. SHR clobbers CF, so capture the
    // carry with SETcc before computing the high dword.
    movl $pd0, %edi
    xorl %esi, %esi        // window index (0..31)
    movl $32, %ebp         // PD count
1:
    xorl %eax, %eax
2:  movl %eax, %ebx
    shll $21, %ebx         // index * 2 MiB (fits in 32 bits)
    movl %esi, %edx
    shll $30, %edx         // low dword of window * 1 GiB
    addl %edx, %ebx        // low dword of PA; CF = carry out
    setc %dl               // dl = carry (0/1)
    movzbl %dl, %edx       // edx = carry
    movl %esi, %ecx
    shrl $2, %ecx          // high dword of window * 1 GiB
    addl %edx, %ecx        // + carry from the low add
    orl  $0x87, %ebx       // Present|Write|User|Huge (2MB page)
    movl %ebx, (%edi, %eax, 8)
    movl %ecx, 4(%edi, %eax, 8)
    incl %eax
    cmpl $512, %eax
    jb 2b
    incl %esi              // next window
    addl $4096, %edi       // next PD
    decl %ebp
    jnz 1b

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
    movw $0x20, %ax
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
pd0:  .space 131072   // 32 PDs (0..32 GiB identity)
trampoline_stack: .space 32768

// 64-bit GDT and saved multiboot args
.section .data, "aw"
.align 8
magic_save: .space 8
info_save:  .space 8
gdt_start:
.quad 0x0000000000000000         // null descriptor (0x00)
.quad 0x00AF9A0000000000         // kernel code (0x08): 64-bit, DPL=0
.quad 0x00CFF2000000FFFF         // user data (0x13): DPL=3, G=1, D/B=1, limit=4GB
.quad 0x00AFFA000000FFFF         // user code (0x1B): 64-bit, DPL=3, G=1, L=1, limit=4GB
.quad 0x00CF920000000000         // kernel data (0x23): DPL=0
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
