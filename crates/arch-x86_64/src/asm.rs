//! x86_64 assembly routines — ported from i386 `klib.S`, `io_*.S`,
//! `debugreg.S`, and `cpu_msr.h`
//!
//! Uses inline `asm!()` for all operations — no separate .S files needed.
//!
//! **x86_64 differences from i386:**
//! - System V AMD64 ABI: args in rdi, rsi, rdx, rcx, r8, r9 (not stack)
//! - All pointers/addresses are 64-bit (movq, not movl)
//! - Context switch saves rbx, rbp, r12–r15 (callee-saved)
//! - `rep movsb` uses 64-bit rcx/rdi/rsi
//! - I/O instructions use the same encoding with 64-bit register addressing

use core::arch::asm;

// I/O port access (byte, word, dword)

/// Read a byte from an I/O port.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack));
    }
    value
}

/// Read a word (16-bit) from an I/O port.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack));
    }
    value
}

/// Read a dword (32-bit) from an I/O port.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack));
    }
    value
}

/// Write a byte to an I/O port.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
    }
}

/// Write a word (16-bit) to an I/O port.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn outw(port: u16, value: u16) {
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack));
    }
}

/// Write a dword (32-bit) to an I/O port.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn outl(port: u16, value: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack));
    }
}

// Interrupt control

/// Disable interrupts (clear IF flag).
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn intr_disable() {
    unsafe {
        asm!("cli", options(nomem, nostack));
    }
}

/// Enable interrupts (set IF flag).
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn intr_enable() {
    unsafe {
        asm!("sti", options(nomem, nostack));
    }
}

// Debug register access

/// Read a debug register (DR0–DR3, DR6, DR7).
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn ld_dr(reg: u32) -> u64 {
    let value: u64;
    unsafe {
        match reg {
            0 => asm!("mov rax, dr0", out("rax") value, options(nomem, nostack)),
            1 => asm!("mov rax, dr1", out("rax") value, options(nomem, nostack)),
            2 => asm!("mov rax, dr2", out("rax") value, options(nomem, nostack)),
            3 => asm!("mov rax, dr3", out("rax") value, options(nomem, nostack)),
            6 => asm!("mov rax, dr6", out("rax") value, options(nomem, nostack)),
            7 => asm!("mov rax, dr7", out("rax") value, options(nomem, nostack)),
            _ => return 0,
        }
    }
    value
}

/// Write a debug register (DR0–DR3, DR6, DR7).
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn st_dr(reg: u32, value: u64) {
    unsafe {
        match reg {
            0 => asm!("mov dr0, rax", in("rax") value, options(nomem, nostack)),
            1 => asm!("mov dr1, rax", in("rax") value, options(nomem, nostack)),
            2 => asm!("mov dr2, rax", in("rax") value, options(nomem, nostack)),
            3 => asm!("mov dr3, rax", in("rax") value, options(nomem, nostack)),
            6 => asm!("mov dr6, rax", in("rax") value, options(nomem, nostack)),
            7 => asm!("mov dr7, rax", in("rax") value, options(nomem, nostack)),
            _ => {}
        }
    }
}

// Memory copy (physical/physical)

/// Copy memory from one physical address to another using `rep movsb`.
///
/// # Safety
/// - `src` and `dst` must point to valid, mapped memory.
/// - The regions must not overlap.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn phys_copy(src: u64, dst: u64, count: usize) {
    unsafe {
        asm!(
            "cld",
            "rep movsb",
            in("rsi") src,
            in("rdi") dst,
            in("rcx") count,
            clobber_abi("C"),
        );
    }
}

// I/O port array operations (string I/O)

/// Input an array of bytes from an I/O port to memory.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn phys_insb(port: u16, buf: u64, count: usize) {
    unsafe {
        asm!(
            "cld",
            "rep insb",
            in("dx") port,
            in("rdi") buf,
            in("rcx") count,
            clobber_abi("C"),
        );
    }
}

/// Input an array of words from an I/O port to memory.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn phys_insw(port: u16, buf: u64, count: usize) {
    let words = count / 2;
    unsafe {
        asm!(
            "cld",
            "rep insw",
            in("dx") port,
            in("rdi") buf,
            in("rcx") words,
            clobber_abi("C"),
        );
    }
}

/// Output an array of bytes from memory to an I/O port.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn phys_outsb(port: u16, buf: u64, count: usize) {
    unsafe {
        asm!(
            "cld",
            "rep outsb",
            in("dx") port,
            in("rsi") buf,
            in("rcx") count,
            clobber_abi("C"),
        );
    }
}

/// Output an array of words from memory to an I/O port.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn phys_outsw(port: u16, buf: u64, count: usize) {
    let words = count / 2;
    unsafe {
        asm!(
            "cld",
            "rep outsw",
            in("dx") port,
            in("rsi") buf,
            in("rcx") words,
            clobber_abi("C"),
        );
    }
}

// MSR access

/// Read an MSR.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdmsr",
            out("eax") low,
            out("edx") high,
            in("ecx") msr,
            options(nomem, nostack),
        );
    }
    (low as u64) | ((high as u64) << 32)
}

/// Write an MSR.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn wrmsr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    unsafe {
        asm!(
            "wrmsr",
            in("eax") low,
            in("edx") high,
            in("ecx") msr,
            options(nomem, nostack),
        );
    }
}

// Context switch

/// Save callee-saved registers and switch stacks.
///
/// Saves rbx, rbp, r12–r15 on the current stack, switches RSP to
/// `new_rsp`, restores the callee-saved registers from the new stack,
/// and returns (pops the return address from the new stack).
///
/// # Safety
/// - `new_rsp` must point to a valid kernel stack with a consistent
///   saved register state at the top.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_to(_new_rsp: u64) {
    unsafe {
        asm!(
            "push   r15",
            "push   r14",
            "push   r13",
            "push   r12",
            "push   rbp",
            "push   rbx",
            "push   rsp",
            "mov    rsp, rdi",
            "pop    rbx",
            "pop    rbp",
            "pop    r12",
            "pop    r13",
            "pop    r14",
            "pop    r15",
            "ret",
            options(noreturn),
        );
    }
}

// CR register access

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn read_cr0() -> u64 {
    let value: u64;
    unsafe {
        asm!("mov rax, cr0", out("rax") value, options(nomem, nostack));
    }
    value
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn write_cr0(value: u64) {
    unsafe {
        asm!("mov cr0, rax", in("rax") value, options(nomem, nostack));
    }
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn read_cr2() -> u64 {
    let value: u64;
    unsafe {
        asm!("mov rax, cr2", out("rax") value, options(nomem, nostack));
    }
    value
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn read_cr3() -> u64 {
    let value: u64;
    unsafe {
        asm!("mov rax, cr3", out("rax") value, options(nomem, nostack));
    }
    value
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn write_cr3(value: u64) {
    unsafe {
        asm!("mov cr3, rax", in("rax") value, options(nomem, nostack));
    }
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn read_cr4() -> u64 {
    let value: u64;
    unsafe {
        asm!("mov rax, cr4", out("rax") value, options(nomem, nostack));
    }
    value
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn write_cr4(value: u64) {
    unsafe {
        asm!("mov cr4, rax", in("rax") value, options(nomem, nostack));
    }
}

// GDT/IDT load

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn lgdt(gdtr: &[u8; 10]) {
    unsafe {
        asm!("lgdt [{}]", in(reg) gdtr.as_ptr(), options(nostack));
    }
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn lidt(idtr: &[u8; 10]) {
    unsafe {
        asm!("lidt [{}]", in(reg) idtr.as_ptr(), options(nostack));
    }
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn ltr(selector: u16) {
    unsafe {
        asm!("ltr {:x}", in(reg) selector, options(nomem, nostack));
    }
}

// TLB management

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn invlpg(addr: u64) {
    unsafe {
        asm!("invlpg [{}]", in(reg) addr, options(nostack));
    }
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn tlb_flush() {
    unsafe {
        let cr3 = read_cr3();
        write_cr3(cr3);
    }
}

// Halt

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn hlt() {
    unsafe {
        asm!("hlt", options(nomem, nostack));
    }
}

// Exception handlers (naked asm, IST-safe, use serial I/O port 0x3F8)

/// Page fault handler — prints 'P', CR2 as hex nibbles, then halts.
/// Uses IST1 (TSS.IST[1]) for a reliable stack.
///
/// On entry: error code is on the stack (above RIP/CS/RFLAGS/SS/RSP for ring-3 faults).
/// CR2 holds the faulting linear address.
#[unsafe(no_mangle)]
#[unsafe(naked)]
#[cfg(target_os = "none")]
/// # Safety
///
/// Must be called only during early boot on the BSP, before SMP is initialized.
pub unsafe extern "C" fn exception_page_fault_entry() {
    // Write 'P' to serial, then write CR2 value as hex, then halt.
    core::arch::naked_asm!(
        // Save rax, rcx, rdx (clobbered by I/O ops).
        "push   rax",
        "push   rcx",
        "push   rdx",
        // Write 'P' (0x50) to COM1 (0x3F8).
        "mov    dx, 0x3F8",
        "mov    al, 0x50",
        "out    dx, al",
        // Read CR2 into rax, then write 16 hex nibbles to serial.
        "mov    rax, cr2",
        // Write CR2 hex. We'll loop over 16 nibbles (64 bits).
        // Since we're in a naked function, we use inline labels.
        "mov    rcx, 16",
        "2:",
        "rol    rax, 4",
        "push   rax",
        "and    al, 0x0F",
        "cmp    al, 10",
        "jb     3f",
        "add    al, 0x57", // 'a' - 10
        "jmp    4f",
        "3:",
        "add    al, 0x30", // '0'
        "4:",
        "out    dx, al",
        "pop    rax",
        "loop   2b",
        // Write newline
        "mov    al, 0x0D",
        "out    dx, al",
        "mov    al, 0x0A",
        "out    dx, al",
        // Restore and halt
        "pop    rdx",
        "pop    rcx",
        "pop    rax",
        "cli",
        "hlt",
    );
}

/// Double fault handler — prints 'D', then halts.
/// Uses IST2 (TSS.IST[2]) for a reliable stack.
#[unsafe(no_mangle)]
#[unsafe(naked)]
#[cfg(target_os = "none")]
/// # Safety
///
/// Must be called only during early boot on the BSP, before SMP is initialized.
pub unsafe extern "C" fn exception_double_fault_entry() {
    core::arch::naked_asm!(
        "mov    dx, 0x3F8",
        "mov    al, 0x44", // 'D'
        "out    dx, al",
        "mov    al, 0x0D",
        "out    dx, al",
        "mov    al, 0x0A",
        "out    dx, al",
        "cli",
        "hlt",
    );
}

/// General protection fault handler — prints 'G', then halts.
#[unsafe(no_mangle)]
#[unsafe(naked)]
#[cfg(target_os = "none")]
/// # Safety
///
/// Must be called only during early boot on the BSP, before SMP is initialized.
pub unsafe extern "C" fn exception_gpf_entry() {
    core::arch::naked_asm!(
        "mov    dx, 0x3F8",
        "mov    al, 0x47", // 'G'
        "out    dx, al",
        "mov    al, 0x0D",
        "out    dx, al",
        "mov    al, 0x0A",
        "out    dx, al",
        "cli",
        "hlt",
    );
}

/// Ring-3 halt stub — a minimal user-mode program that just halts.
/// Used for testing whether the ring-3 context switch itself works.
#[unsafe(no_mangle)]
#[unsafe(naked)]
#[cfg(target_os = "none")]
/// # Safety
///
/// Must be called only during early boot on the BSP, before SMP is initialized.
pub unsafe extern "C" fn ring3_halt_stub() {
    core::arch::naked_asm!("1:", "pause", "jmp    1b",);
}

// SGDT / SIDT / STR

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn sgdt(desc: &mut [u8; 10]) {
    unsafe {
        asm!("sgdt [{}]", in(reg) desc.as_mut_ptr(), options(nostack));
    }
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn sidt(desc: &mut [u8; 10]) {
    unsafe {
        asm!("sidt [{}]", in(reg) desc.as_mut_ptr(), options(nostack));
    }
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn str_sel() -> u16 {
    let sel: u16;
    unsafe {
        asm!("str {:x}", out(reg) sel, options(nomem, nostack));
    }
    sel
}

// SYSRETQ to userspace

/// Execute `sysretq` to jump to ring-3 — first userspace transition.
///
/// # Arguments
///
/// * `proc_ptr` — pointer to the kernel `Proc` struct. `p_reg.rcx` is
///   loaded as RIP, `p_reg.r11` as RFLAGS, `p_reg.rsp` as RSP, and
///   `p_seg.p_cr3` as CR3.
///
/// # Safety
///
/// `proc_ptr` must point to a valid `Proc` whose `p_seg.p_cr3` covers the
/// entry point and stack. Must be called in ring 0 with interrupts disabled.
/// Never returns.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn sysretq_to_user(proc_ptr: *const u8) -> ! {
    core::arch::naked_asm!(
        "mov    rax, [rdi + 256]",
        "mov    cr3, rax",
        "mov    rcx, [rdi + 16]",
        "mov    r11, [rdi + 72]",
        "mov    rsp, [rdi + 168]",
        "sysretq",
    );
}

/// Execute `sysretq` — assumes registers are already set by caller:
///   rax = CR3, rcx = entry (RIP), r11 = RFLAGS, rdx = user RSP
///
/// # Safety
///
/// All registers must contain valid values for ring-3 execution. The
/// page table in CR3 must map the entry point and stack as user-accessible.
/// RCX must point to valid executable code. R11 must contain valid RFLAGS.
/// RSP must point to a valid user-accessible stack.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn sysretq_direct() -> ! {
    core::arch::naked_asm!("mov    cr3, rax", "mov    rsp, rdx", "sysretq",);
}

/// Restore a process context and jump to it via `sysretq`.
///
/// Takes a pointer to a `Proc` struct in `rdi` (System V AMD64 ABI),
/// loads its CR3, RIP (via RCX), RFLAGS (via R11), and user RSP from
/// the `p_reg` and `p_seg` fields, then zeros all other GPRs and
/// executes `sysretq` to enter (or re-enter) the process in ring 3.
///
/// This is the atomic "switch to process" primitive for the scheduler.
/// The caller MUST save the outgoing process's register state into its
/// `p_reg` TrapFrame before calling `restore()`. Never returns.
///
/// # Safety
///
/// `proc_ptr` must point to a valid `Proc` whose `p_reg` and `p_seg`
/// contain valid user-space register values. Must be called in ring 0
/// with interrupts disabled. Never returns.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn restore(proc_ptr: *const u8) -> ! {
    core::arch::naked_asm!(
        // Load the process's private page table from p_seg.p_cr3.
        "mov    r15, [rdi + 256]",
        "mov    cr3, r15",
        // Load RIP (→RCX) and RFLAGS (→R11) for sysretq.
        "mov    rcx, [rdi + 16]",
        "mov    r11, [rdi + 72]",
        // Load the user stack pointer from p_reg.rsp.
        "mov    rsp, [rdi + 168]",
        // Load RAX from p_reg.rax (syscall return value).
        "mov    rax, [rdi]",
        // Zero remaining GPRs.
        "xor    rbx, rbx",
        "xor    rdx, rdx",
        "xor    rsi, rsi",
        "xor    rdi, rdi",
        "xor    r8, r8",
        "xor    r9, r9",
        "xor    r10, r10",
        "xor    r12, r12",
        "xor    r13, r13",
        "xor    r14, r14",
        "xor    r15, r15",
        // Jump to ring 3.
        "sysretq",
    );
}

// The syscall entry point and handler pointer are only available on the
// kernel target (x86_64-pc-minix), not on the host build (Windows tests).
#[cfg(target_os = "none")]
pub mod syscall_abi {
    use core::sync::atomic::AtomicU64;

    /// Global pointer to the syscall C handler.
    #[unsafe(no_mangle)]
    pub static SYSCALL_HANDLER_PTR: AtomicU64 = AtomicU64::new(0);

    /// Set the syscall handler pointer.
    ///
    /// # Safety
    ///
    /// Caller must ensure the page table is valid and the virtual address is mapped.
    pub unsafe fn set_syscall_handler(handler: unsafe extern "C" fn(*const u64)) {
        SYSCALL_HANDLER_PTR.store(
            handler as usize as u64,
            core::sync::atomic::Ordering::Release,
        );
    }

    /// Load the raw handler pointer value.
    pub fn get_syscall_handler_raw() -> u64 {
        SYSCALL_HANDLER_PTR.load(core::sync::atomic::Ordering::Acquire)
    }

    /// Syscall entry point — called by hardware via `syscall` instruction.
    ///
    /// # Safety
    ///
    /// `entry` must point to a valid, writable page table entry.
    #[unsafe(no_mangle)]
    #[unsafe(naked)]
    pub unsafe extern "C" fn syscall_entry() {
        core::arch::naked_asm!(
            "swapgs",
            "push   r15",
            "push   r14",
            "push   r13",
            "push   r12",
            "push   r11",
            "push   r10",
            "push   r9",
            "push   r8",
            "push   rdi",
            "push   rsi",
            "push   rdx",
            "push   rcx",
            "push   rbx",
            "push   rax",
            "mov    rdi, rsp",
            "sub    rsp, 32",
            "mov    rax, [{ptr}]",
            "test   rax, rax",
            "jz     1f",
            "call   rax",
            "1:",
            "add    rsp, 32",
            "pop    rax",
            "pop    rbx",
            "pop    rcx",
            "pop    rdx",
            "pop    rsi",
            "pop    rdi",
            "pop    r8",
            "pop    r9",
            "pop    r10",
            "pop    r11",
            "pop    r12",
            "pop    r13",
            "pop    r14",
            "pop    r15",
            "swapgs",
            "sysretq",
            ptr = sym SYSCALL_HANDLER_PTR,
        );
    }
}

// FPU: FXSAVE, FXRSTOR, FNINIT, FNCLEX

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn fxsave(buf: &mut [u8; 512]) {
    unsafe {
        asm!("fxsave [{}]", in(reg) buf.as_mut_ptr(), options(nostack));
    }
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn fxrstor(buf: &[u8; 512]) {
    unsafe {
        asm!("fxrstor [{}]", in(reg) buf.as_ptr(), options(nostack));
    }
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn fninit() {
    unsafe {
        asm!("fninit", options(nomem, nostack));
    }
}

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn fnclex() {
    unsafe {
        asm!("fnclex", options(nomem, nostack));
    }
}

// TSC / CPUID

///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
#[inline]
pub unsafe fn rdtsc() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack),
        );
    }
    (low as u64) | ((high as u64) << 32)
}

/// Execute the CPUID instruction.
///
/// # Safety
///
/// Executes a privileged instruction. Caller must be in ring 0.
pub unsafe fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    let a: u32;
    let b: u32;
    let c: u32;
    let d: u32;
    unsafe {
        asm!(
            "push rbx",
            "mov eax, ecx",
            "cpuid",
            "mov esi, ebx",
            "pop rbx",
            "mov edi, edx",
            out("eax") a,
            out("esi") b,
            lateout("ecx") c,
            out("edi") d,
            in("ecx") leaf,
            options(preserves_flags, nomem, nostack),
        );
    }
    (a, b, c, d)
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_functions_compile() {
        // Verify the function signatures compile at the type level
        fn _is_fn(_f: unsafe fn(u16) -> u8) {}
        fn _is_fn2(_f: unsafe fn() -> u64) {}
        fn _is_fn3(_f: unsafe fn(u64)) {}
        _is_fn(inb);
        _is_fn2(read_cr3);
        _is_fn3(write_cr3);
    }

    #[test]
    fn test_ld_dr_returns_u64() {
        // ld_dr returns u64; calling with invalid reg returns 0
        let val = unsafe { ld_dr(99) };
        assert_eq!(val, 0);
    }

    #[test]
    fn test_msr_star() {
        assert_eq!(crate::cpu_msr::msr::STAR, 0xC000_0081);
    }

    #[test]
    fn test_rdtsc_monotonic() {
        // RDTSC should return non-decreasing values.
        let a = unsafe { rdtsc() };
        let b = unsafe { rdtsc() };
        assert!(b >= a, "TSC must be monotonic");
    }

    #[test]
    fn test_cpuid_basic_info() {
        // Leaf 0 returns the maximum basic leaf in EAX.
        let (eax, ebx, ecx, edx) = unsafe { cpuid(0) };
        assert!(eax >= 1, "cpuid leaf 0 should report at least leaf 1");
        // Vendor string: ebx:edx:ecx = "GenuineIntel" or "AuthenticAMD" etc.
        // Just verify none of the vendor string registers are zero (some VM
        // environments may have different strings, but non-zero is a safe bet).
        assert!(
            ebx != 0 || edx != 0 || ecx != 0,
            "cpuid vendor string should not be all-zero"
        );
    }

    #[test]
    fn test_str_sel_result() {
        // STR returns the segment selector of the Task Register.
        // STR is accessible from ring 3, so this is safe in a test binary.
        let sel = unsafe { str_sel() };
        // In long mode, the TR is always present and has a non-zero selector.
        assert!(sel != 0, "Task Register selector must be non-zero");
    }

    #[test]
    fn test_sgdt_result() {
        // SGDT is accessible from ring 3; it stores the GDTR.
        let mut desc: [u8; 10] = [0u8; 10];
        unsafe {
            sgdt(&mut desc);
        }
        let _limit = u16::from_ne_bytes([desc[0], desc[1]]);
        let base = u64::from_ne_bytes([
            desc[2], desc[3], desc[4], desc[5], desc[6], desc[7], desc[8], desc[9],
        ]);
        assert!(base != 0, "GDT base must be non-zero");
    }

    #[test]
    fn test_cpuid_extended_info() {
        // Extended leaf 0x80000000 reports the maximum extended leaf.
        let (eax, _, _, _) = unsafe { cpuid(0x80000000u32) };
        // Must be >= 0x80000000; reasonable systems support at least
        // 0x80000008 (address space sizes).
        assert!(eax >= 0x80000000, "max extended leaf must be >= 0x80000000");
    }

    #[test]
    fn test_rdtsc_returns_u64() {
        let tsc = unsafe { rdtsc() };
        // TSC is a monotonically increasing counter, so any value is valid.
        let _: u64 = tsc;
    }

    #[test]
    fn test_fx_and_fn_compiles() {
        // FPU init instructions (FNINIT, FNCLEX) are accessible from ring 3.
        unsafe {
            fninit();
            fnclex();
        }
    }

    #[test]
    fn test_read_cr0_2_3_4_type_check() {
        // Verify the function signatures: read_cr* return u64.
        // We cannot call these from usermode (privileged instruction), so
        // we verify the types statically.
        fn _u64_fn(_: unsafe fn() -> u64) {}
        _u64_fn(read_cr0);
        _u64_fn(read_cr2);
        _u64_fn(read_cr3);
        _u64_fn(read_cr4);
    }

    #[test]
    fn test_hlt_type_check() {
        // HLT is ring-0 only; verify the signature compiles.
        fn _void_fn(_: unsafe fn()) {}
        _void_fn(hlt);
    }

    #[test]
    fn test_inb_outb_type_check() {
        // IN/OUT are privileged in long mode; verify signatures compile.
        fn _in(_: unsafe fn(u16) -> u8) {}
        fn _out(_: unsafe fn(u16, u8)) {}
        _in(inb);
        _out(outb);
    }
}
