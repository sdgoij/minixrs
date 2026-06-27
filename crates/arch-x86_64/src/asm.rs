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

#![allow(clippy::missing_safety_doc)]

use core::arch::asm;

// ═════════════════════════════════════════════════════════════════════════
// I/O port access (byte, word, dword)
// ═════════════════════════════════════════════════════════════════════════

/// Read a byte from an I/O port.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack));
    }
    value
}

/// Read a word (16-bit) from an I/O port.
#[inline]
pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack));
    }
    value
}

/// Read a dword (32-bit) from an I/O port.
#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack));
    }
    value
}

/// Write a byte to an I/O port.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
    }
}

/// Write a word (16-bit) to an I/O port.
#[inline]
pub unsafe fn outw(port: u16, value: u16) {
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack));
    }
}

/// Write a dword (32-bit) to an I/O port.
#[inline]
pub unsafe fn outl(port: u16, value: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack));
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Interrupt control
// ═════════════════════════════════════════════════════════════════════════

/// Disable interrupts (clear IF flag).
#[inline]
pub unsafe fn intr_disable() {
    unsafe {
        asm!("cli", options(nomem, nostack));
    }
}

/// Enable interrupts (set IF flag).
#[inline]
pub unsafe fn intr_enable() {
    unsafe {
        asm!("sti", options(nomem, nostack));
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Debug register access
// ═════════════════════════════════════════════════════════════════════════

/// Read a debug register (DR0–DR3, DR6, DR7).
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

// ═════════════════════════════════════════════════════════════════════════
// Memory copy (physical/physical)
// ═════════════════════════════════════════════════════════════════════════

/// Copy memory from one physical address to another using `rep movsb`.
///
/// # Safety
/// - `src` and `dst` must point to valid, mapped memory.
/// - The regions must not overlap.
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

// ═════════════════════════════════════════════════════════════════════════
// I/O port array operations (string I/O)
// ═════════════════════════════════════════════════════════════════════════

/// Input an array of bytes from an I/O port to memory.
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

// ═════════════════════════════════════════════════════════════════════════
// MSR access
// ═════════════════════════════════════════════════════════════════════════

/// Read an MSR.
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

// ═════════════════════════════════════════════════════════════════════════
// Context switch
// ═════════════════════════════════════════════════════════════════════════

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

// ═════════════════════════════════════════════════════════════════════════
// CR register access
// ═════════════════════════════════════════════════════════════════════════

#[inline]
pub unsafe fn read_cr0() -> u64 {
    let value: u64;
    unsafe {
        asm!("mov rax, cr0", out("rax") value, options(nomem, nostack));
    }
    value
}

#[inline]
pub unsafe fn write_cr0(value: u64) {
    unsafe {
        asm!("mov cr0, rax", in("rax") value, options(nomem, nostack));
    }
}

#[inline]
pub unsafe fn read_cr2() -> u64 {
    let value: u64;
    unsafe {
        asm!("mov rax, cr2", out("rax") value, options(nomem, nostack));
    }
    value
}

#[inline]
pub unsafe fn read_cr3() -> u64 {
    let value: u64;
    unsafe {
        asm!("mov rax, cr3", out("rax") value, options(nomem, nostack));
    }
    value
}

#[inline]
pub unsafe fn write_cr3(value: u64) {
    unsafe {
        asm!("mov cr3, rax", in("rax") value, options(nomem, nostack));
    }
}

#[inline]
pub unsafe fn read_cr4() -> u64 {
    let value: u64;
    unsafe {
        asm!("mov rax, cr4", out("rax") value, options(nomem, nostack));
    }
    value
}

#[inline]
pub unsafe fn write_cr4(value: u64) {
    unsafe {
        asm!("mov cr4, rax", in("rax") value, options(nomem, nostack));
    }
}

// ═════════════════════════════════════════════════════════════════════════
// GDT/IDT load
// ═════════════════════════════════════════════════════════════════════════

#[inline]
pub unsafe fn lgdt(gdtr: &[u8; 10]) {
    unsafe {
        asm!("lgdt [{}]", in(reg) gdtr.as_ptr(), options(nostack));
    }
}

#[inline]
pub unsafe fn lidt(idtr: &[u8; 10]) {
    unsafe {
        asm!("lidt [{}]", in(reg) idtr.as_ptr(), options(nostack));
    }
}

#[inline]
pub unsafe fn ltr(selector: u16) {
    unsafe {
        asm!("ltr {:x}", in(reg) selector, options(nomem, nostack));
    }
}

// ═════════════════════════════════════════════════════════════════════════
// TLB management
// ═════════════════════════════════════════════════════════════════════════

#[inline]
pub unsafe fn invlpg(addr: u64) {
    unsafe {
        asm!("invlpg [{}]", in(reg) addr, options(nostack));
    }
}

#[inline]
pub unsafe fn tlb_flush() {
    unsafe {
        let cr3 = read_cr3();
        write_cr3(cr3);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Halt
// ═════════════════════════════════════════════════════════════════════════

#[inline]
pub unsafe fn hlt() {
    unsafe {
        asm!("hlt", options(nomem, nostack));
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_functions_compile() {
        // Verify the function signatures compile at the type level
        fn _is_fn(_f: unsafe fn(u16) -> u8) {}
        fn _is_fn2(_f: unsafe fn() -> u64) {}
        fn _is_fn3(_f: unsafe fn(u64)) {}
        let _ = _is_fn(inb);
        let _ = _is_fn2(read_cr3);
        let _ = _is_fn3(write_cr3);
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
}
