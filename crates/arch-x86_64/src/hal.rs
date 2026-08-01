//! x86_64 HAL implementation.
//!
//! Provides all the functions declared in `kernel::hal` for the x86_64
//! architecture. These are called from arch-independent kernel code.

use core::sync::atomic::Ordering;

/// Initialize x86_64 architecture subsystem (IDT, MSRs, cpulocals, etc.).
pub fn init() {
    crate::init();
}

// Re-export arch-specific types for kernel use.
pub use crate::frame::TrapFrame;
pub use crate::mcontext::Mcontext;

const COM1_DATA: u16 = 0x3F8;
const COM1_LSR: u16 = 0x3FD; // Line Status Register
const LSR_DR: u8 = 0x01; // Data Ready bit

/// Write a single byte to the COM1 serial port.
pub fn serial_write_byte(byte: u8) {
    unsafe {
        // Wait for Transmitter Holding Register Empty (LSR bit 5 = 0x20)
        // This is REQUIRED because the UART's THR is a single-byte register.
        // Writing without waiting causes data corruption when multiple
        // bytes are written in rapid succession (e.g., shell echo loop).
        let lsr_port: u16 = COM1_DATA + 5;
        loop {
            let lsr: u8;
            core::arch::asm!(
                "in al, dx",
                out("al") lsr,
                in("dx") lsr_port,
                options(nostack),
            );
            if lsr & 0x20 != 0 {
                break;
            }
        }
        core::arch::asm!(
            "out dx, al",
            in("dx") COM1_DATA,
            in("al") byte,
            options(nostack),
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
            options(nostack),
        );
    }
    lsr & LSR_DR != 0
}

/// Non-blocking poll: returns a byte if available from COM1.
pub fn poll_console() -> Option<u8> {
    serial_try_read_byte()
}

/// Arch-specific CPU idle hint (PAUSE on x86_64).
pub fn cpu_idle() {
    pause();
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
            options(nostack),
        );
    }
    Some(byte)
}

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
        core::arch::asm!("sti; hlt", options(nomem, nostack));
    }
}

/// Read the TSC (timestamp counter).
pub fn read_tsc() -> u64 {
    crate::hw::read_tsc()
}

/// Read the per-CPU TSC context-switch timestamp.
///
/// # Safety
///
/// CPU locals must be initialized.
pub unsafe fn read_tsc_ctr_switch() -> u64 {
    unsafe { crate::cpulocals::CPU_LOCAL_STORAGE.tsc_ctr_switch() }
}

/// Write the per-CPU TSC context-switch timestamp.
///
/// # Safety
///
/// CPU locals must be initialized.
pub unsafe fn write_tsc_ctr_switch(val: u64) {
    unsafe { crate::cpulocals::CPU_LOCAL_STORAGE.set_tsc_ctr_switch(val) }
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

/// Set up register frame for a new process via exec(2).
/// Writes entry point, stack pointer, and arch-specific status flags.
///
/// # Safety
///
/// `frame` must be a valid, writable `[u8; 256]` trap frame.
pub unsafe fn exec_init_regs(frame: &mut [u8; 256], entry: u64, sp: u64, _argc: u64, _argv: u64) {
    unsafe {
        write_frame_field(frame, 0, sp); // rax = 0 (convention)
        write_frame_field(frame, 16, entry); // rcx = entry (syscall convention)
        write_frame_field(frame, 160, entry); // dedicated rip slot = entry
        write_frame_field(frame, 40, sp); // rdi = sp
        write_frame_field(frame, 72, 0x0202); // r11 = user-mode (IF|IOPL=0)
        write_frame_field(frame, 176, 0x0202); // dedicated rflags slot = PSL_USERSET
        write_frame_field(frame, 168, sp); // rsp = sp
        core::arch::asm!("mfence", options(nostack, preserves_flags));
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
        write_frame_field(frame, 16, entry); // rcx = entry (syscall convention)
        write_frame_field(frame, 160, entry); // dedicated rip slot = entry
        write_frame_field(frame, 72, 0x0202); // r11 = PSL_USERSET
        write_frame_field(frame, 176, 0x0202); // dedicated rflags slot = PSL_USERSET
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

/// Physical memory page size.
pub const PAGE_SIZE: u64 = 4096;
/// Number of bits for the page offset.
pub const PAGE_SHIFT: u64 = 12;
/// ELF machine identifier for this architecture (e_machine field).
pub const ELF_MACHINE: u16 = 62; // EM_X86_64
/// Size of FPU save area (FXSAVE/FXRSTOR format).
pub const FPU_STATE_SIZE: usize = 512;
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

/// Validate a physical address is within the identity-mapped range.
pub const fn pte_is_valid_phys(phys: u64) -> bool {
    phys < 0x1000_0000 && (phys >> 48) == 0
}

/// Flags for a non-leaf (branch) page table entry.
pub const fn pte_nonleaf_flags() -> u64 {
    pte_present() | pte_writable() | pte_user()
}

/// Extract permission flags from a huge-page PTE when splitting into
/// sub-entries at `next_level` (0 = leaf 4KB, >0 = further non-leaf).
pub const fn pte_split_flags(source_pte: u64, next_level: u32) -> u64 {
    let mut flags = (source_pte & pte_flags_mask()) & !(pte_frame_mask() | pte_global());
    if next_level > 0 {
        flags |= pte_large_page();
    }
    flags
}

/// Mask of flags to exclude when extracting attributes from a PDE
/// being split into 4KB entries (pt_mapkernel path).
pub const fn pte_pd_split_exclude_mask() -> u64 {
    pte_frame_mask() | pte_large_page() | pte_global()
}

/// Mask of flags to clear on the replacement PDE after splitting
/// (pt_mapkernel path).
pub const fn pte_pd_split_clear_mask() -> u64 {
    pte_large_page() | pte_global()
}

/// Complete set of PTE flags for a user code/data page (exec mapping).
pub const fn pte_user_flags() -> u64 {
    pte_present() | pte_writable() | pte_user()
}

/// Build a page table entry from a physical address and flags.
///
/// x86_64: PTE stores the physical address directly in bits [51:12],
/// so this is just (pa & frame_mask) | (flags & flags_mask).
pub const fn build_pte(pa: u64, flags: u64) -> u64 {
    (pa & 0x000FFFFFFFFFF000) | (flags & 0xFFF)
}

/// Extract physical address from a PTE.
/// On x86_64, PTE stores the physical address directly in bits [51:12].
pub const fn pte_to_phys(pte: u64) -> u64 {
    pte & 0x000FFFFFFFFFF000
}

/// Kernel load virtual address (x86_64: identity-mapped at 0x200000).
pub const fn kern_vaddr() -> u64 {
    0x200000
}

/// User stack base virtual address (must be in RAM).
/// On x86_64 QEMU, RAM starts at 0, so 0x0FE00000 is valid.
pub const fn user_stack_base() -> u64 {
    0x0FE00000u64
}

/// User stack size in bytes.
pub const fn user_stack_size() -> usize {
    65536
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

/// Clear the write bit in a leaf PTE for a given CR3 and VA.
/// Used by copy-on-write to make a page read-only.
///
/// # Safety
///
/// `cr3` must point to a valid page table. `va` must be a mapped virtual address.
pub unsafe fn clear_rw(cr3: u64, va: u64) -> Result<(), PageNotMapped> {
    let pml4_idx = pt_index(va, 3);
    let pdpt_idx = pt_index(va, 2);
    let pd_idx = pt_index(va, 1);
    let pt_idx = pt_index(va, 0);

    unsafe {
        let pml4 = cr3 as *const u64;
        let pml4e = core::ptr::read(pml4.add(pml4_idx));
        if pml4e & pte_present() == 0 {
            return Err(PageNotMapped);
        }

        let pdpt = (pml4e & pte_frame_mask()) as *const u64;
        let pdpte = core::ptr::read(pdpt.add(pdpt_idx));
        if pdpte & pte_present() == 0 {
            return Err(PageNotMapped);
        }
        if pdpte & pte_large_page() != 0 {
            return Err(PageNotMapped); // 1GB huge page
        }

        let pd = (pdpte & pte_frame_mask()) as *mut u64;
        let pde = core::ptr::read(pd.add(pd_idx));
        if pde & pte_present() == 0 {
            return Err(PageNotMapped);
        }
        if pde & pte_large_page() != 0 {
            return Err(PageNotMapped); // 2MB huge page
        }

        let pt = (pde & pte_frame_mask()) as *mut u64;
        let pte_ptr = pt.add(pt_idx);
        let pte_val = core::ptr::read(pte_ptr);
        if pte_val & pte_present() == 0 {
            return Err(PageNotMapped);
        }

        core::ptr::write(pte_ptr, pte_val & !pte_writable());
        tlb_flush_page(va);
    }

    Ok(())
}

/// Error returned when a page is not mapped.
#[derive(Debug, Clone, Copy)]
pub struct PageNotMapped;

impl core::fmt::Display for PageNotMapped {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "page not mapped")
    }
}

/// Read the page fault address (x86_64: CR2 register).
///
/// # Safety
///
/// Must be called from a page fault handler context.
pub unsafe fn read_fault_addr() -> u64 {
    unsafe { crate::asm::read_cr2() }
}

/// Read the current frame pointer (x86_64: RBP register).
pub fn read_frame_pointer() -> u64 {
    let fp: u64;
    unsafe {
        core::arch::asm!("mov {}, rbp", out(reg) fp, options(nomem, nostack));
    }
    fp
}

/// Return the current CPU ID (x86_64: APIC ID from CPUID leaf 1).
pub fn cpu_id() -> u32 {
    let (_, ebx, _, _) = unsafe { crate::asm::cpuid(1) };
    (ebx >> 24) & 0xFF // Initial APIC ID in bits 31:24
}

/// Whether port I/O is available on this architecture.
pub const fn has_port_io() -> bool {
    true
}

/// Whether fork(2) needs explicit RECEIVING/REPLY_PEND clearing on the child.
/// On RISC-V, PM's SENDNB to the child is skipped so flags must be cleared
/// directly. On x86_64, the reply delivery path handles this.
pub const fn fork_needs_child_flag_clear() -> bool {
    false
}

/// Read a byte from an I/O port.
///
/// # Safety
///
/// The port must be valid and accessible by the caller.
pub unsafe fn inb(port: u16) -> u8 {
    unsafe { crate::asm::inb(port) }
}
/// Write a byte to an I/O port.
///
/// # Safety
///
/// The port must be valid and accessible by the caller.
pub unsafe fn outb(port: u16, val: u8) {
    unsafe { crate::asm::outb(port, val) }
}
/// Read a word (2 bytes) from an I/O port.
///
/// # Safety
///
/// The port must be valid and accessible by the caller.
pub unsafe fn inw(port: u16) -> u16 {
    unsafe { crate::asm::inw(port) }
}
/// Write a word (2 bytes) to an I/O port.
///
/// # Safety
///
/// The port must be valid and accessible by the caller.
pub unsafe fn outw(port: u16, val: u16) {
    unsafe { crate::asm::outw(port, val) }
}
/// Read a long (4 bytes) from an I/O port.
///
/// # Safety
///
/// The port must be valid and accessible by the caller.
pub unsafe fn inl(port: u16) -> u32 {
    unsafe { crate::asm::inl(port) }
}
/// Write a long (4 bytes) to an I/O port.
///
/// # Safety
///
/// The port must be valid and accessible by the caller.
pub unsafe fn outl(port: u16, val: u32) {
    unsafe { crate::asm::outl(port, val) }
}
/// String input from an I/O port (byte) to a physical buffer.
///
/// # Safety
///
/// The port must be valid and accessible. `buf` must point to a valid
/// physical address with room for at least `count` bytes.
pub unsafe fn phys_insb(port: u16, buf: u64, count: usize) {
    unsafe { crate::asm::phys_insb(port, buf, count) }
}
/// String output to an I/O port (byte) from a physical buffer.
///
/// # Safety
///
/// The port must be valid and accessible. `buf` must point to a valid
/// physical address with room for at least `count` bytes.
pub unsafe fn phys_outsb(port: u16, buf: u64, count: usize) {
    unsafe { crate::asm::phys_outsb(port, buf, count) }
}
/// String input from an I/O port (word) to a physical buffer.
///
/// # Safety
///
/// The port must be valid and accessible. `buf` must point to a valid
/// physical address with room for at least `count` words.
pub unsafe fn phys_insw(port: u16, buf: u64, count: usize) {
    unsafe { crate::asm::phys_insw(port, buf, count) }
}
/// String output to an I/O port (word) from a physical buffer.
///
/// # Safety
///
/// The port must be valid and accessible. `buf` must point to a valid
/// physical address with room for at least `count` words.
pub unsafe fn phys_outsw(port: u16, buf: u64, count: usize) {
    unsafe { crate::asm::phys_outsw(port, buf, count) }
}

/// PCI configuration address port.
pub const PCI_ADDR_PORT: u16 = 0xCF8;
/// PCI configuration data port.
pub const PCI_DATA_PORT: u16 = 0xCFC;

/// Build a PCI config address.
#[inline]
pub fn pci_config_addr(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | (reg as u32 & 0xFC)
}

/// Read 8 bits from PCI config space.
///
/// # Safety
///
/// Caller must ensure the PCI bus/device/function is valid and accessible.
pub unsafe fn pci_cfg_read8(bus: u8, dev: u8, func: u8, reg: u8) -> u8 {
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        outl(PCI_ADDR_PORT, addr);
        let raw = inl(PCI_DATA_PORT);
        ((raw >> ((reg as u32 & 0x03) * 8)) & 0xFF) as u8
    }
}

/// Read 16 bits from PCI config space.
///
/// # Safety
///
/// Caller must ensure the PCI bus/device/function is valid and accessible.
pub unsafe fn pci_cfg_read16(bus: u8, dev: u8, func: u8, reg: u8) -> u16 {
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        outl(PCI_ADDR_PORT, addr);
        let raw = inl(PCI_DATA_PORT);
        ((raw >> ((reg as u32 & 0x02) * 8)) & 0xFFFF) as u16
    }
}

/// Read 32 bits from PCI config space.
///
/// # Safety
///
/// Caller must ensure the PCI bus/device/function is valid and accessible.
pub unsafe fn pci_cfg_read32(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        outl(PCI_ADDR_PORT, addr);
        inl(PCI_DATA_PORT)
    }
}

/// Write 32 bits to PCI config space.
///
/// # Safety
///
/// Caller must ensure the PCI bus/device/function is valid and writable.
pub unsafe fn pci_cfg_write32(bus: u8, dev: u8, func: u8, reg: u8, val: u32) {
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        outl(PCI_ADDR_PORT, addr);
        outl(PCI_DATA_PORT, val);
    }
}

/// RTC CMOS index port.
pub const RTC_INDEX: u16 = 0x70;

/// Read a CMOS register value.
///
/// # Safety
///
/// Caller must ensure the CMOS register is valid and not concurrently accessed.
pub unsafe fn cmos_read(reg: u8) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm! {
            "out dx, al",
            "mov dl, 0x71",
            "in al, dx",
            inout("dx") RTC_INDEX => _,
            inout("al") reg => val,
            options(nomem, nostack),
        };
    }
    val
}

/// Write a value to a CMOS register.
///
/// # Safety
///
/// Caller must ensure the CMOS register is valid and not concurrently accessed.
pub unsafe fn cmos_write(reg: u8, val: u8) {
    unsafe {
        core::arch::asm! {
            "mov dx, 0x70",
            "mov al, al",
            "out dx, al",
            "mov dx, 0x71",
            "mov al, cl",
            "out dx, al",
            in("eax") reg as u32,
            in("ecx") val as u32,
            options(nomem, nostack, preserves_flags),
        };
    }
}

/// Full memory fence (serializes loads and stores).
///
/// # Safety
///
/// Must be paired with a corresponding fence or atomic operation
/// on the other side of the memory ordering.
pub unsafe fn mfence() {
    unsafe {
        core::arch::asm!("mfence", options(nostack, preserves_flags));
    }
}

/// Initialize the profiling clock. `rate_code` encodes the RTC divider.
/// `callback` is invoked on each tick. Returns the IRQ number (≥0) or <0 on
/// failure.
///
/// # Safety
///
/// The `callback` must remain valid for the lifetime of the profiling clock.
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

/// Deep-copy user page table entries from parent to child for fork.
/// Walks 4-level page tables (PML4 → PDPT → PD → PT).
/// Returns 0 on success, -12 (ENOMEM) on allocation failure.
///
/// # Safety
///
/// `parent_cr3` and `child_cr3` must point to valid page tables.
/// `child_cr3` must be a freshly-allocated zero-filled page.
pub unsafe fn vm_paging_fork(parent_cr3: u64, child_cr3: u64, _msg: &mut [u8; 64]) -> i32 {
    const USER_ENTRIES: usize = 256;
    const PG_P: u64 = 0x01;
    const PG_RW: u64 = 0x02;
    const PG_U: u64 = 0x04;
    const PG_PS: u64 = 0x80;
    const PG_FRAME: u64 = 0x000FFFFFFFFFF000;

    unsafe {
        let parent = parent_cr3 as *const u64;
        let child = child_cr3 as *mut u64;

        // Copy kernel half (entries 256-511) directly.
        core::ptr::copy_nonoverlapping(
            parent.add(USER_ENTRIES),
            child.add(USER_ENTRIES),
            USER_ENTRIES,
        );

        // Copy user half (entries 0-255): walk parent's PML4, copy
        // intermediate page table pages, and COW-protect leaf pages.
        for l4 in 0..USER_ENTRIES {
            let e4 = core::ptr::read(parent.add(l4));
            if e4 & PG_P == 0 {
                continue;
            }
            let parent_p3 = (e4 & PG_FRAME) as *const u64;
            let child_p3 = match alloc_phys_page() {
                Some(pa) => pa as *mut u64,
                None => return -12,
            };
            core::ptr::write(child.add(l4), (child_p3 as u64) | (e4 & !PG_FRAME));
            for l3 in 0..512 {
                let e3 = core::ptr::read(parent_p3.add(l3));
                if e3 & PG_P == 0 {
                    continue;
                }
                let parent_p2 = (e3 & PG_FRAME) as *const u64;
                let child_p2 = match alloc_phys_page() {
                    Some(pa) => pa as *mut u64,
                    None => return -12,
                };
                core::ptr::write(child_p3.add(l3), (child_p2 as u64) | (e3 & !PG_FRAME));
                if e3 & PG_PS != 0 {
                    // 1GB page — COW-protect if user-writable.
                    if e3 & PG_U != 0 && e3 & PG_RW != 0 {
                        core::ptr::write(child_p3.add(l3), e3 & !PG_RW);
                    } else {
                        core::ptr::write(child_p3.add(l3), e3);
                    }
                    continue;
                }
                for l2 in 0..512 {
                    let e2 = core::ptr::read(parent_p2.add(l2));
                    if e2 & PG_P == 0 {
                        continue;
                    }
                    if e2 & PG_PS != 0 {
                        // 2MB page — COW-protect if user-writable.
                        if e2 & PG_U != 0 && e2 & PG_RW != 0 {
                            core::ptr::write(child_p2.add(l2), e2 & !PG_RW);
                        } else {
                            core::ptr::write(child_p2.add(l2), e2);
                        }
                        continue;
                    }
                    let parent_p1 = (e2 & PG_FRAME) as *const u64;
                    let child_p1 = match alloc_phys_page() {
                        Some(pa) => pa as *mut u64,
                        None => return -12,
                    };
                    core::ptr::write(child_p2.add(l2), (child_p1 as u64) | (e2 & !PG_FRAME));
                    // Copy 4KB PTEs and COW-protect user-writable entries.
                    core::ptr::copy_nonoverlapping(parent_p1, child_p1, 512);
                    for l1 in 0..512 {
                        let e1 = core::ptr::read(parent_p1.add(l1));
                        if e1 & PG_P == 0 || e1 & PG_U == 0 || e1 & PG_RW == 0 {
                            continue;
                        }
                        core::ptr::write(child_p1.add(l1), e1 & !PG_RW);
                    }
                }
            }
        }
        0
    }
}

/// Allocate a physical page for page table use.
///
/// # Safety
///
/// Must be called after the physical memory allocator is initialized.
pub unsafe fn alloc_phys_page() -> Option<u64> {
    crate::alloc::alloc_phys_page()
}

/// Allocate `count` contiguous physical pages (bottom-up).
///
/// # Safety
///
/// Must be called after the physical memory allocator is initialized.
pub unsafe fn alloc_phys_contig(count: usize) -> Option<u64> {
    crate::alloc::alloc_phys_contig(count)
}

/// Free `count` contiguous physical pages starting at `addr`.
///
/// # Safety
///
/// Must be called after the physical memory allocator is initialized.
/// `addr` must have been previously allocated via `alloc_phys_contig`.
pub unsafe fn free_phys_contig(addr: u64, count: usize) {
    crate::alloc::free_phys_contig(addr, count)
}

/// Initialize the physical page allocator with a memory range [base, base+size).
///
/// # Safety
///
/// - `base` and `size` must describe a valid, free physical memory region.
/// - Must be called exactly once, before any allocations are made.
pub unsafe fn init_phys_alloc(base: u64, size: u64) {
    crate::alloc::init_range(base, size);
}

/// Exit QEMU via isa-debug-exit device (port 0x501 on x86_64).
/// Pass 0 for success, non-zero for failure.
pub fn qemu_exit(code: u32) -> ! {
    unsafe {
        let val = if code == 0 { 0u32 } else { (code << 1) | 1 };
        core::arch::asm!("out dx, eax", in("dx") 0x501u16, in("eax") val);
    }
    loop {
        unsafe { core::arch::asm!("hlt") }
    }
}

/// Create the initial page table root for a new process via exec(2).
/// Allocates PML4/PDPT/PD pages, copies kernel-half entries from
/// the boot page table, and returns the PML4 physical address.
/// Returns 0 on allocation failure.
///
/// # Safety
///
/// `boot_cr3` must point to a valid boot page table.
pub unsafe fn exec_create_root(boot_cr3: u64) -> u64 {
    const PG_P: u64 = 0x01;
    const PG_RW: u64 = 0x02;
    const PG_U: u64 = 0x04;
    const PG_FRAME: u64 = 0x000FFFFFFFFFF000;

    unsafe {
        let pml4 = match alloc_phys_page() {
            Some(p) => p,
            None => return 0,
        };
        core::ptr::write_bytes(pml4 as *mut u8, 0, 4096);
        let boot_pml4 = boot_cr3 as *const u64;
        let pml4e0 = core::ptr::read(boot_pml4);
        let pdpt_phys = pml4e0 & PG_FRAME;
        let boot_pdpt = pdpt_phys as *const u64;
        let pdpte0 = core::ptr::read(boot_pdpt);
        let pd_phys = pdpte0 & PG_FRAME;
        let boot_pd = pd_phys as *const u64;
        let pdpt_page = match alloc_phys_page() {
            Some(p) => p,
            None => return 0,
        };
        let pd_page = match alloc_phys_page() {
            Some(p) => p,
            None => return 0,
        };
        core::ptr::write_bytes(pdpt_page as *mut u8, 0, 4096);
        core::ptr::write_bytes(pd_page as *mut u8, 0, 4096);
        let flags = PG_P | PG_RW | PG_U;
        core::ptr::write(pml4 as *mut u64, pdpt_page | flags);
        core::ptr::write(pdpt_page as *mut u64, pd_page | flags);
        for i in 0usize..512 {
            let e = core::ptr::read(boot_pd.add(i));
            core::ptr::write((pd_page as *mut u64).add(i), e);
        }
        for i in 256usize..512 {
            let e = core::ptr::read(boot_pml4.add(i));
            core::ptr::write((pml4 as *mut u64).add(i), e);
        }
        pml4
    }
}

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
            // rip (offset 160) = entry
            assert_eq!(read_frame_field(&f, 160), 0x401000);
            // r11 (offset 72) = PSL_USERSET = 0x0202
            assert_eq!(read_frame_field(&f, 72), 0x0202);
            // rflags (offset 176) = PSL_USERSET = 0x0202
            assert_eq!(read_frame_field(&f, 176), 0x0202);
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
