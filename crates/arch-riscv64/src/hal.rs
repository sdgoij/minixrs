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

// Re-export arch-specific types for kernel use.
pub use crate::frame::TrapFrame;
pub use crate::mcontext::Mcontext;

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

/// Non-blocking poll: returns a byte if available from any console source
/// (SBI debug console or MMIO 8250 UART).
pub fn poll_console() -> Option<u8> {
    if let Some(byte) = crate::sbi::console_getchar() {
        return Some(byte);
    }
    if serial_byte_available() {
        unsafe { Some(core::ptr::read_volatile(0x10000000usize as *const u8)) }
    } else {
        None
    }
}

/// Arch-specific CPU idle hint (wfi with interrupts enabled on RISC-V).
pub fn cpu_idle() {
    unsafe {
        core::arch::asm!(
            "csrsi sstatus, 2",
            "wfi",
            "csrci sstatus, 2",
            options(nomem, nostack)
        );
    }
}

pub fn read_cycles() -> u64 {
    // RISC-V has no rdtsc equivalent; the `time` CSR (read via `rdtime`)
    // is the free-running counter that the clock code uses for CPU-time
    // accounting (ms_2_cpu_time converts against cpu_freq, the CLINT
    // timebase). The privileged `cycle` CSR is not guaranteed readable in
    // S-mode, so use `rdtime` like clint::read_time does.
    let time: u64;
    unsafe {
        core::arch::asm!("rdtime {time}", time = out(reg) time, options(nomem, nostack));
    }
    time
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

/// Set up register frame for a new process via exec(2).
/// Writes entry point (sepc), stack pointer (sp), argc (a0),
/// argv (a1), and sstatus.
///
/// # Safety
///
/// `frame` must be a valid, writable `[u8; 256]` trap frame.
pub unsafe fn exec_init_regs(frame: &mut [u8; 256], entry: u64, sp: u64, argc: u64, argv: u64) {
    unsafe {
        write_frame_field(frame, 0, entry); // sepc in x0 slot
        write_frame_field(frame, 16, sp); // sp (x2)
        write_frame_field(frame, 80, argc); // a0 (x10)
        write_frame_field(frame, 88, argv); // a1 (x11)
        // sstatus = SPIE | FS_INITIAL (SIE=0, SPIE=1, SPP=0)
        write_frame_field(frame, 248, 0x00000220);
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }
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
pub unsafe fn set_initial_regs(frame: &mut [u8; 256], entry: u64, sp: u64, arg: u64) {
    // RISC-V: set up initial register state for new process.
    // sepc = entry (stored at offset 0 = x0 slot, never loaded as GPR)
    // sp = stack pointer (x2 at offset 16)
    // a0 = arg (x10 at offset 80): the spawned-thread entry (thread_start)
    //     receives its ThreadInit box pointer here. Exec/boot entries read
    //     argc/argv from the stack and ignore a0.
    // sstatus = SPIE | FS_INITIAL (SIE=0, SPIE=1, SPP=0, FS=initial)
    // SIE=0 is CRITICAL: prevents supervisor interrupts from firing between
    // `csrw sstatus` and `sret` in switch_to_user.
    unsafe {
        write_frame_field(frame, 0, entry); // sepc in x0 slot
        write_frame_field(frame, 16, sp); // sp (x2 at offset 16)
        write_frame_field(frame, 80, arg); // a0 (x10) = arg
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

// Signal-delivery sigframe (SIGNALS.md Phase 4).
//
// RISC-V layout (296 bytes, arch_common::consts::sigframe):
//   [0..256)   saved p_reg (sepc@0, ra@8, sp@16, a0@80, sstatus@248)
//   [256..272) signal mask to restore on sigreturn
//   [272..276) signal number
//   [280..288) frame base (scp, for the trampoline)
//   [288..296) SC_MAGIC
// The kernel points sepc at the handler, sp at the frame, a0 at the signal
// number, and ra at the trampoline; the handler's `ret` jumps to it with sp
// still at the frame.

/// Size of the signal frame on the target's stack.
pub const fn sigframe_size() -> usize {
    arch_common::consts::sigframe::SIZE
}

/// Compute the frame address below a saved stack pointer (16-aligned).
pub const fn sigframe_addr(old_sp: u64) -> u64 {
    (old_sp - arch_common::consts::sigframe::SIZE as u64) & !0xF
}

/// Read the user stack pointer (sp, x2@16) from a saved frame.
///
/// # Safety
///
/// `frame` must contain valid saved register state.
pub unsafe fn read_frame_sp(frame: &[u8; 256]) -> u64 {
    unsafe { read_frame_field(frame, 16) }
}

/// Build a signal frame in `dst` from the saved registers.
///
/// # Safety
///
/// `dst` must be `sigframe_size()` bytes; `saved` must be valid register
/// state; `mask` must be 16 bytes.
pub unsafe fn build_sigframe(
    dst: &mut [u8],
    saved: &[u8; 256],
    signo: u32,
    mask: &[u8; 16],
    _trampoline: u64,
    frame_addr: u64,
) {
    use arch_common::consts::sigframe as sf;
    dst[sf::REGS_OFF..sf::REGS_OFF + 256].copy_from_slice(saved);
    dst[sf::MASK_OFF..sf::MASK_OFF + 16].copy_from_slice(mask);
    dst[sf::SIGNAL_OFF..sf::SIGNAL_OFF + 4].copy_from_slice(&signo.to_ne_bytes());
    dst[sf::SC_P_OFF..sf::SC_P_OFF + 8].copy_from_slice(&frame_addr.to_ne_bytes());
    dst[sf::MAGIC_OFF..sf::MAGIC_OFF + 8]
        .copy_from_slice(&arch_common::consts::SC_MAGIC.to_ne_bytes());
}

/// Point a saved frame at the signal handler: sepc = handler, sp = frame,
/// a0 = signo, ra = trampoline.
///
/// # Safety
///
/// `p_reg` must point to valid saved register state.
pub unsafe fn sigframe_set_entry(
    p_reg: &mut [u8; 256],
    handler: u64,
    frame: u64,
    signo: u32,
    trampoline: u64,
) {
    unsafe {
        write_frame_field(p_reg, 0, handler); // sepc
        write_frame_field(p_reg, 16, frame); // sp
        write_frame_field(p_reg, 8, trampoline); // ra — handler returns to it
        write_frame_field(p_reg, 80, signo as u64); // a0 = signo
    }
}

/// Restore a saved frame's registers from a signal frame on sigreturn.
///
/// # Safety
///
/// `frame` must be a valid signal frame (`sigframe_size()` bytes).
pub unsafe fn sigframe_restore(p_reg: &mut [u8; 256], frame: &[u8]) {
    use arch_common::consts::sigframe as sf;
    p_reg.copy_from_slice(&frame[sf::REGS_OFF..sf::REGS_OFF + 256]);
}

/// Initialize architecture-specific process state in the trap frame.
///
/// Called from `do_exec_handler` (PM exec path) to set up a new process's
/// initial register state before its first schedule.  On RISC-V, sets:
/// - sepc (offset 0) = entry point
/// - sp   (offset 16) = stack pointer
/// - sstatus (offset 248) = SPIE | FS_INITIAL (user mode, interrupts on)
///
/// # Safety
///
/// `frame` must be a valid, writable p_reg buffer (256 bytes).
pub unsafe fn arch_proc_init(
    frame: &mut [u8; 256],
    entry: u64,
    stack: u64,
    _name: &[u8],
    _ps_str: u64,
) {
    // Clear the entire frame for a clean start.
    frame.fill(0);
    unsafe {
        write_frame_field(frame, 0, entry); // sepc = entry point
        write_frame_field(frame, 16, stack); // sp = user stack
        write_frame_field(
            frame,
            248,
            crate::psl::sstatus::SPIE | crate::psl::sstatus::FS_INITIAL,
        ); // sstatus: SIE=0, SPIE=1, SPP=0
    }
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

/// ELF machine identifier for this architecture (e_machine field).
pub const ELF_MACHINE: u16 = 243; // EM_RISCV
/// Size of FPU save area.
pub const FPU_STATE_SIZE: usize = 256;

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

/// Validate a physical address is within the identity-mapped range.
pub const fn pte_is_valid_phys(phys: u64) -> bool {
    phys < 0x8_0000_0000
}

/// Flags for a non-leaf (branch) page table entry.
pub const fn pte_nonleaf_flags() -> u64 {
    pte_present()
}

/// Fixed leaf flags for `map_page` (none on x86: PG_P is added by the
/// caller and PG_RW/PG_U come in the flags).
pub const fn pte_leaf_flags() -> u64 {
    0
}

/// Extract permission flags from a huge-page PTE when splitting into
/// sub-entries. RISC-V preserves all flags except the frame mask.
pub const fn pte_split_flags(source_pte: u64, _next_level: u32) -> u64 {
    (source_pte & pte_flags_mask()) & !pte_frame_mask()
}

/// Mask of flags to exclude when extracting attributes from a PDE
/// being split into 4KB entries (pt_mapkernel path).
pub const fn pte_pd_split_exclude_mask() -> u64 {
    pte_frame_mask() | pte_global()
}

/// Mask of flags to clear on the replacement PDE after splitting
/// (pt_mapkernel path). On RISC-V, only G must be cleared.
pub const fn pte_pd_split_clear_mask() -> u64 {
    pte_global()
}

/// Complete set of PTE flags for a user code/data page (exec mapping).
pub const fn pte_user_flags() -> u64 {
    pte::PTE_V | pte::PTE_R | pte::PTE_W | pte::PTE_X | pte::PTE_U | pte::PTE_A | pte::PTE_D
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

/// Decide whether a user leaf PTE at `va` maps a frame owned by the process.
///
/// RISC-V boot processes map an identity guard region below the user stack
/// (phys == va, PTE_U set); those shared frames must never be freed when a
/// process exits. Real per-process allocations always map at a phys != va.
pub const fn pte_user_owned(pte: u64, va: u64) -> bool {
    let user_present = pte_present() | pte_user();
    (pte & user_present) == user_present && pte_to_phys(pte) != va
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
    // 1MB: server binaries allocate large stack frames (e.g. pfs_main's
    // inlined init uses ~340KB) that underflow a 64KB stack into the
    // identity-mapped RAM below it. That region only exists when RAM is
    // large enough (stack VA 0x8FE00000 sits at ~2.3 GiB), so give every
    // process a stack large enough for the biggest frame — same rationale
    // as the AArch64 HAL.
    0x100_000
}

/// Base of the anonymous-mmap search range, above the brk heap
/// (0x3FE00000..0x3FF00000) and below the kernel at 0x80200000.
pub const fn mmap_base() -> u64 {
    0x4000_0000
}

pub const MAP_PRESENT: u64 = pte::PTE_V;
pub const MAP_READ: u64 = pte::PTE_R;
// SV39 requires W to imply R: a leaf with W=1,R=0 is reserved and faults
// on access, so the VM-facing "writable" flag carries both bits.
pub const MAP_WRITE: u64 = pte::PTE_R | pte::PTE_W;
pub const MAP_USER: u64 = pte::PTE_U;
pub const MAP_NX: u64 = 0; // RISC-V: NX is absence of X bit
// SV39 user space is 2^38 bytes: bit 38 must be clear in U-mode, so the
// first non-user address is 0x4000000000 (exclusive bound, like x86's
// 0x800000000000). The old value (0x3FFFFFFFFFFF) admitted non-canonical
// SV39 addresses, which the kernel then happily "mapped" — but the CPU
// faults on access.
pub const MAX_USER_ADDRESS: u64 = 0x40_0000_0000;

pub fn boot_cr3() -> u64 {
    crate::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed)
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
/// Actually reads the current SATP CSR value, NOT boot_cr3().
/// Converts the SATP PPN field back to a physical address
/// (matching the format expected by write_cr3 and p_seg.p_cr3).
/// This is critical for delivermsg() and other kernel functions
/// that need to save/restore the current page table.
///
/// # Safety
///
/// No special safety requirements; the SATP CSR is always readable.
pub unsafe fn read_cr3() -> u64 {
    let satp: u64;
    unsafe {
        core::arch::asm!("csrr {}, satp", out(reg) satp, options(nomem, nostack));
    }
    // SATP format: MODE (bits 60-63) | ASID (bits 44-59) | PPN (bits 0-43)
    // PPN is the page number (4KB pages). Convert back to physical address
    // by shifting left by PAGE_SHIFT (12), matching the format write_cr3
    // expects (physical address of root page table).
    let ppn = satp & 0x00000FFFFFFFFFFF;
    ppn << 12
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

/// Error returned when a page is not mapped.
#[derive(Debug, Clone, Copy)]
pub struct PageNotMapped;

impl core::fmt::Display for PageNotMapped {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "page not mapped")
    }
}

/// Clear the write bit in a leaf PTE (not yet implemented on RISC-V).
///
/// # Safety
///
/// `cr3` must point to a valid page table.
pub unsafe fn clear_rw(_cr3: u64, _va: u64) -> Result<(), PageNotMapped> {
    Err(PageNotMapped)
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

/// Free `count` contiguous physical pages starting at `addr`.
///
/// # Safety
///
/// Must be called after the physical memory allocator is initialized.
/// `addr` must have been previously allocated via `alloc_phys_contig`.
pub unsafe fn free_phys_contig(addr: u64, count: usize) {
    unsafe { crate::alloc::free_phys_contig(addr, count) }
}

/// Whether port I/O is available on this architecture.
pub const fn has_port_io() -> bool {
    false
}

/// Whether fork(2) needs explicit RECEIVING/REPLY_PEND clearing on the child.
pub const fn fork_needs_child_flag_clear() -> bool {
    true
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

/// PCI configuration address port (unused on RISC-V).
pub const PCI_ADDR_PORT: u16 = 0xCF8;
/// PCI configuration data port (unused on RISC-V).
pub const PCI_DATA_PORT: u16 = 0xCFC;
/// RTC CMOS index port (unused on RISC-V).
pub const RTC_INDEX: u16 = 0x70;

/// Build a PCI config address (same encoding on all arches).
#[inline]
pub fn pci_config_addr(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | (reg as u32 & 0xFC)
}

/// Read 8 bits from PCI config space (stub — returns 0xFF on RISC-V).
///
/// # Safety
///
/// Stub: always safe to call but returns a sentinel value.
pub unsafe fn pci_cfg_read8(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u8 {
    0xFF
}

/// Read 16 bits from PCI config space (stub — returns 0xFFFF on RISC-V).
///
/// # Safety
///
/// Stub: always safe to call but returns a sentinel value.
pub unsafe fn pci_cfg_read16(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u16 {
    0xFFFF
}

/// Read 32 bits from PCI config space (stub — returns 0xFFFF_FFFF on RISC-V).
///
/// # Safety
///
/// Stub: always safe to call but returns a sentinel value.
pub unsafe fn pci_cfg_read32(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u32 {
    0xFFFF_FFFF
}

/// Write 32 bits to PCI config space (stub — no-op on RISC-V).
///
/// # Safety
///
/// Stub: always safe to call; PCI config is not accessible.
pub unsafe fn pci_cfg_write32(_bus: u8, _dev: u8, _func: u8, _reg: u8, _val: u32) {}

/// Read a CMOS register value (stub — returns 0 on RISC-V).
///
/// # Safety
///
/// Stub: always safe to call; CMOS is not accessible.
pub unsafe fn cmos_read(_reg: u8) -> u8 {
    0
}

/// Write a value to a CMOS register (stub — no-op on RISC-V).
///
/// # Safety
///
/// Stub: always safe to call; CMOS is not accessible.
pub unsafe fn cmos_write(_reg: u8, _val: u8) {}

/// Full memory fence (compiler fence on RISC-V).
///
/// # Safety
///
/// Issues a SeqCst compiler fence; sufficient for single-hart use.
pub unsafe fn mfence() {
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
}

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
    all(target_os = "minix", not(target_arch = "riscv64"))
))]
#[used]
#[unsafe(no_mangle)]
pub static __bss_start: u8 = 0;
#[cfg(any(
    target_os = "windows",
    all(target_os = "minix", not(target_arch = "riscv64"))
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
    crate::cpulocals::bill_proc() as *mut core::ffi::c_void
}

/// Set the billable process pointer.
///
/// # Safety
///
/// Must be called from a scheduler context where the pointer is valid.
pub unsafe fn sched_set_bill_proc(proc: *mut core::ffi::c_void) {
    unsafe { crate::cpulocals::set_bill_proc(proc as u64) }
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

/// Read the timestamp counter.
pub fn read_tsc() -> u64 {
    crate::clint::read_time()
}

/// Read the per-CPU TSC context-switch timestamp.
///
/// Returns 0 on RISC-V to make context_stop a no-op for quantum
/// accounting. Preemptive scheduling is handled cooperatively
/// in the post-syscall hook.
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

/// Set the calling thread's thread pointer. On RISC-V the thread pointer is
/// the `tp` (x4) general register, saved/restored with the register frame;
/// the thread library sets it directly in user mode, so this is a no-op.
///
/// # Safety
///
/// No-op.
pub unsafe fn set_tls_current(_tls: u64) {}

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

/// Exit QEMU via sifive_test device (MMIO 0x100000 on virt machine).
/// 0x5555 = pass (exit code 0), 0x3333 = fail (exit code 1).
///
/// Falls back to SBI SRST shutdown if no test-exit device is present
/// (QEMU builds without `-device test-exit` treat the 0x100000 write as
/// plain RAM). The exit code is then lost, so callers that need pass/fail
/// detection must check the serial log for result markers.
pub fn qemu_exit(code: u32) -> ! {
    unsafe {
        let val = if code == 0 { 0x5555u32 } else { 0x3333u32 };
        core::ptr::write_volatile(0x100000 as *mut u32, val);
    }
    crate::sbi::system_reset(true)
}

/// Deep-copy user page table entries from parent to child for fork.
/// Walks SV39 3-level page tables (L2 → L1 → L0).
/// Returns 0 on success, -12 (ENOMEM) on allocation failure.
///
/// # Safety
///
/// `parent_cr3` and `child_cr3` must point to valid page tables.
/// `child_cr3` must be a freshly-allocated zero-filled page.
pub unsafe fn vm_paging_fork(parent_cr3: u64, child_cr3: u64, _msg: &mut [u8; 64]) -> i32 {
    const V: u64 = 0x001;
    const R: u64 = 0x002;
    const W: u64 = 0x004;
    const X: u64 = 0x008;
    const U: u64 = 0x010;
    const PPN_MASK: u64 = 0x003FFFFFFFFFFC00;

    unsafe {
        let parent = parent_cr3 as *const u64;
        let child_root = child_cr3 as *mut u64;

        // Phase 1: Copy the parent's root page table (L2) to the child.
        core::ptr::copy_nonoverlapping(parent, child_root, 512);

        // Phase 2: Deep-copy all intermediate L1 and L0 page table pages.
        for l2 in 0..512 {
            let e2 = core::ptr::read(parent.add(l2));
            if e2 & V == 0 {
                continue;
            }
            let l2_leaf = (e2 & (R | W | X)) != 0;
            if !l2_leaf {
                let parent_l1_pa = pte_to_phys(e2);
                let parent_l1 = parent_l1_pa as *const u64;
                let child_l1_pa = match alloc_phys_page() {
                    Some(p) => p,
                    None => return -12,
                };
                let child_l1 = child_l1_pa as *mut u64;
                core::ptr::copy_nonoverlapping(parent_l1, child_l1, 512);
                let l2_flags = e2 & !PPN_MASK;
                core::ptr::write(child_root.add(l2), build_pte(child_l1_pa, l2_flags));

                for l1 in 0..512 {
                    let e1 = core::ptr::read(child_l1.add(l1));
                    if e1 & V == 0 {
                        continue;
                    }
                    let l1_leaf = (e1 & (R | W | X)) != 0;
                    if !l1_leaf {
                        let parent_l0_pa = pte_to_phys(e1);
                        let parent_l0 = parent_l0_pa as *const u64;
                        let child_l0_pa = match alloc_phys_page() {
                            Some(p) => p,
                            None => return -12,
                        };
                        let child_l0 = child_l0_pa as *mut u64;
                        core::ptr::copy_nonoverlapping(parent_l0, child_l0, 512);
                        let l1_flags = e1 & !PPN_MASK;
                        core::ptr::write(child_l1.add(l1), build_pte(child_l0_pa, l1_flags));
                    }
                }
            }
        }

        // Phase 3: Walk parent's page table hierarchy and deep-copy each
        // user leaf page via direct PTE writes into the child's page table.
        for l2 in 0..512 {
            let e2 = core::ptr::read(parent.add(l2));
            if e2 & V == 0 {
                continue;
            }
            let l2_leaf = (e2 & (R | W | X)) != 0;
            if l2_leaf {
                if e2 & U != 0 {
                    let src_1gb = pte_to_phys(e2);
                    let l1_pa = match alloc_phys_page() {
                        Some(p) => p,
                        None => return -12,
                    };
                    let l1 = l1_pa as *mut u64;
                    let is_writable = (e2 & W) != 0;
                    for l1_idx in 0..512 {
                        let pa_2mb = src_1gb + (l1_idx as u64) * 0x200000;
                        let flags = if is_writable {
                            (e2 & !(PPN_MASK | W)) | V
                        } else {
                            e2 & !PPN_MASK
                        };
                        core::ptr::write(l1.add(l1_idx), build_pte(pa_2mb, flags));
                    }
                    let l2_branch = build_pte(l1_pa, V);
                    core::ptr::write(parent.add(l2) as *mut u64, l2_branch);
                    core::ptr::write(child_root.add(l2), l2_branch);
                } else {
                    core::ptr::write(child_root.add(l2), e2);
                }
                continue;
            }
            let parent_l1_pa = pte_to_phys(e2);
            let parent_l1 = parent_l1_pa as *const u64;

            for l1 in 0..512 {
                let e1 = core::ptr::read(parent_l1.add(l1));
                if e1 & V == 0 {
                    continue;
                }
                let l1_leaf = (e1 & (R | W | X)) != 0;

                if l1_leaf {
                    if e1 & U != 0 && e1 & W != 0 {
                        let cow = e1 & !W;
                        let child_l2e = core::ptr::read((child_cr3 as *const u64).add(l2));
                        let child_l1_pa = pte_to_phys(child_l2e);
                        if child_l1_pa != 0 {
                            core::ptr::write((child_l1_pa as *mut u64).add(l1), cow);
                        }
                    }
                    continue;
                }
                let parent_l0_pa = pte_to_phys(e1);
                let parent_l0 = parent_l0_pa as *const u64;

                for l0 in 0..512 {
                    let e0 = core::ptr::read(parent_l0.add(l0));
                    if e0 & V == 0 {
                        continue;
                    }
                    let pa = pte_to_phys(e0);
                    if e0 & U == 0 || e0 & W == 0 {
                        // Kernel page or already read-only: share directly.
                        let child_l2e = core::ptr::read((child_cr3 as *const u64).add(l2));
                        let child_l1_pa = pte_to_phys(child_l2e);
                        let child_l1e = core::ptr::read((child_l1_pa as *const u64).add(l1));
                        let child_l0_pa = pte_to_phys(child_l1e);
                        let child_l0_ptr = (child_l0_pa as *mut u64).add(l0);
                        core::ptr::write(child_l0_ptr, build_pte(pa, e0 & !PPN_MASK));
                        continue;
                    }
                    // User writable 4KB page — COW in child only.
                    let cow_e0 = e0 & !W;
                    let child_l2e = core::ptr::read((child_cr3 as *const u64).add(l2));
                    let child_l1_pa = pte_to_phys(child_l2e);
                    let child_l1e = core::ptr::read((child_l1_pa as *const u64).add(l1));
                    let child_l0_pa = pte_to_phys(child_l1e);
                    let child_l0_ptr = (child_l0_pa as *mut u64).add(l0);
                    core::ptr::write(child_l0_ptr, build_pte(pa, cow_e0 & !PPN_MASK));
                }
            }
        }

        // Restore kernel identity map entries in the child's page table
        // after COW splitting may have removed them.
        let boot_cr3 = crate::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
        if boot_cr3 != 0 {
            let boot = boot_cr3 as *const u64;
            let cr = child_cr3 as *mut u64;
            for i in 0..4 {
                let child_entry = core::ptr::read(cr.add(i));
                if child_entry & V == 0 {
                    let boot_entry = core::ptr::read(boot.add(i));
                    if boot_entry != 0 {
                        core::ptr::write(cr.add(i), boot_entry);
                    }
                }
            }
        }

        0
    }
}

/// Create the initial page table root for a new process via exec(2).
/// Allocates an L2 page, copies kernel identity-map entries from the
/// boot page table, and returns the L2 physical address.
/// Returns 0 on allocation failure.
///
/// # Safety
///
/// `boot_cr3` must point to a valid boot page table.
pub unsafe fn exec_create_root(boot_cr3: u64) -> u64 {
    unsafe {
        let new_root = match alloc_phys_page() {
            Some(p) => p,
            None => return 0,
        };
        core::ptr::write_bytes(new_root as *mut u8, 0, PAGE_SIZE as usize);
        let boot_root = boot_cr3 as *const u64;
        // Copy the full identity map (0..32 GiB, one 1 GiB block per L2
        // entry); the entries are supervisor-only, matching the boot table.
        for i in 0usize..32 {
            let e = core::ptr::read(boot_root.add(i));
            core::ptr::write((new_root as *mut u64).add(i), e);
        }
        new_root
    }
}

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
