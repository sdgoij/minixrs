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

/// CPU relax hint.
#[inline]
pub fn pause() {
    unsafe {
        core::arch::asm!("pause", options(nomem, nostack));
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

/// Initialize per-CPU local storage.
///
/// # Safety
///
/// Must be called once during early boot on the BSP.
pub unsafe fn init_cpulocals() {
    unsafe { crate::cpulocals::init_cpulocals() }
}

// ── Scheduler cpulocals accessors ──────────────────────────────────────

/// Get the run queue head pointer array from per-CPU storage.
pub fn sched_run_q_head() -> *mut [*mut core::ffi::c_void; 16] {
    unsafe { crate::cpulocals::CPU_LOCAL_STORAGE.run_q_head_ptr() }
}

/// Get the run queue tail pointer array from per-CPU storage.
pub fn sched_run_q_tail() -> *mut [*mut core::ffi::c_void; 16] {
    unsafe { crate::cpulocals::CPU_LOCAL_STORAGE.run_q_tail_ptr() }
}

/// Number of scheduling priority queues (16).
pub fn sched_nr_queues() -> usize {
    crate::cpulocals::NR_SCHED_QUEUES
}

/// Get the current process pointer (scheduler context).
pub fn sched_current_proc() -> *mut core::ffi::c_void {
    unsafe { crate::cpulocals::get_cpulocal_proc_ptr() }
}

/// Get the billable process pointer.
pub fn sched_bill_proc() -> *mut core::ffi::c_void {
    unsafe { crate::cpulocals::CPU_LOCAL_STORAGE.bill_ptr() }
}

/// Set the billable process pointer.
///
/// # Safety
///
/// `proc` must point to a valid `Proc` or be null.
pub unsafe fn sched_set_bill_proc(proc: *mut core::ffi::c_void) {
    unsafe { crate::cpulocals::CPU_LOCAL_STORAGE.set_bill_ptr(proc) }
}

/// Get the current process pointer (SMP context).
pub fn smp_proc_ptr() -> *mut core::ffi::c_void {
    unsafe { crate::cpulocals::get_cpulocal_proc_ptr() }
}

/// Set the current process pointer (SMP context).
///
/// # Safety
///
/// `proc` must point to a valid `Proc` or be null.
pub unsafe fn smp_set_proc_ptr(proc: *mut core::ffi::c_void) {
    unsafe { crate::cpulocals::set_cpulocal_proc_ptr(proc) }
}

/// Halt the CPU (single `hlt` instruction, no infinite loop).
pub fn hlt() {
    unsafe {
        core::arch::asm!("hlt", options(nomem, nostack));
    }
}

// ── Timestamp counter ────────────────────────────────────────────────────

/// Read the timestamp counter.
pub fn read_tsc() -> u64 {
    crate::hw::read_tsc()
}

/// Release the FPU for a process.
///
/// # Safety
///
/// `proc` must point to a valid `Proc` that owns the FPU state.
pub unsafe fn release_fpu(proc: *mut core::ffi::c_void) {
    unsafe { crate::hw::release_fpu(proc) }
}

/// Flush the entire TLB.
///
/// # Safety
///
/// Must be called after page table modifications.
pub unsafe fn tlb_flush() {
    unsafe { crate::asm::tlb_flush() }
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

/// Acquire the Big Kernel Lock (BKL).
///
/// # Safety
///
/// Must be paired with a subsequent `bkl_unlock()`. Nested locking is
/// not supported and will deadlock.
pub unsafe fn bkl_lock() {
    unsafe { crate::spinlock::bkl_lock() }
}

/// Release the Big Kernel Lock (BKL).
///
/// # Safety
///
/// Must be called from the same context that acquired the lock.
pub unsafe fn bkl_unlock() {
    unsafe { crate::spinlock::bkl_unlock() }
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
    if offset > 248 {
        panic!(
            "write_frame_field: offset {} out of range (max 248)",
            offset
        );
    }
    let bytes = val.to_ne_bytes();
    for (i, b) in bytes.iter().enumerate() {
        unsafe {
            core::ptr::write_volatile(frame.as_mut_ptr().add(offset + i), *b);
        }
    }
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
/// Kernel base virtual address.
pub const KERNBASE: u64 = 0xFFFF8000_00000000u64;

/// Page table entry type (x86_64: 8-byte PTE with 4-level paging).
pub type PtEntry = u64;

/// Number of page table levels (x86_64: 4-level: PML4→PDPT→PD→PT).
pub const fn pt_levels() -> u32 {
    4
}

/// Extract the page table index at a given level.
/// Level 0 = PT (offset 12), level 1 = PD (offset 21),
/// level 2 = PDPT (offset 30), level 3 = PML4 (offset 39).
pub const fn pt_index(va: u64, level: u32) -> usize {
    ((va >> (12 + level * 9)) & 0x1FF) as usize
}

/// PTE flag: present / valid bit.
pub const fn pte_present() -> u64 {
    0x0000000000000001 // PG_P
}
/// PTE flag: writable.
pub const fn pte_writable() -> u64 {
    0x0000000000000002 // PG_RW
}
/// PTE flag: user-accessible.
pub const fn pte_user() -> u64 {
    0x0000000000000004 // PG_U
}
/// PTE flag: large page (2MB / 1GB).
pub const fn pte_large_page() -> u64 {
    0x0000000000000080 // PG_PS
}
/// PTE flag: global page (not flushed on CR3 write).
pub const fn pte_global() -> u64 {
    0x0000000000000100 // PG_G
}
/// Physical address page mask (bits 12-51).
pub const fn pte_frame_mask() -> u64 {
    0x000FFFFFFFFFF000 // PG_FRAME
}
/// Lower PTE flags mask (bits 0-11).
pub const fn pte_flags_mask() -> u64 {
    0x0000000000000FFF // PG_PTEMASK
}

/// Kernel load virtual address (x86_64: identity-mapped at 0x200000).
pub const fn kern_vaddr() -> u64 {
    0x200000
}

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

/// Read the page fault address (x86_64: CR2 register).
///
/// # Safety
///
/// Must be called from a page fault handler context.
pub unsafe fn read_fault_addr() -> u64 {
    unsafe { crate::asm::read_cr2() }
}

// ── Port I/O (x86_64-specific, used by do_devio / do_vdevio / do_sdevio) ──

/// Read a byte from an I/O port.
pub unsafe fn inb(port: u16) -> u8 {
    unsafe { crate::asm::inb(port) }
}
/// Write a byte to an I/O port.
pub unsafe fn outb(port: u16, val: u8) {
    unsafe { crate::asm::outb(port, val) }
}
/// Read a word (2 bytes) from an I/O port.
pub unsafe fn inw(port: u16) -> u16 {
    unsafe { crate::asm::inw(port) }
}
/// Write a word (2 bytes) to an I/O port.
pub unsafe fn outw(port: u16, val: u16) {
    unsafe { crate::asm::outw(port, val) }
}
/// Read a long (4 bytes) from an I/O port.
pub unsafe fn inl(port: u16) -> u32 {
    unsafe { crate::asm::inl(port) }
}
/// Write a long (4 bytes) to an I/O port.
pub unsafe fn outl(port: u16, val: u32) {
    unsafe { crate::asm::outl(port, val) }
}
/// String input from an I/O port (byte) to a physical buffer.
pub unsafe fn phys_insb(port: u16, buf: u64, count: usize) {
    unsafe { crate::asm::phys_insb(port, buf, count) }
}
/// String output to an I/O port (byte) from a physical buffer.
pub unsafe fn phys_outsb(port: u16, buf: u64, count: usize) {
    unsafe { crate::asm::phys_outsb(port, buf, count) }
}
/// String input from an I/O port (word) to a physical buffer.
pub unsafe fn phys_insw(port: u16, buf: u64, count: usize) {
    unsafe { crate::asm::phys_insw(port, buf, count) }
}
/// String output to an I/O port (word) from a physical buffer.
pub unsafe fn phys_outsw(port: u16, buf: u64, count: usize) {
    unsafe { crate::asm::phys_outsw(port, buf, count) }
}

// ── Profile clock (RTC-based) ────────────────────────────────────────────

/// Initialize the profiling clock. `rate_code` encodes the RTC divider.
/// `callback` is invoked on each tick. Returns the IRQ number (≥0) or <0 on
/// failure.
pub unsafe fn init_profile_clock(rate_code: u32, callback: unsafe extern "C" fn()) -> i32 {
    let irq = unsafe { crate::apic::arch_init_profile_clock(rate_code as u8) };
    if irq >= 0 {
        let vector = crate::interrupt::VECTOR_TIMER as u32 + irq as u32;
        let handler_fn = crate::apic::profile_clock_isr_entry as *const () as u64;
        unsafe {
            (*crate::idt::IDT.get()).set_handler(vector as usize, handler_fn, 0, 3);
        }
        unsafe { crate::apic::set_profile_clock_handler(callback) };
    }
    irq
}

/// Stop the profiling clock.
pub fn stop_profile_clock() {
    unsafe { crate::apic::arch_stop_profile_clock() }
}

// Stub linker symbols for builds without the kernel linker script.
// The linker script (`minix-raw.ld`) defines these from the sections.
// These stubs prevent unresolved symbol errors in dev/test builds.
#[cfg(any(
    target_os = "windows",
    all(target_os = "none", not(target_vendor = "pc"))
))]
#[used]
#[unsafe(no_mangle)]
pub static __bss_start: u8 = 0;
#[cfg(any(
    target_os = "windows",
    all(target_os = "none", not(target_vendor = "pc"))
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

// ── Tests ─────────────────────────────────────────────────────────────────

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
    extern crate std;
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
            write_frame_field(&mut f, 0, 0xDEADBEEF);
            assert_eq!(read_frame_field(&f, 0), 0xDEADBEEF);
        }
    }

    #[test]
    fn write_retval_writes_to_offset_0() {
        let mut f = frame_default();
        unsafe {
            write_retval(&mut f, 42);
            assert_eq!(read_frame_field(&f, 0), 42);
        }
    }

    #[test]
    fn read_syscall_nr_from_rax() {
        let mut f = frame_default();
        unsafe {
            write_frame_field(&mut f, 0, 59); // NR_WAITPID
            assert_eq!(read_syscall_nr(&f), 59);
        }
    }

    #[test]
    fn read_syscall_args_by_index() {
        let mut f = frame_default();
        unsafe {
            // x86_64: arg0=rdi(40), arg1=rsi(32), arg2=rdx(24),
            // arg3=r10(64), arg4=r8(48), arg5=r9(56)
            write_frame_field(&mut f, 40, 10); // rdi = arg0
            write_frame_field(&mut f, 32, 20); // rsi = arg1
            write_frame_field(&mut f, 24, 30); // rdx = arg2
            write_frame_field(&mut f, 64, 40); // r10 = arg3
            write_frame_field(&mut f, 48, 50); // r8  = arg4
            write_frame_field(&mut f, 56, 60); // r9  = arg5
            assert_eq!(read_syscall_arg(&f, 0), 10);
            assert_eq!(read_syscall_arg(&f, 1), 20);
            assert_eq!(read_syscall_arg(&f, 2), 30);
            assert_eq!(read_syscall_arg(&f, 3), 40);
            assert_eq!(read_syscall_arg(&f, 4), 50);
            assert_eq!(read_syscall_arg(&f, 5), 60);
        }
    }

    #[test]
    fn read_frame_ip_from_offset_160() {
        let mut f = frame_default();
        unsafe {
            write_frame_field(&mut f, 160, 0x401000);
            assert_eq!(read_frame_ip(&f), 0x401000);
        }
    }

    #[test]
    fn write_frame_ip_writes_to_offset_160() {
        let mut f = frame_default();
        unsafe {
            write_frame_ip(&mut f, 0x401000);
            assert_eq!(read_frame_field(&f, 160), 0x401000);
        }
    }

    #[test]
    fn set_initial_regs_sets_rcx_r11_rsp_rdi() {
        let mut f = frame_default();
        unsafe {
            set_initial_regs(&mut f, 0x401000, 0x7FFF_F000, 0x7FFF_F000);
            // rcx (offset 16) = entry
            assert_eq!(read_frame_field(&f, 16), 0x401000);
            // r11 (offset 72) = PSL_USERSET = 0x0202
            assert_eq!(read_frame_field(&f, 72), 0x0202);
            // rsp (offset 168) = stack pointer
            assert_eq!(read_frame_field(&f, 168), 0x7FFF_F000);
            // rdi (offset 40) = arg0
            assert_eq!(read_frame_field(&f, 40), 0x7FFF_F000);
        }
    }

    #[test]
    fn copy_frame_copies_all_256_bytes() {
        let mut src = frame_default();
        let mut dst = frame_default();
        unsafe {
            write_frame_field(&mut src, 0, 0x1234);
            write_frame_field(&mut src, 200, 0x5678);
            copy_frame(&mut dst, &src);
        }
        assert_eq!(dst, src);
    }

    #[test]
    fn trapframe_mcontext_roundtrip_preserves_regs() {
        let mut f = frame_default();
        unsafe {
            write_frame_field(&mut f, 0, 0xAAAA); // rax
            write_frame_field(&mut f, 160, 0xBBBB); // rip
            write_frame_field(&mut f, 168, 0xCCCC); // rsp

            let mc = trapframe_to_mcontext(&f);
            assert_eq!(mc.mc_rax, 0xAAAA);
            assert_eq!(mc.mc_rip, 0xBBBB);
            assert_eq!(mc.mc_rsp, 0xCCCC);

            let mut f2 = frame_default();
            mcontext_to_trapframe(&mut f2, &mc);
            assert_eq!(f2, f);
        }
    }

    #[test]
    fn frame_field_out_of_bounds_panics() {
        let mut f = frame_default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            unsafe { write_frame_field(&mut f, 252, 0) };
        }));
        assert!(result.is_err(), "offset 252+8 > 256 should panic");
    }
}
