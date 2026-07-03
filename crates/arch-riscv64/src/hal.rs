//! RISC-V64 HAL stub implementation.
//!
//! This module provides the minimal HAL exports needed for the kernel
//! crate to compile for riscv64. Real implementations are deferred to
//! their respective Phase 19 sub-tasks.

use core::sync::atomic::Ordering;

// ── Initialization ────────────────────────────────────────────────────────

/// Initialize RISC-V64 architecture subsystem (SBI, PLIC, CLINT, etc.).
pub fn init() {
    crate::init();
}

// ── Serial port I/O (SBI console, Phase 19.3) ────────────────────────────

pub fn serial_write_byte(_byte: u8) {
    todo!("SBI console putchar; see Phase 19.3");
}

pub fn serial_read_byte() -> u8 {
    todo!("SBI console getchar; see Phase 19.3");
}

pub fn serial_byte_available() -> bool {
    todo!("SBI console poll; see Phase 19.3");
}

// ── Cycle counter ─────────────────────────────────────────────────────────

pub fn read_cycles() -> u64 {
    todo!("RISC-V cycle CSR (mcycle/cycle); see Phase 19.4");
}

// ── Halt ──────────────────────────────────────────────────────────────────

pub fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

// ── Per-CPU current process pointer ───────────────────────────────────────

use core::ffi::c_void;

pub unsafe fn set_current_proc(_proc: *mut c_void) {
    todo!("RISC-V tp-relative per-CPU data; see Phase 19.13");
}

pub fn current_proc() -> *mut c_void {
    todo!("RISC-V tp-relative per-CPU data; see Phase 19.13");
}

// ── Spinlocks ─────────────────────────────────────────────────────────────

pub struct Spinlock(core::sync::atomic::AtomicBool);

impl Spinlock {
    pub const fn new() -> Self {
        Self(core::sync::atomic::AtomicBool::new(false))
    }

    pub fn acquire(&self) {
        while self
            .0
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            unsafe {
                core::arch::asm!("pause", options(nomem, nostack));
            }
        }
    }

    pub fn release(&self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Default for Spinlock {
    fn default() -> Self {
        Self::new()
    }
}

pub unsafe fn bkl_lock() {
    todo!("RISC-V BKL; see Phase 19.5");
}

pub unsafe fn bkl_unlock() {
    todo!("RISC-V BKL; see Phase 19.5");
}

// ── TrapFrame accessors (Phase 19.4 — trap handler) ──────────────────────

// RISC-V TrapFrame layout (32 GPR + sepc + sstatus + scause = 35 × 8 = 280 bytes)
// We use the same [u8; 256] layout as x86_64 for now. Expand to 288 if needed later.

pub unsafe fn read_frame_field(frame: &[u8; 256], offset: usize) -> u64 {
    u64::from_ne_bytes(frame[offset..offset + 8].try_into().unwrap())
}

pub unsafe fn write_frame_field(frame: &mut [u8; 256], offset: usize, val: u64) {
    frame[offset..offset + 8].copy_from_slice(&val.to_ne_bytes());
}

pub unsafe fn read_syscall_arg(frame: &[u8; 256], i: usize) -> u64 {
    // RISC-V syscall convention: a0-a5 for args 0-5
    // a0 = x10 at offset 80, a1 = x11 at offset 88, etc.
    let offset = match i {
        0 => 80,  // a0 (x10)
        1 => 88,  // a1 (x11)
        2 => 96,  // a2 (x12)
        3 => 104, // a3 (x13)
        4 => 112, // a4 (x14)
        5 => 120, // a5 (x15)
        _ => 0,
    };
    unsafe { read_frame_field(frame, offset) }
}

pub unsafe fn write_retval(frame: &mut [u8; 256], val: u64) {
    // RISC-V: return value in a0 (x10 at offset 80)
    unsafe { write_frame_field(frame, 80, val) }
}

pub unsafe fn read_syscall_nr(frame: &[u8; 256]) -> u64 {
    // RISC-V: syscall number in a7 (x17 at offset 136)
    unsafe { read_frame_field(frame, 136) }
}

pub unsafe fn read_frame_ip(frame: &[u8; 256]) -> u64 {
    // RISC-V: sepc at offset 256 (above the 32 GPRs)
    // But our frame is only 256 bytes, so this won't fit with all 32 regs.
    // TODO: expand frame to 288 bytes for RISC-V full register set.
    unsafe { read_frame_field(frame, 248) } // temporary: last 8 bytes of 256
}

pub unsafe fn write_frame_ip(_frame: &mut [u8; 256], _ip: u64) {
    todo!("RISC-V sepc write; see Phase 19.4");
}

pub unsafe fn set_initial_regs(frame: &mut [u8; 256], entry: u64, sp: u64, _arg: u64) {
    // RISC-V: set up initial register state for new process.
    // sepc = entry, sp = stack pointer, a0 = arg.
    unsafe {
        write_frame_field(frame, 248, entry); // sepc (temporary location)
        write_frame_field(frame, 8, sp); // sp (x2 at offset 8)
        write_frame_field(frame, 80, 0); // a0 (x10) = 0
    }
}

pub unsafe fn copy_frame(dst: &mut [u8; 256], src: &[u8; 256]) {
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr(), 256);
    }
}

pub fn frame_default() -> [u8; 256] {
    [0u8; 256]
}

pub unsafe fn arch_proc_init(
    _frame: &mut [u8; 256],
    _entry: u64,
    _stack: u64,
    _name: &[u8],
    _ps_str: u64,
) {
    todo!("RISC-V arch_proc_init; see Phase 19.4");
}

pub unsafe fn trapframe_to_mcontext(_frame: &[u8; 256]) -> Mcontext {
    todo!("RISC-V mcontext; see Phase 19.6");
}

pub unsafe fn mcontext_to_trapframe(_frame: &mut [u8; 256], _mc: &Mcontext) {
    todo!("RISC-V mcontext; see Phase 19.6");
}

/// Stub mcontext for RISC-V until Phase 19.6.
pub struct Mcontext {
    pub mc_rax: u64,
    pub mc_rip: u64,
    pub mc_rsp: u64,
}

// ── Page table constants (Phase 19.5 — SV39 page tables) ──────────────────

pub const PAGE_SIZE: u64 = 4096;
pub const PAGE_SHIFT: u64 = 12;
pub const MAP_PRESENT: u64 = 0x0000000000000001; // V (Valid)
pub const MAP_WRITE: u64 = 0x0000000000000002; // W (Writable)
pub const MAP_USER: u64 = 0x0000000000000004; // X (eXecutable) — note: RISC-V uses X, not U here
pub const MAP_NX: u64 = 0x0000000000000000; // RISC-V: no NX bit; use X bit inverted
pub const MAX_USER_ADDRESS: u64 = 0x0000003FFFFFFF; // SV39: 39-bit address space top

pub fn boot_cr3() -> u64 {
    todo!("RISC-V SATP register read; see Phase 19.5");
}

pub unsafe fn write_cr3(_cr3: u64) {
    todo!("RISC-V SATP register write; see Phase 19.5");
}

pub unsafe fn read_cr3() -> u64 {
    todo!("RISC-V SATP register read; see Phase 19.5");
}

pub unsafe fn tlb_flush_page(_va: u64) {
    todo!("RISC-V sfence.vma; see Phase 19.5");
}

pub unsafe fn alloc_phys_page() -> Option<u64> {
    todo!("RISC-V physical allocator; see Phase 19.9");
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinlock_acquire_release() {
        let lock = Spinlock::new();
        lock.acquire();
        lock.release();
    }

    #[test]
    fn frame_default_is_zeroed() {
        let f = frame_default();
        assert_eq!(f.len(), 256);
        assert!(f.iter().all(|&b| b == 0));
    }

    #[test]
    fn read_write_frame_field_roundtrip() {
        let mut f = frame_default();
        unsafe {
            write_frame_field(&mut f, 0, 42);
            assert_eq!(read_frame_field(&f, 0), 42);
        }
    }
}
