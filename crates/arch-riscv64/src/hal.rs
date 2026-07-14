//! RISC-V64 HAL stub implementation.
//!
//! This module provides the minimal HAL exports needed for the kernel
//! crate to compile for riscv64. Real implementations are deferred to
//! their respective Phase 19 sub-tasks.

use core::sync::atomic::Ordering;

use crate::pte;

/// Initialize RISC-V64 architecture subsystem (SBI, PLIC, CLINT, etc.).
pub fn init() {
    crate::init();
}

/// Write a single byte to the SBI debug console.
pub fn serial_write_byte(byte: u8) {
    crate::sbi::console_putchar(byte);
}

/// Read a byte from the 8250 UART at MMIO 0x10000000 (blocking).
pub fn serial_read_byte() -> u8 {
    unsafe {
        // Wait until data is ready (LSR bit 0 = DR).
        while (core::ptr::read_volatile((0x10000000usize + 5) as *const u8) & 1) == 0 {
            core::hint::spin_loop();
        }
        // Read the data byte from RBR.
        core::ptr::read_volatile(0x10000000usize as *const u8)
    }
}

/// Non-blocking check: is a byte available from the 8250 UART?
pub fn serial_byte_available() -> bool {
    unsafe { (core::ptr::read_volatile((0x10000000usize + 5) as *const u8) & 1) != 0 }
}

pub fn read_cycles() -> u64 {
    todo!("RISC-V cycle CSR (mcycle/cycle); see Phase 19.4");
}

pub fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

/// CPU relax hint.
#[inline]
pub fn pause() {
    core::hint::spin_loop();
}

use core::ffi::c_void;

/// Set the current process pointer for this hart.
///
/// # Safety
///
/// `proc` must point to a valid `Proc` or be null.
pub unsafe fn set_current_proc(proc: *mut c_void) {
    unsafe {
        crate::cpulocals::set_current_proc(proc as u64);
    }
}

pub fn current_proc() -> *mut c_void {
    crate::cpulocals::current_proc() as *mut c_void
}

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
            // RISC-V: use fence + lightweight hint
            unsafe {
                core::arch::asm!("fence", options(nomem, nostack));
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

/// Acquire the big kernel lock.
///
/// # Safety
///
/// Must be called in a context where the lock can be safely acquired.
pub unsafe fn bkl_lock() {
    todo!("RISC-V BKL; see Phase 19.5");
}

/// Release the big kernel lock.
///
/// # Safety
///
/// Must be called by the hart that currently holds the lock.
pub unsafe fn bkl_unlock() {
    todo!("RISC-V BKL; see Phase 19.5");
}

// RISC-V TrapFrame layout (32 GPR + sepc + sstatus + scause = 35 × 8 = 280 bytes)
// We use the same [u8; 256] layout as x86_64 for now. Expand to 288 if needed later.

/// Read a u64 field from a trap frame at the given byte offset.
///
/// # Safety
///
/// `frame` must be a valid trap frame; `offset` must be in bounds.
pub unsafe fn read_frame_field(frame: &[u8; 256], offset: usize) -> u64 {
    u64::from_ne_bytes(frame[offset..offset + 8].try_into().unwrap())
}

/// Write a u64 field to a trap frame at the given byte offset.
///
/// # Safety
///
/// `frame` must be a valid trap frame; `offset` must be in bounds.
pub unsafe fn write_frame_field(frame: &mut [u8; 256], offset: usize, val: u64) {
    frame[offset..offset + 8].copy_from_slice(&val.to_ne_bytes());
}

/// Read a syscall argument from the trap frame.
///
/// # Safety
///
/// `frame` must be a valid trap frame captured from a syscall entry.
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

/// Write a syscall return value into the trap frame.
///
/// # Safety
///
/// `frame` must be a valid trap frame.
pub unsafe fn write_retval(frame: &mut [u8; 256], val: u64) {
    // RISC-V: return value in a0 (x10 at offset 80)
    unsafe { write_frame_field(frame, 80, val) }
}

/// Read the syscall number from the trap frame.
///
/// # Safety
///
/// `frame` must be a valid trap frame captured from a syscall entry.
pub unsafe fn read_syscall_nr(frame: &[u8; 256]) -> u64 {
    // RISC-V: syscall number in a7 (x17 at offset 136)
    unsafe { read_frame_field(frame, 136) }
}

/// Read the faulting instruction pointer (sepc) from the trap frame.
///
/// # Safety
///
/// `frame` must be a valid trap frame.
pub unsafe fn read_frame_ip(frame: &[u8; 256]) -> u64 {
    // RISC-V: sepc stored at offset 0 (x0 slot, never loaded as GPR)
    unsafe { read_frame_field(frame, 0) }
}

/// Write the faulting instruction pointer into the trap frame.
///
/// # Safety
///
/// `frame` must be a valid trap frame.
pub unsafe fn write_frame_ip(_frame: &mut [u8; 256], _ip: u64) {
    todo!("RISC-V sepc write; see Phase 19.4");
}

/// Set initial register values in a trap frame for a new process.
///
/// # Safety
///
/// `frame` must be a valid, writable trap frame.
pub unsafe fn set_initial_regs(frame: &mut [u8; 256], entry: u64, sp: u64, _arg: u64) {
    // RISC-V: set up initial register state for new process.
    // sepc = entry (stored at offset 0 = x0 slot, never loaded as GPR)
    // sp = stack pointer (x2 at offset 16)
    // a0 = arg (x10 at offset 80) = 0
    // sstatus = SPIE | FS_INITIAL (SIE=0, SPIE=1, SPP=0, FS=initial)
    // SIE=0 is CRITICAL: prevents supervisor interrupts from firing between
    // `csrw sstatus` and `sret` in switch_to_user.
    unsafe {
        write_frame_field(frame, 0, entry); // sepc in x0 slot
        write_frame_field(frame, 16, sp); // sp (x2 at offset 16)
        write_frame_field(frame, 80, 0); // a0 (x10) = 0
        write_frame_field(
            frame,
            248,
            crate::psl::sstatus::SPIE | crate::psl::sstatus::FS_INITIAL,
        ); // sstatus: SIE=0, SPIE=1
    }
}

/// Copy a trap frame from `src` to `dst`.
///
/// # Safety
///
/// Both `dst` and `src` must point to valid, non-overlapping trap frames.
pub unsafe fn copy_frame(dst: &mut [u8; 256], src: &[u8; 256]) {
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr(), 256);
    }
}

pub fn frame_default() -> [u8; 256] {
    [0u8; 256]
}

/// Initialize architecture-specific process state in the trap frame.
///
/// # Safety
///
/// `frame` must be a valid, writable trap frame.
pub unsafe fn arch_proc_init(
    _frame: &mut [u8; 256],
    _entry: u64,
    _stack: u64,
    _name: &[u8],
    _ps_str: u64,
) {
    todo!("RISC-V arch_proc_init; see Phase 19.4");
}

/// Convert a trap frame to a machine context (for signal handling).
///
/// # Safety
///
/// `_frame` must be a valid trap frame.
pub unsafe fn trapframe_to_mcontext(_frame: &[u8; 256]) -> crate::mcontext::Mcontext {
    todo!("RISC-V mcontext; see Phase 19.6");
}

/// Restore a trap frame from a machine context.
///
/// # Safety
///
/// `_frame` must be a valid, writable trap frame.
pub unsafe fn mcontext_to_trapframe(_frame: &mut [u8; 256], _mc: &crate::mcontext::Mcontext) {
    todo!("RISC-V mcontext; see Phase 19.6");
}

pub const PAGE_SIZE: u64 = 4096;
pub const PAGE_SHIFT: u64 = 12;

/// Page table entry type (RISC-V SV39: 8-byte PTE with 3-level paging).
pub type PtEntry = u64;

/// Number of page table levels (RISC-V SV39: 3-level: PUD→PMD→PT).
pub const fn pt_levels() -> u32 {
    3
}

/// Extract the page table index at a given level.
/// Level 0 = PT (offset 12), level 1 = PMD (offset 21),
/// level 2 = PUD (offset 30).
pub const fn pt_index(va: u64, level: u32) -> usize {
    ((va >> (12 + level * 9)) & 0x1FF) as usize
}

/// PTE flag: present / valid bit.
pub const fn pte_present() -> u64 {
    pte::PTE_V
}
/// PTE flag: writable (RISC-V: requires both R+W for writable).
pub const fn pte_writable() -> u64 {
    pte::PTE_W
}
/// PTE flag: user-accessible.
pub const fn pte_user() -> u64 {
    pte::PTE_U
}
/// PTE flag: large page indicator (SV39: any R/W/X set at non-leaf level).
pub const fn pte_large_page() -> u64 {
    pte::PTE_R | pte::PTE_W | pte::PTE_X
}
/// PTE flag: global page.
pub const fn pte_global() -> u64 {
    pte::PTE_G
}
/// Physical address page mask (bits 10-53, 44-bit PPN).
pub const fn pte_frame_mask() -> u64 {
    pte::PTE_PPN_MASK
}
/// Lower PTE flags mask (bits 0-9, 10-bit flags).
pub const fn pte_flags_mask() -> u64 {
    pte::PTE_FLAGS_MASK
}

/// Build a page table entry from a physical address and flags.
///
/// RISC-V SV39: PTE stores PPN = pa >> 12 at bits [53:10],
/// NOT the raw physical address (unlike x86_64 where PA is stored directly).
/// This function correctly encodes the PPN for SV39.
pub const fn build_pte(pa: u64, flags: u64) -> u64 {
    // PPN = pa >> 12, stored at PTE bits [53:10]: (PPN << 10) = (pa >> 2)
    // Mask off low 10 bits (flags) and keep just PPN field:
    ((pa >> 2) & pte::PTE_PPN_MASK) | (flags & pte::PTE_FLAGS_MASK)
}

/// Extract physical address from a PTE (reverse of build_pte).
/// On RISC-V, PTE stores PPN = pa >> 12 at bits [53:10], so the physical
/// address is ((pte & PPN_MASK) >> 10) << 12 = (pte & PPN_MASK) << 2.
pub const fn pte_to_phys(pte: u64) -> u64 {
    ((pte & pte::PTE_PPN_MASK) >> 10) << 12
}

/// Kernel load virtual address (RISC-V: linked at 0x80200000).
pub const fn kern_vaddr() -> u64 {
    0x80200000
}

/// User stack base virtual address (must be in RAM).
/// On RISC-V QEMU virt, RAM starts at 0x80000000, so use 0x8FE00000.
pub const fn user_stack_base() -> u64 {
    0x8FE00000u64
}

/// User stack size in bytes.
pub const fn user_stack_size() -> usize {
    65536
}

pub const MAP_PRESENT: u64 = pte::PTE_V;
pub const MAP_WRITE: u64 = pte::PTE_W;
pub const MAP_USER: u64 = pte::PTE_U;
pub const MAP_NX: u64 = 0; // RISC-V: NX is absence of X bit
pub const MAX_USER_ADDRESS: u64 = 0x0000003FFFFFFFFFFF;

pub fn boot_cr3() -> u64 {
    // Read SATP CSR and extract the physical page table address.
    // SATP format: [63:60]=MODE, [59:44]=ASID, [43:0]=PPN (phys>>12)
    // Return the full physical address (like x86_64 CR3).
    unsafe {
        let satp: u64;
        core::arch::asm!("csrr {satp}, satp", satp = out(reg) satp, options(nomem, nostack));
        // Extract PPN (bits [43:0]) and shift to get physical address
        (satp & 0x00000FFFFFFFFFFF) << 12
    }
}

/// Write the SATP register (RISC-V equivalent of x86 CR3).
///
/// # Safety
///
/// `cr3` must point to a valid, page-aligned root page table.
pub unsafe fn write_cr3(cr3: u64) {
    // Write SATP CSR
    // SV39 mode = 8 (bits 60-63), ASID = 0 (bits 44-59), PPN = bits 0-43
    // cr3 is the physical page number (PPN) of the root page table
    let satp = (8u64 << 60) | (cr3 >> 12); // MODE=SV39, PPN=cr3>>12
    unsafe {
        // SAFETY: `nomem` is intentionally omitted — the csrw satp
        // invalidates cached translations, so memory accesses must not
        // be reordered across this instruction.
        core::arch::asm!("csrw satp, {satp}", satp = in(reg) satp, options(nostack));
    }
    // Flush TLB after SATP write
    unsafe {
        // SAFETY: `nomem` omitted — sfence.vma is a TLB invalidation
        // barrier.  Memory accesses must not cross it.
        core::arch::asm!("sfence.vma", options(nostack));
    }
}

/// Read the SATP register (RISC-V equivalent of x86 CR3).
///
/// # Safety
///
/// No special safety requirements; the SATP CSR is always readable.
pub unsafe fn read_cr3() -> u64 {
    boot_cr3()
}

/// Flush the TLB for a single virtual address.
///
/// # Safety
///
/// Must be called after modifying a page table entry.
pub unsafe fn tlb_flush_page(_va: u64) {
    // RISC-V sfence.vma with a single address
    unsafe {
        core::arch::asm!("sfence.vma", options(nomem, nostack));
    }
}

/// Read the page fault address (RISC-V: stval CSR).
///
/// # Safety
///
/// Must be called from a page fault handler context.
pub unsafe fn read_fault_addr() -> u64 {
    let addr: u64;
    unsafe {
        core::arch::asm!("csrr {}, stval", out(reg) addr, options(nomem, nostack));
    }
    addr
}

/// Read the current frame pointer (RISC-V: s0 register).
pub fn read_frame_pointer() -> u64 {
    let fp: u64;
    unsafe {
        core::arch::asm!("addi {}, s0, 0", out(reg) fp, options(nomem, nostack));
    }
    fp
}

/// Return the current CPU ID (RISC-V: mhartid CSR).
pub fn cpu_id() -> u32 {
    let hartid: u64;
    unsafe {
        core::arch::asm!("csrr {}, mhartid", out(reg) hartid, options(nomem, nostack));
    }
    hartid as u32
}

/// Allocate a physical page.
///
/// # Safety
///
/// Must be called after the physical memory allocator has been initialized.
pub unsafe fn alloc_phys_page() -> Option<u64> {
    crate::alloc::alloc_phys_page()
}

/// Allocate `count` contiguous physical pages (bottom-up).
pub unsafe fn alloc_phys_contig(count: usize) -> Option<u64> {
    crate::alloc::alloc_phys_contig(count)
}

/// Read a byte from an I/O port (unimplemented on RISC-V).
pub unsafe fn inb(_port: u16) -> u8 {
    0
}
/// Write a byte to an I/O port (unimplemented on RISC-V).
pub unsafe fn outb(_port: u16, _val: u8) {}
/// Read a word from an I/O port (unimplemented on RISC-V).
pub unsafe fn inw(_port: u16) -> u16 {
    0
}
/// Write a word to an I/O port (unimplemented on RISC-V).
pub unsafe fn outw(_port: u16, _val: u16) {}
/// Read a long from an I/O port (unimplemented on RISC-V).
pub unsafe fn inl(_port: u16) -> u32 {
    0
}
/// Write a long to an I/O port (unimplemented on RISC-V).
pub unsafe fn outl(_port: u16, _val: u32) {}
/// String input from I/O port (byte) to physical buffer (unimplemented on RISC-V).
pub unsafe fn phys_insb(_port: u16, _buf: u64, _count: usize) {}
/// String output to I/O port (byte) from physical buffer (unimplemented on RISC-V).
pub unsafe fn phys_outsb(_port: u16, _buf: u64, _count: usize) {}
/// String input from I/O port (word) to physical buffer (unimplemented on RISC-V).
pub unsafe fn phys_insw(_port: u16, _buf: u64, _count: usize) {}
/// String output to I/O port (word) from physical buffer (unimplemented on RISC-V).
pub unsafe fn phys_outsw(_port: u16, _buf: u64, _count: usize) {}

/// Initialize the profiling clock (no-op on RISC-V).
pub unsafe fn init_profile_clock(_rate_code: u32, _callback: unsafe extern "C" fn()) -> i32 {
    -1
}

/// Stop the profiling clock (no-op on RISC-V).
pub fn stop_profile_clock() {}

// Stub linker symbols for builds without the kernel linker script.
// The RISC-V linker script (`minix-raw-riscv64.ld`) defines these from
// the sections. These stubs prevent unresolved symbol errors in dev/test.
#[cfg(any(
    target_os = "windows",
    all(target_os = "none", not(target_arch = "riscv64"))
))]
#[used]
#[unsafe(no_mangle)]
pub static __bss_start: u8 = 0;
#[cfg(any(
    target_os = "windows",
    all(target_os = "none", not(target_arch = "riscv64"))
))]
#[used]
#[unsafe(no_mangle)]
pub static __bss_end: u8 = 0;

/// Return the kernel BSS start address (linker symbol `__bss_start`).
pub fn bss_start() -> u64 {
    unsafe extern "C" {
        static __bss_start: u8;
    }
    core::ptr::addr_of!(__bss_start) as u64
}

/// Return the kernel BSS end address (linker symbol `__bss_end`).
pub fn bss_end() -> u64 {
    unsafe extern "C" {
        static __bss_end: u8;
    }
    core::ptr::addr_of!(__bss_end) as u64
}

/// Kernel base virtual address (SV39: 0xFFFFFF8000000000+).
pub const KERNBASE: u64 = 0xFFFFFF8000000000u64;

/// Initialize the physical page allocator with a memory range [base, base+size).
///
/// # Safety
///
/// - `base` and `size` must describe a valid, free physical memory region.
/// - Must be called exactly once, before any allocations are made.
pub unsafe fn init_phys_alloc(base: u64, size: u64) {
    unsafe {
        crate::alloc::init_range(base, size);
    }
}

/// Initialize per-CPU local storage.
///
/// # Safety
///
/// Must be called once during early boot on the BSP hart.
pub unsafe fn init_cpulocals() {
    unsafe {
        crate::cpulocals::init_cpulocals();
    }
}

// ── Scheduler cpulocals accessors (Phase 19.7) ──────────────────────────

/// Get the run queue head pointer array.
pub fn sched_run_q_head() -> *mut [*mut core::ffi::c_void; 16] {
    crate::cpulocals::run_q_head_ptr()
}

/// Get the run queue tail pointer array.
pub fn sched_run_q_tail() -> *mut [*mut core::ffi::c_void; 16] {
    crate::cpulocals::run_q_tail_ptr()
}

/// Number of scheduling priority queues.
pub fn sched_nr_queues() -> usize {
    16
}

/// Get the current process pointer (scheduler context).
pub fn sched_current_proc() -> *mut core::ffi::c_void {
    crate::cpulocals::current_proc() as *mut core::ffi::c_void
}

/// Get the billable process pointer.
pub fn sched_bill_proc() -> *mut core::ffi::c_void {
    todo!("RISC-V sched_bill_proc; see Phase 19.7");
}

/// Set the billable process pointer.
/// Set the billable process pointer.
///
/// # Safety
///
/// Must be called from a scheduler context where the pointer is valid.
pub unsafe fn sched_set_bill_proc(_proc: *mut core::ffi::c_void) {
    todo!("RISC-V sched_set_bill_proc; see Phase 19.7");
}

/// Get the current process pointer (SMP context).
pub fn smp_proc_ptr() -> *mut core::ffi::c_void {
    crate::cpulocals::current_proc() as *mut core::ffi::c_void
}

/// Set the current process pointer (SMP context).
/// Set the current process pointer for the current hart (SMP context).
///
/// # Safety
///
/// `proc` must point to a valid `Proc` or be null.
pub unsafe fn smp_set_proc_ptr(proc: *mut core::ffi::c_void) {
    unsafe {
        crate::cpulocals::set_current_proc(proc as u64);
    }
}

/// Halt the CPU (single `wfi` instruction, no infinite loop).
pub fn hlt() {
    unsafe {
        core::arch::asm!("wfi", options(nomem, nostack));
    }
}

// ── Timestamp counter ────────────────────────────────────────────────────

/// Read the timestamp counter.
pub fn read_tsc() -> u64 {
    crate::clint::read_time()
}

/// Read the per-CPU TSC context-switch timestamp (RISC-V: uses read_time).
///
/// # Safety
///
/// CPU locals must be initialized.
pub unsafe fn read_tsc_ctr_switch() -> u64 {
    unsafe { crate::cpulocals::tsc_ctr_switch() }
}

/// Write the per-CPU TSC context-switch timestamp.
///
/// # Safety
///
/// CPU locals must be initialized.
pub unsafe fn write_tsc_ctr_switch(val: u64) {
    unsafe { crate::cpulocals::set_tsc_ctr_switch(val) }
}

/// Release FPU state for a process (no-op on RISC-V).
///
/// # Safety
///
/// `_proc` must point to a valid process or be null.
pub unsafe fn release_fpu(_proc: *mut core::ffi::c_void) {}

/// Flush the entire TLB.
///
/// # Safety
///
/// Must be called after modifying page tables.
pub unsafe fn tlb_flush() {
    unsafe {
        core::arch::asm!("sfence.vma", options(nomem, nostack));
    }
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
