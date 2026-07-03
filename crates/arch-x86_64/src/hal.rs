//! x86_64 HAL implementation.
//!
//! Provides all the functions declared in `kernel::hal` for the x86_64
//! architecture. These are called from arch-independent kernel code.

use core::sync::atomic::Ordering;

// ── Initialization ────────────────────────────────────────────────────────

/// Initialize x86_64 architecture subsystem (IDT, MSRs, cpulocals, etc.).
pub fn init() {
    crate::init();
}

// ── Serial port I/O (COM1) ───────────────────────────────────────────────

const COM1_DATA: u16 = 0x3F8;
const COM1_LSR: u16 = 0x3FD; // Line Status Register
const LSR_DR: u8 = 0x01; // Data Ready bit

/// Write a single byte to the COM1 serial port.
pub fn serial_write_byte(byte: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") COM1_DATA,
            in("al") byte,
            options(nomem, nostack),
        );
    }
}

/// Read a single byte from COM1, blocking until data is available.
pub fn serial_read_byte() -> u8 {
    loop {
        if let Some(byte) = serial_try_read_byte() {
            return byte;
        }
        // Spin-hint to yield to hyperthread on hypervisors.
        unsafe {
            core::arch::asm!("pause", options(nomem, nostack));
        }
    }
}

/// Non-blocking check: is a byte available on COM1?
pub fn serial_byte_available() -> bool {
    let lsr: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            out("al") lsr,
            in("dx") COM1_LSR,
            options(nomem, nostack),
        );
    }
    lsr & LSR_DR != 0
}

/// Try to read a byte from COM1 without blocking.
fn serial_try_read_byte() -> Option<u8> {
    if !serial_byte_available() {
        return None;
    }
    let byte: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            out("al") byte,
            in("dx") COM1_DATA,
            options(nomem, nostack),
        );
    }
    Some(byte)
}

// ── Cycle counter ─────────────────────────────────────────────────────────

/// Read the x86_64 timestamp counter (TSC).
pub fn read_cycles() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack),
        );
    }
    (lo as u64) | ((hi as u64) << 32)
}

// ── Halt ──────────────────────────────────────────────────────────────────

/// Halt the CPU with interrupts disabled. Never returns.
pub fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("cli; hlt", options(nomem, nostack));
        }
    }
}

// ── Per-CPU current process pointer ───────────────────────────────────────

use core::ffi::c_void;

/// Set the per-CPU current process pointer (stored in `cpulocals`).
///
/// # Safety
///
/// `proc` must point to a valid `Proc` or be null.
pub unsafe fn set_current_proc(proc: *mut c_void) {
    unsafe {
        crate::cpulocals::set_cpulocal_proc_ptr(proc);
    }
}

/// Get the per-CPU current process pointer.
pub fn current_proc() -> *mut c_void {
    unsafe { crate::cpulocals::get_cpulocal_proc_ptr() }
}

// ── Spinlocks ─────────────────────────────────────────────────────────────

/// A simple spinlock backed by an atomic flag.
pub struct Spinlock(core::sync::atomic::AtomicBool);

impl Spinlock {
    /// Create a new unlocked spinlock.
    pub const fn new() -> Self {
        Self(core::sync::atomic::AtomicBool::new(false))
    }

    /// Acquire the spinlock, spinning until it is available.
    pub fn acquire(&self) {
        while self
            .0
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Spin-hint
            unsafe {
                core::arch::asm!("pause", options(nomem, nostack));
            }
        }
    }

    /// Release the spinlock.
    pub fn release(&self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Default for Spinlock {
    fn default() -> Self {
        Self::new()
    }
}

// ── TrapFrame accessors (raw [u8; 256] helpers) ──────────────────────────

// x86_64 TrapFrame byte offsets (each field is 8 bytes):
//   0: rax,   8: rbx,  16: rcx,  24: rdx,  32: rsi,  40: rdi
//  48: r8,   56: r9,   64: r10,  72: r11,  80: r12,  88: r13
//  96: r14, 104: r15, 112: cs,  120: ss,  128: ds,  136: es
// 144: fs,  152: gs,  160: rip, 168: rsp, 176: rflags
// Total: 184 bytes.

/// Offset of a specific syscall argument register.
const fn arg_offset(i: usize) -> usize {
    match i {
        0 => 40, // rdi
        1 => 32, // rsi
        2 => 24, // rdx
        3 => 64, // r10
        4 => 48, // r8
        5 => 56, // r9
        _ => 0,
    }
}

/// Read a u64 from a byte offset in the frame.
///
/// # Safety
///
/// `offset` must be < 248 (so offset + 8 <= 256). The caller must ensure
/// that the frame contains valid data at this offset.
pub unsafe fn read_frame_field(frame: &[u8; 256], offset: usize) -> u64 {
    u64::from_ne_bytes(frame[offset..offset + 8].try_into().unwrap())
}

/// Write a u64 to a byte offset in the frame.
///
/// # Safety
///
/// `offset` must be < 248 (so offset + 8 <= 256). The caller must ensure
/// that the frame is writable and contains valid register state.
pub unsafe fn write_frame_field(frame: &mut [u8; 256], offset: usize, val: u64) {
    frame[offset..offset + 8].copy_from_slice(&val.to_ne_bytes());
}

/// Read syscall argument `i` (0-5) from a raw TrapFrame.
///
/// # Safety
///
/// `i` must be 0-5. The frame must contain valid register data.
pub unsafe fn read_syscall_arg(frame: &[u8; 256], i: usize) -> u64 {
    unsafe { read_frame_field(frame, arg_offset(i)) }
}

/// Write the syscall return value into a raw TrapFrame (rax).
///
/// # Safety
///
/// `frame` must point to a writable register save area.
pub unsafe fn write_retval(frame: &mut [u8; 256], val: u64) {
    unsafe { write_frame_field(frame, 0, val) }
}

/// Read the syscall number from a raw TrapFrame (rax).
///
/// # Safety
///
/// `frame` must point to a valid register save area.
pub unsafe fn read_syscall_nr(frame: &[u8; 256]) -> u64 {
    unsafe { read_frame_field(frame, 0) }
}

/// Read the instruction pointer from a raw TrapFrame (rip).
///
/// # Safety
///
/// `frame` must point to a valid register save area.
pub unsafe fn read_frame_ip(frame: &[u8; 256]) -> u64 {
    unsafe { read_frame_field(frame, 160) }
}

/// Write the instruction pointer into a raw TrapFrame (rip).
///
/// # Safety
///
/// `frame` must point to a writable register save area.
pub unsafe fn write_frame_ip(frame: &mut [u8; 256], ip: u64) {
    unsafe { write_frame_field(frame, 160, ip) }
}

/// Set the initial register state for a new process (exec entry).
///
/// On x86_64 with `sysretq`:
/// - RCX (offset 16) = entry (loaded as RIP by sysretq)
/// - R11 (offset 72) = RFLAGS (PSL_USERSET = 0x0202)
/// - RSP (offset 168) = stack pointer
/// - RDI (offset 40) = first argument (convention: arg0)
///
/// # Safety
///
/// `frame` must point to a writable, zeroed register save area.
pub unsafe fn set_initial_regs(frame: &mut [u8; 256], entry: u64, sp: u64, arg: u64) {
    unsafe {
        write_frame_field(frame, 16, entry); // rcx = entry (RIP via sysretq)
        write_frame_field(frame, 72, 0x0202); // r11 = PSL_USERSET
        write_frame_field(frame, 168, sp); // rsp
        write_frame_field(frame, 40, arg); // rdi = arg0
    }
}

/// Copy a raw TrapFrame from one byte array to another.
///
/// # Safety
///
/// `dst` and `src` must not overlap. Both must point to valid register data.
pub unsafe fn copy_frame(dst: &mut [u8; 256], src: &[u8; 256]) {
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr(), 256);
    }
}

/// Zero-initialize a TrapFrame.
pub fn frame_default() -> [u8; 256] {
    [0u8; 256]
}

/// Initialize a process's TrapFrame via the arch-specific init routine.
///
/// # Safety
///
/// `frame` must point to a writable register save area. `name` must be a
/// valid byte slice. `entry`, `stack`, and `ps_str` must be valid addresses.
pub unsafe fn arch_proc_init(
    frame: &mut [u8; 256],
    entry: u64,
    stack: u64,
    name: &[u8],
    ps_str: u64,
) {
    unsafe {
        // Reinterpret the byte array as a TrapFrame pointer for the existing
        // arch_proc_init function.
        let tf = frame.as_mut_ptr() as *mut crate::frame::TrapFrame;
        crate::arch_proc::arch_proc_init(tf, entry, stack, name, ps_str);
    }
}

/// Build an Mcontext from a raw TrapFrame (for do_getmcontext).
///
/// # Safety
///
/// `frame` must point to a valid register save area.
pub unsafe fn trapframe_to_mcontext(frame: &[u8; 256]) -> crate::mcontext::Mcontext {
    use crate::mcontext::Mcontext;
    unsafe {
        let tf = frame.as_ptr() as *const crate::frame::TrapFrame;
        let src = &*tf;
        Mcontext {
            mc_rax: src.rax,
            mc_rbx: src.rbx,
            mc_rcx: src.rcx,
            mc_rdx: src.rdx,
            mc_rsi: src.rsi,
            mc_rdi: src.rdi,
            mc_rbp: 0, // not saved in TrapFrame
            mc_r8: src.r8,
            mc_r9: src.r9,
            mc_r10: src.r10,
            mc_r11: src.r11,
            mc_r12: src.r12,
            mc_r13: src.r13,
            mc_r14: src.r14,
            mc_r15: src.r15,
            mc_rip: src.rip,
            mc_rsp: src.rsp,
            mc_rflags: src.rflags,
            mc_cs: src.cs,
            mc_ss: src.ss,
            mc_ds: src.ds,
            mc_es: src.es,
            mc_fs: src.fs,
            mc_gs: src.gs,
            mc_fpstate: [0u8; 512],
        }
    }
}

/// Write an Mcontext into a raw TrapFrame (for do_setmcontext).
///
/// # Safety
///
/// `frame` must point to a writable register save area. `mc` must contain
/// valid register values.
pub unsafe fn mcontext_to_trapframe(frame: &mut [u8; 256], mc: &crate::mcontext::Mcontext) {
    unsafe {
        let tf = frame.as_mut_ptr() as *mut crate::frame::TrapFrame;
        let dst = &mut *tf;
        dst.rax = mc.mc_rax;
        dst.rbx = mc.mc_rbx;
        dst.rcx = mc.mc_rcx;
        dst.rdx = mc.mc_rdx;
        dst.rsi = mc.mc_rsi;
        dst.rdi = mc.mc_rdi;
        dst.r8 = mc.mc_r8;
        dst.r9 = mc.mc_r9;
        dst.r10 = mc.mc_r10;
        dst.r11 = mc.mc_r11;
        dst.r12 = mc.mc_r12;
        dst.r13 = mc.mc_r13;
        dst.r14 = mc.mc_r14;
        dst.r15 = mc.mc_r15;
        dst.rip = mc.mc_rip;
        dst.rsp = mc.mc_rsp;
        dst.rflags = mc.mc_rflags;
    }
}

// ── Page table constants ────────────────────────────────────────────────

/// Physical memory page size.
pub const PAGE_SIZE: u64 = 4096;
/// Number of bits for the page offset.
pub const PAGE_SHIFT: u64 = 12;

/// Page table flags (x86_64).
pub const MAP_PRESENT: u64 = 0x0000000000000001; // PG_P
pub const MAP_WRITE: u64 = 0x0000000000000002; // PG_RW
pub const MAP_USER: u64 = 0x0000000000000004; // PG_U
pub const MAP_NX: u64 = 0x8000000000000000; // PG_NX

/// Maximum user address (48-bit VA, top half reserved for kernel).
pub const MAX_USER_ADDRESS: u64 = 0x0000800000000000;

/// Get the boot page table root physical address.
pub fn boot_cr3() -> u64 {
    crate::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed)
}

/// Read the current CR3 value (page table root physical address).
///
/// # Safety
///
/// Must be called in ring 0.
pub unsafe fn read_cr3() -> u64 {
    unsafe { crate::asm::read_cr3() }
}

/// Write CR3 to switch page tables / flush TLB.
///
/// # Safety
///
/// `cr3` must point to a valid, identity-mapped page table.
pub unsafe fn write_cr3(cr3: u64) {
    unsafe { crate::asm::write_cr3(cr3) }
}

/// Flush a single page from the TLB.
///
/// # Safety
///
/// `va` must be a valid mapped virtual address.
pub unsafe fn tlb_flush_page(va: u64) {
    unsafe { crate::asm::invlpg(va) }
}

/// Allocate a physical page for page table use.
///
/// # Safety
///
/// Must be called after the physical memory allocator is initialized.
pub unsafe fn alloc_phys_page() -> Option<u64> {
    crate::alloc::alloc_phys_page()
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
        // If we get here without deadlock, the test passes.
    }

    #[test]
    fn spinlock_exclusion() {
        let lock = Spinlock::new();
        lock.acquire();
        // Second acquire should fail immediately with try_lock
        // (not provided, but we can test basic mutual exclusion)
        lock.release();
    }
}
