//! AArch64 HAL implementation.
//!
//! Provides all the functions declared in `kernel::hal` for the AArch64
//! architecture. These are called from arch-independent kernel code.

use core::sync::atomic::Ordering;

/// Initialize AArch64 architecture subsystem.
pub fn init() {
    crate::init();
}

// Re-export arch-specific types for kernel use.
pub use crate::frame::TrapFrame;
pub use crate::mcontext::Mcontext;

const UART_BASE: usize = 0x0900_0000;
const UART_DR: usize = UART_BASE + 0x00;
const UART_FR: usize = UART_BASE + 0x18;
const UART_LCR_H: usize = UART_BASE + 0x2C;
const UART_CR: usize = UART_BASE + 0x30;
const UART_IMSC: usize = UART_BASE + 0x38; // Interrupt Mask Set/Clear
const UART_ICR: usize = UART_BASE + 0x44; // Interrupt Clear
const FR_RXFE: u32 = 1 << 4; // Receive FIFO empty
const FR_TXFF: u32 = 1 << 5; // Transmit FIFO full
const IMSC_RXIM: u32 = 1 << 4; // Receive interrupt mask
const LCR_FEN: u32 = 1 << 4; // FIFO enable
const LCR_WLEN_8: u32 = 3 << 5; // 8-bit word length

/// Initialize the PL011 UART.
pub fn uart_init() {
    unsafe {
        // 8-bit, FIFO enabled (LCR_H.FEN). Without FEN the PL011 runs in
        // single-byte mode (RX depth 1): a piped burst overruns and only the
        // first byte survives, which stalls the shell on burst console input.
        core::ptr::write_volatile(UART_LCR_H as *mut u32, LCR_FEN | LCR_WLEN_8);
        // Enable UART: UARTEN | TXE | RXE
        let cr: u32 = core::ptr::read_volatile(UART_CR as *const u32);
        core::ptr::write_volatile(UART_CR as *mut u32, cr | (1 << 0) | (1 << 8) | (1 << 9));
    }
}

/// Enable the PL011 receive interrupt (IMSC RXIM).
///
/// Without this the UART never raises its IRQ line, so piped input is
/// only drained on timer ticks / read_blocking and a burst overruns the
/// RX FIFO while the shell is busy. The GIC must already route SPI 33
/// (see `enable_gic` in kernel-boot) and `el1_irq_handler_c` drains the
/// UART on every IRQ, so enabling the mask is all that is needed.
pub fn enable_rx_interrupt() {
    unsafe {
        // Clear any stale RX interrupt before unmasking.
        core::ptr::write_volatile(UART_ICR as *mut u32, IMSC_RXIM);
        let imsc: u32 = core::ptr::read_volatile(UART_IMSC as *const u32);
        core::ptr::write_volatile(UART_IMSC as *mut u32, imsc | IMSC_RXIM);
    }
}

/// Write a single byte to the PL011 UART.
pub fn serial_write_byte(byte: u8) {
    unsafe {
        // Wait for TX FIFO not full.
        while (core::ptr::read_volatile(UART_FR as *const u32) & FR_TXFF) != 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile(UART_DR as *mut u32, byte as u32);
    }
}

/// Read a single byte from the PL011 UART, blocking until data is available.
pub fn serial_read_byte() -> u8 {
    unsafe {
        while (core::ptr::read_volatile(UART_FR as *const u32) & FR_RXFE) != 0 {
            core::hint::spin_loop();
        }
        (core::ptr::read_volatile(UART_DR as *const u32) & 0xFF) as u8
    }
}

/// Non-blocking check: is a byte available on the PL011 UART?
pub fn serial_byte_available() -> bool {
    unsafe { (core::ptr::read_volatile(UART_FR as *const u32) & FR_RXFE) == 0 }
}

/// Non-blocking poll: returns a byte if available from UART.
pub fn poll_console() -> Option<u8> {
    if serial_byte_available() {
        Some(serial_read_byte())
    } else {
        None
    }
}

/// Arch-specific CPU idle hint.
pub fn cpu_idle() {
    unsafe {
        core::arch::asm!("wfi", options(nomem, nostack));
    }
}

/// Read the generic timer counter.
pub fn read_cycles() -> u64 {
    let val: u64;
    unsafe {
        core::arch::asm!("mrs {val}, cntpct_el0", val = out(reg) val);
    }
    val
}

/// Halt the CPU. Never returns.
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

/// Set the per-CPU current process pointer.
///
/// # Safety
///
/// `proc` must point to a valid Proc or be null.
pub unsafe fn set_current_proc(proc: *mut c_void) {
    unsafe {
        crate::cpulocals::set_current_proc(proc as u64);
    }
}

/// Get the per-CPU current process pointer.
pub fn current_proc() -> *mut c_void {
    crate::cpulocals::current_proc() as *mut c_void
}

/// Initialize per-CPU local storage.
pub unsafe fn init_cpulocals() {
    unsafe { crate::cpulocals::init_cpulocals() }
}

pub fn sched_run_q_head() -> *mut [*mut c_void; 16] {
    crate::cpulocals::run_q_head_ptr()
}

pub fn sched_run_q_tail() -> *mut [*mut c_void; 16] {
    crate::cpulocals::run_q_tail_ptr()
}

pub fn sched_nr_queues() -> usize {
    crate::cpulocals::NR_SCHED_QUEUES
}

pub fn sched_current_proc() -> *mut c_void {
    crate::cpulocals::current_proc() as *mut c_void
}

pub fn sched_bill_proc() -> *mut c_void {
    crate::cpulocals::bill_proc() as *mut c_void
}

pub unsafe fn sched_set_bill_proc(proc: *mut c_void) {
    unsafe {
        crate::cpulocals::set_bill_proc(proc as u64);
    }
}

pub fn smp_proc_ptr() -> *mut c_void {
    crate::cpulocals::current_proc() as *mut c_void
}

pub unsafe fn smp_set_proc_ptr(proc: *mut c_void) {
    unsafe {
        crate::cpulocals::set_current_proc(proc as u64);
    }
}

pub fn hlt() {
    unsafe {
        core::arch::asm!("wfi", options(nomem, nostack));
    }
}

pub fn read_tsc() -> u64 {
    read_cycles()
}

pub unsafe fn read_tsc_ctr_switch() -> u64 {
    crate::cpulocals::tsc_ctr_switch()
}

pub unsafe fn write_tsc_ctr_switch(val: u64) {
    unsafe { crate::cpulocals::set_tsc_ctr_switch(val) }
}

pub unsafe fn release_fpu(_proc: *mut core::ffi::c_void) {}

/// Set the calling thread's tpidr_el0 (the AArch64 thread pointer for TLS).
///
/// # Safety
///
/// `tls` must be a valid user-space address in the current process, or 0.
pub unsafe fn set_tls_current(tls: u64) {
    unsafe {
        core::arch::asm!(
            "msr tpidr_el0, {t}",
            t = in(reg) tls,
            options(nomem, nostack),
        );
    }
}

pub unsafe fn tlb_flush() {
    unsafe {
        core::arch::asm!("tlbi vmalle1; dsb ish; isb", options(nomem, nostack));
    }
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
            unsafe {
                core::arch::asm!("dmb ish", options(nomem, nostack));
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

pub unsafe fn bkl_lock() {}
pub unsafe fn bkl_unlock() {}

/// Read a u64 field from a raw trap frame at the given byte offset.
///
/// # Safety
///
/// `frame` must be a valid trap frame; `offset` must be in bounds.
pub unsafe fn read_frame_field(frame: &[u8; 288], offset: usize) -> u64 {
    u64::from_ne_bytes(frame[offset..offset + 8].try_into().unwrap())
}

/// Write a u64 field to a raw trap frame at the given byte offset.
///
/// # Safety
///
/// `frame` must be a valid, writable trap frame; `offset` must be in bounds.
pub unsafe fn write_frame_field(frame: &mut [u8; 288], offset: usize, val: u64) {
    frame[offset..offset + 8].copy_from_slice(&val.to_ne_bytes());
}

/// Set up register frame for a new process via exec(2).
///
/// # Safety
///
/// `frame` must be a valid, writable raw trap frame.
pub unsafe fn exec_init_regs(frame: &mut [u8; 288], entry: u64, sp: u64, argc: u64, argv: u64) {
    unsafe {
        // ELR_EL1 = entry point (offset 256).
        write_frame_field(frame, 256, entry);
        // SP (x2) = stack pointer (offset 16).
        write_frame_field(frame, 16, sp);
        // x0 = argc (offset 0).
        write_frame_field(frame, 0, argc);
        // x1 = argv (offset 8).
        write_frame_field(frame, 8, argv);
        // SP_EL0 = stack pointer (offset 248). Must match x2 so
        // _start's [sp] references read from the exec'd stack.
        write_frame_field(frame, 248, sp);
        // SPSR_EL1 = 0 (EL0t, interrupts unmasked).
        write_frame_field(frame, 264, 0);
    }
}

/// Read a syscall argument from the trap frame.
/// AArch64: arguments in x0-x5 (offsets 0, 8, 16, 24, 32, 40).
///
/// # Safety
///
/// `frame` must be a valid trap frame captured from a syscall entry.
pub unsafe fn read_syscall_arg(frame: &[u8; 288], i: usize) -> u64 {
    let offset = i * 8;
    unsafe { read_frame_field(frame, offset) }
}

/// Write a syscall return value into the trap frame (x0 at offset 0).
///
/// # Safety
///
/// `frame` must be a valid trap frame.
pub unsafe fn write_retval(frame: &mut [u8; 288], val: u64) {
    unsafe { write_frame_field(frame, 0, val) }
}

/// Read the syscall number from the trap frame (x8 at offset 64).
///
/// # Safety
///
/// `frame` must be a valid trap frame captured from a syscall entry.
pub unsafe fn read_syscall_nr(frame: &[u8; 288]) -> u64 {
    unsafe { read_frame_field(frame, 64) }
}

/// Read the faulting instruction pointer (ELR_EL1 at offset 256).
///
/// # Safety
///
/// `frame` must be a valid trap frame.
pub unsafe fn read_frame_ip(frame: &[u8; 288]) -> u64 {
    unsafe { read_frame_field(frame, 256) }
}

/// Write the faulting instruction pointer (ELR_EL1 at offset 256).
///
/// # Safety
///
/// `frame` must be a valid, writable trap frame.
pub unsafe fn write_frame_ip(frame: &mut [u8; 288], ip: u64) {
    unsafe { write_frame_field(frame, 256, ip) }
}

/// Set initial register values in a trap frame for a new process.
///
/// # Safety
///
/// `frame` must be a valid, writable raw trap frame.
pub unsafe fn set_initial_regs(frame: &mut [u8; 288], entry: u64, sp: u64, _arg: u64) {
    unsafe {
        frame.fill(0);
        // Frame layout matching exception handlers and switch_to_user:
        //   offset 256 = ELR_EL1
        //   offset 264 = SPSR_EL1
        //   offset 248 = SP_EL0
        //   offset 16  = x2 (stack pointer)
        write_frame_field(frame, 256, entry); // ELR_EL1
        write_frame_field(frame, 16, sp); // x2 = stack
        write_frame_field(frame, 264, 0); // SPSR_EL1 = EL0t
        write_frame_field(frame, 248, sp); // SP_EL0 = stack
    }
}

/// Copy a raw trap frame from `src` to `dst`.
///
/// # Safety
///
/// Both must point to valid, non-overlapping frames.
pub unsafe fn copy_frame(dst: &mut [u8; 288], src: &[u8; 288]) {
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr(), 288);
    }
}

pub fn frame_default() -> [u8; 288] {
    [0u8; 288]
}

// Signal-delivery sigframe (SIGNALS.md Phase 4).
//
// AArch64 layout (328 bytes, arch_common::consts::sigframe):
//   [0..288)   saved p_reg (x0@0, x2@16, x30@240, SP_EL0@248,
//              ELR_EL1@256, SPSR_EL1@264)
//   [288..304) signal mask to restore on sigreturn
//   [304..308) signal number
//   [312..320) frame base (scp, for the trampoline)
//   [320..328) SC_MAGIC
// The kernel points ELR_EL1 at the handler, SP/x2 at the frame, x0 at the
// signal number, and x30 (lr) at the trampoline; the handler's `ret` jumps
// to it with sp still at the frame.

/// Size of the signal frame on the target's stack.
pub const fn sigframe_size() -> usize {
    arch_common::consts::sigframe::SIZE
}

/// Compute the frame address below a saved stack pointer (16-aligned).
pub const fn sigframe_addr(old_sp: u64) -> u64 {
    (old_sp - arch_common::consts::sigframe::SIZE as u64) & !0xF
}

/// Read the user stack pointer (SP_EL0@248) from a saved frame.
///
/// # Safety
///
/// `frame` must contain valid saved register state.
pub unsafe fn read_frame_sp(frame: &[u8; 288]) -> u64 {
    unsafe { read_frame_field(frame, 248) }
}

/// Build a signal frame in `dst` from the saved registers.
///
/// # Safety
///
/// `dst` must be `sigframe_size()` bytes; `saved` must be valid register
/// state; `mask` must be 16 bytes.
pub unsafe fn build_sigframe(
    dst: &mut [u8],
    saved: &[u8; 288],
    signo: u32,
    mask: &[u8; 16],
    _trampoline: u64,
    frame_addr: u64,
) {
    use arch_common::consts::sigframe as sf;
    dst[sf::REGS_OFF..sf::REGS_OFF + 288].copy_from_slice(saved);
    dst[sf::MASK_OFF..sf::MASK_OFF + 16].copy_from_slice(mask);
    dst[sf::SIGNAL_OFF..sf::SIGNAL_OFF + 4].copy_from_slice(&signo.to_ne_bytes());
    dst[sf::SC_P_OFF..sf::SC_P_OFF + 8].copy_from_slice(&frame_addr.to_ne_bytes());
    dst[sf::MAGIC_OFF..sf::MAGIC_OFF + 8]
        .copy_from_slice(&arch_common::consts::SC_MAGIC.to_ne_bytes());
}

/// Point a saved frame at the signal handler: ELR_EL1 = handler, x2/SP_EL0
/// = frame, x0 = signo, x30 (lr) = trampoline.
///
/// # Safety
///
/// `p_reg` must point to valid saved register state.
pub unsafe fn sigframe_set_entry(
    p_reg: &mut [u8; 288],
    handler: u64,
    frame: u64,
    signo: u32,
    trampoline: u64,
) {
    unsafe {
        write_frame_field(p_reg, 256, handler); // ELR_EL1
        write_frame_field(p_reg, 16, frame); // x2 = sp
        write_frame_field(p_reg, 248, frame); // SP_EL0
        write_frame_field(p_reg, 240, trampoline); // x30 (lr) — handler returns to it
        write_frame_field(p_reg, 0, signo as u64); // x0 = signo
    }
}

/// Restore a saved frame's registers from a signal frame on sigreturn.
///
/// # Safety
///
/// `frame` must be a valid signal frame (`sigframe_size()` bytes).
pub unsafe fn sigframe_restore(p_reg: &mut [u8; 288], frame: &[u8]) {
    use arch_common::consts::sigframe as sf;
    p_reg.copy_from_slice(&frame[sf::REGS_OFF..sf::REGS_OFF + 288]);
}

/// Initialize architecture-specific process state in the trap frame.
///
/// # Safety
///
/// `frame` must be a valid, writable p_reg buffer.
pub unsafe fn arch_proc_init(
    frame: &mut [u8; 288],
    entry: u64,
    stack: u64,
    _name: &[u8],
    _ps_str: u64,
) {
    frame.fill(0);
    unsafe {
        write_frame_field(frame, 256, entry); // ELR_EL1 = entry
        write_frame_field(frame, 16, stack); // x2 = stack
        write_frame_field(frame, 264, 0); // SPSR_EL1 = EL0t, all masked
        write_frame_field(frame, 248, stack); // SP_EL0 = stack
    }
}

/// Convert a trap frame to a machine context (not implemented).
pub unsafe fn trapframe_to_mcontext(_frame: &[u8; 288]) -> crate::mcontext::Mcontext {
    Mcontext::default()
}

/// Restore a trap frame from a machine context (not implemented).
pub unsafe fn mcontext_to_trapframe(_frame: &mut [u8; 288], _mc: &crate::mcontext::Mcontext) {}

pub const PAGE_SIZE: u64 = 4096;
pub const PAGE_SHIFT: u64 = 12;
pub const ELF_MACHINE: u16 = 183; // EM_AARCH64
pub const FPU_STATE_SIZE: usize = 512;
pub const KERNBASE: u64 = 0xFFFF_0000_0000_0000;

pub type PtEntry = u64;

/// 4-level paging (AArch64 with 4KB granule).
pub const fn pt_levels() -> u32 {
    4
}

/// Extract the page table index at a given level.
/// Level 0 = PT (offset 12), level 1 = PMD (offset 21),
/// level 2 = PUD (offset 30), level 3 = PGD (offset 39).
pub const fn pt_index(va: u64, level: u32) -> usize {
    ((va >> (12 + level * 9)) & 0x1FF) as usize
}

pub const fn pte_present() -> u64 {
    crate::pte::PTE_VALID
}
pub const fn pte_writable() -> u64 {
    // Writable is implicit: AP[2:1] controls access.
    // Use a pseudo-flag; the pagetable.rs uses PG_RW for flags.
    0
}
pub const fn pte_user() -> u64 {
    crate::pte::PTE_AP_EL0_RW
}
pub const fn pte_large_page() -> u64 {
    // Use PTE_AF (Access Flag) to distinguish blocks from tables.
    // Blocks have bits[1:0]=01 with AF=1; tables have bits[1:0]=11 with no AF.
    // This allows map_page to correctly detect and split huge pages.
    crate::pte::PTE_AF
}
pub const fn pte_global() -> u64 {
    0 // nG bit = 0 means global
}
pub const fn pte_frame_mask() -> u64 {
    crate::pte::PTE_ADDR_MASK
}
pub const fn pte_flags_mask() -> u64 {
    crate::pte::PTE_ATTR_MASK
}

pub const fn pte_is_valid_phys(phys: u64) -> bool {
    phys < 0x1_0000_0000
}

/// Flags for a non-leaf (branch) page table entry.
pub const fn pte_nonleaf_flags() -> u64 {
    crate::pte::PTE_TABLE
}

/// Fixed flags for a leaf page descriptor, OR'd into the flags the caller
/// passed to `map_page`. AArch64's L3 page descriptor needs bits[1:0] =
/// 0b11 (VALID|TYPE) plus AF/SH/attr/NG; the generic `map_page` would
/// otherwise emit VALID|AP = 0b01, which is a reserved block descriptor at
/// L3 and faults on access.
pub const fn pte_leaf_flags() -> u64 {
    crate::pte::PTE_TYPE
        | crate::pte::PTE_AF
        | crate::pte::PTE_SH_INNER
        | crate::pte::PTE_ATTR_NORMAL
        | crate::pte::PTE_NG
}

pub const fn pte_split_flags(source_pte: u64, next_level: u32) -> u64 {
    // Preserve attributes except the block/page type bits.
    // Keep AP, SH, AF, AttrIndx — clear type bits and address.
    let attr = source_pte & (crate::pte::PTE_ATTR_MASK & !3);
    if next_level > 0 {
        // Block descriptor at PMD level: bits[1:0] = 0b01.
        // Keeps AF (0x400) so map_page can detect and split further.
        attr | crate::pte::PTE_VALID
    } else {
        // Page descriptor at PTE level: bits[1:0] = 0b11.
        attr | crate::pte::PTE_VALID | crate::pte::PTE_TYPE
    }
}

pub const fn pte_pd_split_exclude_mask() -> u64 {
    crate::pte::PTE_ADDR_MASK
}

pub const fn pte_pd_split_clear_mask() -> u64 {
    0
}

/// Complete set of PTE flags for a user code/data page.
pub const fn pte_user_flags() -> u64 {
    crate::pte::PTE_VALID
        | crate::pte::PTE_TYPE
        | crate::pte::PTE_AF
        | crate::pte::PTE_AP_EL0_RW
        | crate::pte::PTE_SH_INNER
        | crate::pte::PTE_ATTR_NORMAL
        | crate::pte::PTE_NG
}

/// Build a page table entry from a physical address and flags.
/// AArch64: PA stored directly in bits [47:12].
pub const fn build_pte(pa: u64, flags: u64) -> u64 {
    (pa & crate::pte::PTE_ADDR_MASK) | (flags & crate::pte::PTE_ATTR_MASK)
}

/// Extract physical address from a PTE.
pub const fn pte_to_phys(pte: u64) -> u64 {
    pte & crate::pte::PTE_ADDR_MASK
}

/// Decide whether a user leaf PTE at `va` maps a frame owned by the process.
///
/// AArch64 per-process tables split the low-GB 2MB alias blocks into 4KB
/// entries that wrap phys = win_base + ((va - user_low) % win_size) (see
/// `create_low_gb_pmd_table`); the device MMIO window (0x08000000-
/// 0x10000000) is identity-mapped (phys == va). Those shared alias/identity
/// frames must never be freed when a process exits. Real per-process
/// allocations always map at a phys != va and != the alias frame.
pub fn pte_user_owned(pte: u64, va: u64) -> bool {
    const DEV_BASE: u64 = 0x0800_0000;
    const DEV_END: u64 = 0x1000_0000;
    const USER_LOW: u64 = 0x100_0000;
    let user_present = pte_present() | pte_user();
    if pte & user_present != user_present {
        return false;
    }
    let frame = pte_to_phys(pte);
    if frame == va || (va >= DEV_BASE && va < DEV_END) {
        return false; // identity mapping (RAM identity or device MMIO)
    }
    if va >= USER_LOW {
        // Alias frames from the low-GB window (create_low_gb_pmd_table)
        // belong to no process; freeing them would double-free live
        // allocator frames. Real allocations coincide with an alias frame
        // only at the window base (first boot servers), which are never
        // destroyed.
        let win_base = crate::alloc::base();
        let win_size = crate::alloc::total_pages() as u64 * 4096;
        if win_size != 0 && frame == win_base + ((va - USER_LOW) % win_size) {
            return false;
        }
    }
    true
}

pub const fn kern_vaddr() -> u64 {
    0x4000_0000
}

pub const fn user_stack_base() -> u64 {
    // Must be in PUD[0] range. 0x3FC00000 is just below the RAM start
    // (0x40000000), giving maximum space for code and heap below it.
    0x3FC0_0000u64
}

/// Base of the anonymous-mmap search range. Must stay in the user-accessible
/// low 1 GiB (PUD[0]): everything at/above 0x40000000 is the kernel's
/// EL1-only identity map and cannot be mapped for user access.
pub const fn mmap_base() -> u64 {
    0x3000_0000
}

pub const fn user_stack_size() -> usize {
    // 1MB: the current server binaries allocate large stack frames (e.g.
    // pfs_main's inlined init uses ~340KB), which would underflow a 64KB
    // stack. x86/RISC-V tolerate that via permissive low memory mappings;
    // on AArch64 the low-GB alias is only available to exec'd processes, so
    // give every process a stack large enough for the biggest frame.
    0x100_000
}

pub const MAP_PRESENT: u64 = crate::pte::PTE_VALID;
pub const MAP_READ: u64 = 0; // aarch64: no separate read bit; AP bits encode R/W
pub const MAP_WRITE: u64 = 0;
pub const MAP_USER: u64 = crate::pte::PTE_AP_EL0_RW;
pub const MAP_NX: u64 = 0;
pub const MAX_USER_ADDRESS: u64 = 0x0000_0FFF_FFFF_FFFF;

pub fn boot_cr3() -> u64 {
    crate::BOOT_CR3.load(Ordering::Relaxed)
}

/// Write TTBR0_EL1 (AArch64 equivalent of x86 CR3).
///
/// # Safety
///
/// `cr3` must point to a valid, page-aligned root page table (PGD).
/// Write TTBR0_EL1 and flush TLB (MMU already enabled by boot code).
pub unsafe fn write_cr3(cr3: u64) {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "tlbi vmalle1is",
            "dsb ish",
            "ic ialluis",
            "dsb ish",
            "isb",
            "msr ttbr0_el1, {v}",
            "isb",
            v = in(reg) cr3,
            options(nomem, nostack),
        );
    }
}

/// Clean + invalidate the D-cache for an entire 4KB page by VA.
/// Uses DC CIVAC (Clean and Invalidate by VA to PoC) for each
/// cache line, ensuring page table data is visible to the walker.
#[cfg(target_arch = "aarch64")]
pub unsafe fn dcache_clean_invalidate_page(va: u64) {
    let ctr_el0: u64;
    unsafe {
        core::arch::asm!("mrs {v}, ctr_el0", v = out(reg) ctr_el0, options(nomem, nostack));
    }
    let dcache_line_shift = ((ctr_el0 >> 16) & 0xF) + 2;
    let dcache_line_size = 4u64 << dcache_line_shift;

    let mut addr = va;
    let end = va + PAGE_SIZE;
    while addr < end {
        unsafe {
            core::arch::asm!("dc civac, {va}", va = in(reg) addr, options(nostack));
        }
        addr += dcache_line_size;
    }
    unsafe {
        core::arch::asm!("dsb ish", options(nostack));
    }
}

/// Read TTBR0_EL1.
///
/// # Safety
///
/// No special safety requirements.
pub unsafe fn read_cr3() -> u64 {
    let cr3: u64;
    unsafe {
        core::arch::asm!("mrs {cr3}, ttbr0_el1", cr3 = out(reg) cr3, options(nomem, nostack));
    }
    cr3
}

/// Flush the TLB for a single virtual address.
///
/// # Safety
///
/// Must be called after modifying a page table entry.
pub unsafe fn tlb_flush_page(va: u64) {
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi vaae1is, {va}",
            "dsb ish",
            "isb",
            va = in(reg) (va >> 12),
            options(nostack),
        );
    }
}

/// Error returned when a page is not mapped.
#[derive(Debug, Clone, Copy)]
pub struct PageNotMapped;

impl core::fmt::Display for PageNotMapped {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "page not mapped")
    }
}

/// Clear the write bit in a leaf PTE. AArch64 makes this complex
/// because write is controlled by AP bits, not a single flag.
pub unsafe fn clear_rw(_cr3: u64, _va: u64) -> Result<(), PageNotMapped> {
    Err(PageNotMapped)
}

/// Read the page fault address (FAR_EL1).
///
/// # Safety
///
/// Must be called from a page fault handler context.
pub unsafe fn read_fault_addr() -> u64 {
    let addr: u64;
    unsafe {
        core::arch::asm!("mrs {addr}, far_el1", addr = out(reg) addr, options(nomem, nostack));
    }
    addr
}

/// Read the current frame pointer (x29).
pub fn read_frame_pointer() -> u64 {
    let fp: u64;
    unsafe {
        core::arch::asm!("mov {fp}, x29", fp = out(reg) fp, options(nomem, nostack));
    }
    fp
}

/// Return the current CPU ID.
/// On QEMU virt with single CPU, always 0.
pub fn cpu_id() -> u32 {
    let mpidr: u64;
    unsafe {
        core::arch::asm!("mrs {mpidr}, mpidr_el1", mpidr = out(reg) mpidr, options(nomem, nostack));
    }
    (mpidr & 0xFF) as u32
}

/// Allocate a physical page.
pub unsafe fn alloc_phys_page() -> Option<u64> {
    crate::alloc::alloc_phys_page()
}

/// Allocate contiguous physical pages.
pub unsafe fn alloc_phys_contig(count: usize) -> Option<u64> {
    crate::alloc::alloc_phys_contig(count)
}

/// Base of the physical allocator's free-RAM window (just past the kernel
/// image). The per-process kernel identity PMD table wraps within this
/// window, so boot uses it to bound the identity map to real RAM.
pub fn phys_alloc_base() -> u64 {
    crate::alloc::base()
}

/// Total pages in the physical allocator's free-RAM window.
pub fn phys_alloc_total_pages() -> usize {
    crate::alloc::total_pages()
}

/// Free contiguous physical pages.
pub unsafe fn free_phys_contig(addr: u64, count: usize) {
    unsafe { crate::alloc::free_phys_contig(addr, count) }
}

/// Initialize the physical page allocator.
///
/// No-op on AArch64: the bitmap allocator places its bitmap at the top of
/// the free range (0x4FFFE000 for the 256MB QEMU virt RAM). A server
/// process calling this (VM's vm_main passes the same RAM range) computes
/// the same physical bitmap address, and its per-process page tables
/// identity-map all of RAM as EL0_RW — so the write would reset the
/// kernel's allocation records. Subsequent allocations (e.g. exec) would
/// then reuse the boot page-table pages and corrupt the boot table.
/// Servers never allocate via their kernel-crate allocator copies, so there
/// is nothing to initialize here.
pub unsafe fn init_phys_alloc(_base: u64, _size: u64) {}

pub const fn has_port_io() -> bool {
    false
}

pub const fn fork_needs_child_flag_clear() -> bool {
    // AArch64: after fork, the child inherits REPLY_PEND + RECEIVING
    // from the parent's SENDREC. PM's SENDNB to the child is rejected
    // by will_receive_sendrec because REPLY_PEND's p_getfrom_e != PM.
    true
}

pub unsafe fn inb(_port: u16) -> u8 {
    0xFF
}
pub unsafe fn outb(_port: u16, _val: u8) {}
pub unsafe fn inw(_port: u16) -> u16 {
    0xFFFF
}
pub unsafe fn outw(_port: u16, _val: u16) {}
pub unsafe fn inl(_port: u16) -> u32 {
    0xFFFF_FFFF
}
pub unsafe fn outl(_port: u16, _val: u32) {}
pub unsafe fn phys_insb(_port: u16, _addr: u64, _count: usize) {}
pub unsafe fn phys_outsb(_port: u16, _addr: u64, _count: usize) {}
pub unsafe fn phys_insw(_port: u16, _addr: u64, _count: usize) {}
pub unsafe fn phys_outsw(_port: u16, _addr: u64, _count: usize) {}

pub const PCI_ADDR_PORT: u16 = 0;
pub const PCI_DATA_PORT: u16 = 0;
pub const RTC_INDEX: u16 = 0;

pub fn pci_config_addr(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u32 {
    0
}
pub unsafe fn pci_cfg_read8(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u8 {
    0xFF
}
pub unsafe fn pci_cfg_read16(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u16 {
    0xFFFF
}
pub unsafe fn pci_cfg_read32(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u32 {
    0xFFFF_FFFF
}
pub unsafe fn pci_cfg_write32(_bus: u8, _dev: u8, _func: u8, _reg: u8, _val: u32) {}
pub unsafe fn cmos_read(_reg: u8) -> u8 {
    0
}
pub unsafe fn cmos_write(_reg: u8, _val: u8) {}
pub unsafe fn mfence() {
    unsafe {
        core::arch::asm!("dmb sy", options(nomem, nostack));
    }
}

pub unsafe fn init_profile_clock(_rate_code: u32, _callback: unsafe extern "C" fn()) {}
pub fn stop_profile_clock() {}

#[allow(non_upper_case_globals)]
pub static __bss_start: u8 = 0;
#[allow(non_upper_case_globals)]
pub static __bss_end: u8 = 0;

pub fn bss_start() -> u64 {
    unsafe extern "C" {
        static __bss_start: u8;
    }
    core::ptr::addr_of!(__bss_start) as u64
}

pub fn bss_end() -> u64 {
    unsafe extern "C" {
        static __bss_end: u8;
    }
    core::ptr::addr_of!(__bss_end) as u64
}

/// Exit QEMU via PSCI SYSTEM_OFF (HVC conduit).
///
/// QEMU's virt machine routes `hvc` to PSCI and exits the emulator with
/// status 0 on SYSTEM_OFF (there is no exit-code device and semihosting
/// via `hvc #0xf000` is not intercepted in this QEMU build). Callers that
/// need pass/fail detection must check the serial log for result markers.
pub fn qemu_exit(code: u32) -> ! {
    let _ = code;
    unsafe {
        core::arch::asm!(
            "movz x0, #0x0008",
            "movk x0, #0x8400, lsl #16", // PSCI_SYSTEM_OFF
            "hvc #0",
            options(nomem, nostack),
        );
    }
    // SYSTEM_OFF never returns; if PSCI is unavailable, park the CPU.
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

/// Deep-copy the parent's page table for fork. See `crate::fork::vm_paging_fork`.
pub use crate::fork::vm_paging_fork;

/// Build the PUD[0] PMD table used by per-process page tables: the low
/// 1GB is a user-accessible RAM alias (tolerates stack underflow below the
/// 64KB user stack at 0x3FC00000), except the device MMIO window (GIC at
/// 0x08000000, PL011 UART at 0x09000000) which is identity-mapped so kernel
/// device access keeps working while this process's page table is loaded.
/// map_page() later splits the user code/stack pages out of this table.
///
/// The alias window maps onto *free* RAM above the kernel image (the
/// physical allocator's range, which boot sets to start just past the
/// kernel), never onto the kernel image itself: the first 16 MiB of the low
/// GB (the NULL page and the gap below the image base 0x1000000) is left
/// unmapped, and every other VA wraps within the free-RAM window. This keeps
/// a stray user (or kernel copy) write to a low user VA from corrupting
/// kernel text at PA 0x40000000.
///
/// # Safety
///
/// Caller must be in a context where physical allocation is allowed.
pub unsafe fn create_low_gb_pmd_table() -> Option<u64> {
    unsafe {
        let pmd_low = alloc_phys_page()?;
        const PMD_BLOCK: u64 = 0b01u64 | (0b01u64 << 6) | (0b11u64 << 8) | (1u64 << 10); // 0x741
        let dev_base: u64 = 0x0800_0000;
        let dev_end: u64 = 0x1000_0000;
        // User binaries load at VA 0x1000000; VAs below that (the NULL page
        // and the gap under the image) are unmapped so accesses fault.
        let user_low: u64 = 0x100_0000;
        // Free RAM starts just past the kernel image (the allocator base);
        // the low-GB alias wraps within it. Cap the window at the
        // *usable* (bitmap-excluded) region, rounded down to 2 MiB so every
        // alias block lands wholly inside it: at large RAM sizes the
        // un-capped window's top blocks wrapped onto the allocator bitmap,
        // and splitting such a block (exec maps the brk at 0x3FE00000,
        // which at 1 GiB sits in the wrap tail) exposed the bitmap as
        // user-writable alias leaves — corrupting allocation state.
        let win_base: u64 = crate::alloc::base();
        let win_size: u64 = (crate::alloc::usable_size() / 0x20_0000) * 0x20_0000;
        for i in 0..512usize {
            let va = (i as u64) * 0x20_0000;
            let pa = if va >= dev_base && va < dev_end {
                va // identity: device MMIO
            } else if va < user_low || win_size == 0 {
                0 // unmapped: NULL page + gap below the image
            } else {
                win_base + ((va - user_low) % win_size)
            };
            core::ptr::write_volatile((pmd_low as *mut u64).add(i), pa | PMD_BLOCK);
        }
        Some(pmd_low)
    }
}

pub unsafe fn exec_create_root(boot_cr3: u64) -> u64 {
    // Create a new PGD + private PUD, copying boot entries.
    // PUD[1] copied as 1GB BLOCK (AP=EL1_only) from boot page table.
    // PUD[0] pre-filled with AP=EL0_RW 2MB PMD entries so user
    // processes can access RAM through the low-GB alias.
    unsafe {
        let new_pgd = match alloc_phys_page() {
            Some(p) => p,
            None => return 0,
        };
        for i in 0..512 {
            core::ptr::write_volatile((new_pgd as *mut u64).add(i), 0);
        }

        let private_pud = match alloc_phys_page() {
            Some(p) => p,
            None => return 0,
        };
        for i in 0..512 {
            core::ptr::write_volatile((private_pud as *mut u64).add(i), 0);
        }

        // Copy boot PUD entries: PUD[1] = 1GB BLOCK for kernel identity.
        let boot_pgd = boot_cr3 as *const u64;
        let boot_pgd0 = core::ptr::read(boot_pgd);
        let boot_pud_phys = pte_to_phys(boot_pgd0);
        let kern_entry = core::ptr::read((boot_pud_phys as *const u64).add(1));
        core::ptr::write((private_pud as *mut u64).add(1), kern_entry);

        // PUD[0] = user-accessible low-GB PMD table (see create_low_gb_pmd_table).
        let pmd_low = match create_low_gb_pmd_table() {
            Some(p) => p,
            None => return 0,
        };
        let pud0_table = crate::pte::make_pte(pmd_low, pte_nonleaf_flags());
        core::ptr::write((private_pud as *mut u64).add(0), pud0_table);

        // PGD[0] → private PUD.
        let pgd0_entry = crate::pte::make_pte(private_pud, pte_nonleaf_flags());
        core::ptr::write(new_pgd as *mut u64, pgd0_entry);

        new_pgd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_default_is_zeroed() {
        let f = frame_default();
        assert_eq!(f[0], 0);
        assert_eq!(f[287], 0);
    }

    #[test]
    fn test_read_write_frame_field_roundtrip() {
        let mut f = frame_default();
        unsafe {
            write_frame_field(&mut f, 0, 0xDEADBEEF_CAFEBABE);
        }
        let val = unsafe { read_frame_field(&f, 0) };
        assert_eq!(val, 0xDEADBEEF_CAFEBABE);
    }

    #[test]
    fn test_pt_levels() {
        assert_eq!(pt_levels(), 4);
    }

    #[test]
    fn test_pt_index() {
        assert_eq!(pt_index(0, 0), 0);
        assert_eq!(pt_index(0x1000, 0), 1);
        assert_eq!(pt_index(0x200000, 1), 1);
        assert_eq!(pt_index(0x40000000, 2), 1);
    }
}
