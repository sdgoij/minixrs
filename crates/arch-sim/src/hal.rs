//! The kernel's HAL surface, implemented with host primitives.
//!
//! Grouped the way `ARCH_WASM32.md` §8 groups the x86_64 port, so the two can be
//! read side by side. Where a function is inert, the doc comment says why rather
//! than leaving it to the reader.
//!
//! # Not exercisable here
//!
//! Anything that depends on real address translation. `boot_cr3` returns 0 and
//! `pt_levels` returns 0, so a page-table walk reports "not mapped" without ever
//! dereferencing a derived address. Paths that treat a value from this HAL as a
//! real pointer — `system.rs`'s root address-space setup, exec, grants — are out
//! of scope until paging is modelled. They fail loudly (null dereference) rather
//! than corrupting state.

use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::frame;

/// Types the kernel names through the HAL rather than importing directly.
pub use crate::frame::TrapFrame;
pub use crate::mcontext::Mcontext;

// ------------------------------------------------------------ CPU / sched

const NR_QUEUES: usize = 16;

static CURRENT_PROC: AtomicUsize = AtomicUsize::new(0);
static SMP_PROC: AtomicUsize = AtomicUsize::new(0);
static BILL_PROC: AtomicUsize = AtomicUsize::new(0);
static RUN_Q_HEAD: crate::Shared<[*mut c_void; NR_QUEUES]> =
    crate::Shared::new([null_mut(); NR_QUEUES]);
static RUN_Q_TAIL: crate::Shared<[*mut c_void; NR_QUEUES]> =
    crate::Shared::new([null_mut(); NR_QUEUES]);
static CPULOCALS_READY: AtomicBool = AtomicBool::new(false);

pub fn init() {
    crate::reset();
    // SAFETY: single-threaded; nothing else holds a borrow of the per-CPU state
    // during kernel init.
    unsafe { init_cpulocals() };
}

/// # Safety
///
/// Caller must not hold another borrow of the per-CPU state.
pub unsafe fn init_cpulocals() {
    // SAFETY: single-threaded; the kernel does not race its own boot.
    unsafe {
        for q in RUN_Q_HEAD.get().iter_mut() {
            *q = null_mut();
        }
        for q in RUN_Q_TAIL.get().iter_mut() {
            *q = null_mut();
        }
    }
    CPULOCALS_READY.store(true, Ordering::Relaxed);
}

/// # Safety
///
/// `proc` must stay valid for as long as the kernel keeps it current.
pub unsafe fn set_current_proc(proc: *mut c_void) {
    CURRENT_PROC.store(proc as usize, Ordering::Relaxed);
}

pub fn current_proc() -> *mut c_void {
    CURRENT_PROC.load(Ordering::Relaxed) as *mut c_void
}

pub fn sched_run_q_head() -> *mut [*mut c_void; NR_QUEUES] {
    // SAFETY: single-threaded; the kernel stores only Proc pointers here.
    unsafe { RUN_Q_HEAD.get() as *mut [*mut c_void; NR_QUEUES] }
}

pub fn sched_run_q_tail() -> *mut [*mut c_void; NR_QUEUES] {
    // SAFETY: single-threaded; see `sched_run_q_head`.
    unsafe { RUN_Q_TAIL.get() as *mut [*mut c_void; NR_QUEUES] }
}

pub fn sched_nr_queues() -> usize {
    NR_QUEUES
}

pub fn sched_current_proc() -> *mut c_void {
    current_proc()
}

pub fn sched_bill_proc() -> *mut c_void {
    BILL_PROC.load(Ordering::Relaxed) as *mut c_void
}

/// # Safety
///
/// `proc` must stay valid while the kernel might bill it.
pub unsafe fn sched_set_bill_proc(proc: *mut c_void) {
    BILL_PROC.store(proc as usize, Ordering::Relaxed);
}

pub fn smp_proc_ptr() -> *mut c_void {
    SMP_PROC.load(Ordering::Relaxed) as *mut c_void
}

/// # Safety
///
/// `proc` must stay valid while the kernel might report it.
pub unsafe fn smp_set_proc_ptr(proc: *mut c_void) {
    SMP_PROC.store(proc as usize, Ordering::Relaxed);
}

/// Single CPU. The simulator has no APIC and no secondary processors.
pub fn cpu_id() -> u32 {
    0
}

// ------------------------------------------------------------- interrupts

/// Interrupts are never actually disabled, which matches the x86_64 port's
/// behaviour: its syscalls already run masked, so this reports the same `false`.
pub fn irq_save() -> bool {
    false
}

pub fn irq_restore(_enabled: bool) {}

/// Idling advances the clock so a kernel idle loop makes progress instead of
/// spinning forever; there is no timer interrupt to wake it.
pub fn cpu_idle() {
    crate::advance_cycles(1);
}

pub fn pause() {
    crate::advance_cycles(1);
}

pub fn hlt() {
    crate::advance_cycles(1);
}

pub fn halt() -> ! {
    static HALTING: AtomicBool = AtomicBool::new(false);
    if HALTING.swap(true, Ordering::SeqCst) {
        // Already unwinding a halt: do not recurse into the panic path.
        loop {
            core::hint::spin_loop();
        }
    }
    panic!("arch-sim: kernel halted");
}

pub fn qemu_exit(code: u32) -> ! {
    panic!("arch-sim: kernel exit with code {}", code);
}

// ------------------------------------------------------------------ clock

pub fn read_cycles() -> u64 {
    crate::clock_read()
}

pub fn read_tsc() -> u64 {
    crate::clock_read()
}

/// # Safety
///
/// No preconditions; `unsafe` only to match the other ports' signatures.
pub unsafe fn read_tsc_ctr_switch() -> u64 {
    crate::tsc_switch()
}

/// # Safety
///
/// No preconditions; see `read_tsc_ctr_switch`.
pub unsafe fn write_tsc_ctr_switch(val: u64) {
    crate::set_tsc_switch(val);
}

/// # Safety
///
/// `callback` must be callable with no arguments.
pub unsafe fn init_profile_clock(_rate_code: u32, callback: unsafe extern "C" fn()) -> i32 {
    crate::set_profile_callback(Some(callback));
    0
}

pub fn stop_profile_clock() {
    crate::set_profile_callback(None);
}

/// Run the profiler callback, if one is installed. There is no timer interrupt
/// to drive sampling, so tests call this explicitly.
pub fn profile_tick() {
    crate::profile_tick();
}

// ------------------------------------------------------------------ console

pub fn serial_write_byte(byte: u8) {
    crate::console_write(byte);
}

pub fn serial_byte_available() -> bool {
    crate::console_len_input() > 0
}

pub fn poll_console() -> Option<u8> {
    crate::console_take_input()
}

/// Blocking read. Bounded rather than unbounded: with no timer there is nothing
/// to interrupt a wait, so a missing byte would hang the test run rather than
/// fail it. Panicking names the problem instead.
pub fn serial_read_byte() -> u8 {
    for _ in 0..1_000_000 {
        if let Some(byte) = crate::console_take_input() {
            return byte;
        }
        crate::advance_cycles(1);
    }
    panic!("arch-sim: serial_read_byte found no input (nothing will supply one)");
}

// ------------------------------------------------------------- locking

/// # Safety
///
/// No preconditions; the simulator is single-threaded.
pub unsafe fn bkl_lock() {}

/// # Safety
///
/// No preconditions; the simulator is single-threaded.
pub unsafe fn bkl_unlock() {}

/// # Safety
///
/// No preconditions.
pub unsafe fn mfence() {}

/// # Safety
///
/// No preconditions; there is no real TLB.
pub unsafe fn tlb_flush() {}

/// # Safety
///
/// No preconditions; there is no real TLB.
pub unsafe fn tlb_flush_page(_va: u64) {}

/// # Safety
///
/// No preconditions.
pub unsafe fn release_fpu(_proc: *mut c_void) {}

/// # Safety
///
/// No preconditions.
pub unsafe fn set_tls_current(_tls: u64) {}

// ---------------------------------------------------------------- frames

/// # Safety
///
/// `offset + 8` must lie within the 256-byte frame.
pub unsafe fn read_frame_field(frame: &[u8; 256], offset: usize) -> u64 {
    unsafe { frame::read_field(frame, offset) }
}

/// # Safety
///
/// `offset + 8` must lie within the 256-byte frame.
pub unsafe fn write_frame_field(frame: &mut [u8; 256], offset: usize, val: u64) {
    unsafe { frame::write_field(frame, offset, val) }
}

/// # Safety
///
/// `frame` must be a writable register save area.
///
/// `main_hdr` is the `PT_INTERP` loader's second argument on the hardware
/// ports. The simulator never runs a loader, so it is accepted to keep the
/// HAL uniform and dropped.
pub unsafe fn exec_init_regs(
    frame: &mut [u8; 256],
    entry: u64,
    sp: u64,
    _argc: u64,
    _argv: u64,
    _main_hdr: u64,
) {
    unsafe {
        write_frame_field(frame, frame::RIP, entry);
        write_frame_field(frame, frame::RSP, sp);
        write_frame_field(
            frame,
            frame::RFLAGS,
            read_frame_field(frame, frame::RFLAGS) | 0x200,
        );
        write_frame_field(frame, frame::CS, 1);
    }
}

/// # Safety
///
/// `frame` must be a readable register save area.
pub unsafe fn read_syscall_arg(frame: &[u8; 256], i: usize) -> u64 {
    unsafe { read_frame_field(frame, frame::arg_offset(i)) }
}

/// # Safety
///
/// `frame` must be a writable register save area.
pub unsafe fn write_retval(frame: &mut [u8; 256], val: u64) {
    unsafe { write_frame_field(frame, frame::RAX, val) }
}

/// Read back the syscall return value [`write_retval`] last stored.
///
/// The shipping arches read this out of the trap frame on the way back to
/// userland. A wasm instance has no such path — its syscall is an ordinary call
/// whose return belongs to the host — so the platform layer asks for it
/// explicitly when it resumes a call that blocked, and a later write (a receive's
/// sender endpoint, say) is what that resume observes.
///
/// # Safety
///
/// `frame` must be a readable register save area.
pub unsafe fn read_retval(frame: &[u8; 256]) -> u64 {
    unsafe { read_frame_field(frame, frame::RAX) }
}

/// # Safety
///
/// `frame` must be a readable register save area.
pub unsafe fn read_syscall_nr(frame: &[u8; 256]) -> u64 {
    unsafe { read_frame_field(frame, frame::RAX) }
}

/// # Safety
///
/// `frame` must be a readable register save area.
pub unsafe fn read_frame_ip(frame: &[u8; 256]) -> u64 {
    unsafe { read_frame_field(frame, frame::RIP) }
}

/// # Safety
///
/// `frame` must be a writable register save area.
pub unsafe fn write_frame_ip(frame: &mut [u8; 256], ip: u64) {
    unsafe { write_frame_field(frame, frame::RIP, ip) }
}

/// # Safety
///
/// `frame` must be a writable register save area.
pub unsafe fn set_initial_regs(frame: &mut [u8; 256], entry: u64, sp: u64, arg: u64) {
    unsafe {
        write_frame_field(frame, frame::RIP, entry);
        write_frame_field(frame, frame::RSP, sp);
        write_frame_field(frame, frame::arg_offset(0), arg);
        write_frame_field(frame, frame::RFLAGS, 0x202);
        write_frame_field(frame, frame::CS, 1);
    }
}

/// # Safety
///
/// Both frames must be valid 256-byte register save areas.
pub unsafe fn copy_frame(dst: &mut [u8; 256], src: &[u8; 256]) {
    dst.copy_from_slice(src);
}

pub fn frame_default() -> [u8; 256] {
    frame::default_frame()
}

/// # Safety
///
/// `frame` must be a readable register save area.
pub unsafe fn read_frame_sp(frame: &[u8; 256]) -> u64 {
    unsafe { read_frame_field(frame, frame::RSP) }
}

/// # Safety
///
/// `frame` must be a writable register save area of at least 256 bytes.
pub unsafe fn arch_proc_init(
    frame: &mut [u8; 256],
    entry: u64,
    stack: u64,
    _name: &[u8],
    _ps_str: u64,
) {
    unsafe { set_initial_regs(frame, entry, stack, 0) }
}

// ---------------------------------------------------------------- signals

/// No signal frames are built in the simulator: signals are exercised through
/// the kernel's own bookkeeping, and there is no user memory to place a frame in.
/// The size is reported so the kernel's arithmetic stays self-consistent.
pub const fn sigframe_size() -> usize {
    512
}

pub const fn sigframe_addr(old_sp: u64) -> u64 {
    old_sp.saturating_sub(sigframe_size() as u64) & !0xF
}

/// # Safety
///
/// `dst` must have room for `sigframe_size()` bytes.
pub unsafe fn build_sigframe(
    dst: &mut [u8],
    saved: &[u8; 256],
    signo: u32,
    mask: &[u8; 16],
    trampoline: u64,
    _frame_addr: u64,
) {
    use arch_common::consts::{SC_MAGIC, sigframe as sf};
    dst[0..8].copy_from_slice(&trampoline.to_ne_bytes());
    dst[sf::MASK_OFF..sf::MASK_OFF + 16].copy_from_slice(mask);
    dst[sf::SIGNAL_OFF..sf::SIGNAL_OFF + 4].copy_from_slice(&signo.to_ne_bytes());
    dst[sf::REGS_OFF..sf::REGS_OFF + 256].copy_from_slice(saved);
    dst[sf::MAGIC_OFF..sf::MAGIC_OFF + 8].copy_from_slice(&SC_MAGIC.to_ne_bytes());
}

/// # Safety
///
/// `p_reg` must be a writable register save area.
pub unsafe fn sigframe_set_entry(
    p_reg: &mut [u8; 256],
    handler: u64,
    frame: u64,
    _signo: u32,
    _trampoline: u64,
) {
    unsafe {
        write_frame_field(p_reg, frame::RIP, handler);
        write_frame_field(p_reg, frame::RSP, frame);
    }
}

/// # Safety
///
/// `frame` must hold at least 32 + 256 bytes written by `build_sigframe`.
pub unsafe fn sigframe_restore(p_reg: &mut [u8; 256], frame: &[u8]) {
    use arch_common::consts::sigframe as sf;
    p_reg.copy_from_slice(&frame[sf::REGS_OFF..sf::REGS_OFF + 256]);
}

/// # Safety
///
/// `frame` must be a readable register save area.
pub unsafe fn trapframe_to_mcontext(frame: &[u8; 256]) -> Mcontext {
    let mut mc = Mcontext::default();
    unsafe {
        mc.mc_rax = read_frame_field(frame, frame::RAX);
        mc.mc_rip = read_frame_field(frame, frame::RIP);
        mc.mc_rsp = read_frame_field(frame, frame::RSP);
        mc.mc_rflags = read_frame_field(frame, frame::RFLAGS);
        mc.mc_cs = read_frame_field(frame, frame::CS);
        for i in 0..6 {
            let val = read_frame_field(frame, frame::arg_offset(i));
            match i {
                0 => mc.mc_rdi = val,
                1 => mc.mc_rsi = val,
                2 => mc.mc_rdx = val,
                3 => mc.mc_rcx = val,
                4 => mc.mc_r8 = val,
                _ => mc.mc_r9 = val,
            }
        }
    }
    mc
}

/// # Safety
///
/// `frame` must be a writable register save area.
pub unsafe fn mcontext_to_trapframe(frame: &mut [u8; 256], mc: &Mcontext) {
    unsafe {
        write_frame_field(frame, frame::RAX, mc.mc_rax);
        write_frame_field(frame, frame::RIP, mc.mc_rip);
        write_frame_field(frame, frame::RSP, mc.mc_rsp);
        write_frame_field(frame, frame::RFLAGS, mc.mc_rflags);
        write_frame_field(frame, frame::CS, mc.mc_cs);
        write_frame_field(frame, frame::arg_offset(0), mc.mc_rdi);
        write_frame_field(frame, frame::arg_offset(1), mc.mc_rsi);
        write_frame_field(frame, frame::arg_offset(2), mc.mc_rdx);
        write_frame_field(frame, frame::arg_offset(3), mc.mc_rcx);
        write_frame_field(frame, frame::arg_offset(4), mc.mc_r8);
        write_frame_field(frame, frame::arg_offset(5), mc.mc_r9);
    }
}

/// # Safety
///
/// No preconditions; there is no FPU state to release.
pub unsafe fn read_frame_pointer() -> u64 {
    0
}

// ---------------------------------------------------------------- paging

pub type PtEntry = u64;

pub struct PageNotMapped;

/// Zero levels, so a page-table walk reports "not mapped" immediately instead of
/// dereferencing an address this HAL invented.
pub const fn pt_levels() -> u32 {
    0
}

pub const fn pt_index(va: u64, level: u32) -> usize {
    ((va >> (12 + 9 * level)) & 0x1FF) as usize
}

pub const fn pte_present() -> u64 {
    0x1
}

pub const fn pte_writable() -> u64 {
    0x2
}

pub const fn pte_is_writable(pte: u64) -> bool {
    pte & 0x2 != 0
}

pub const fn pte_set_writable(pte: u64) -> u64 {
    pte | 0x2
}

pub const fn pte_is_user(pte: u64) -> bool {
    pte & 0x4 != 0
}

pub const fn pte_user() -> u64 {
    0x4
}

pub const fn pte_large_page() -> u64 {
    0x80
}

pub const fn pte_global() -> u64 {
    0x100
}

pub const fn pte_frame_mask() -> u64 {
    0x000F_FFFF_FFFF_F000
}

pub const fn pte_flags_mask() -> u64 {
    !pte_frame_mask()
}

/// No physical address is meaningful here, so only the trivially safe cases pass.
pub const fn pte_is_valid_phys(phys: u64) -> bool {
    phys != 0
}

pub const fn pte_nonleaf_flags() -> u64 {
    pte_present() | pte_writable() | pte_user()
}

pub const fn pte_leaf_flags() -> u64 {
    pte_present() | pte_writable() | pte_user()
}

pub const fn pte_split_flags(source_pte: u64, _next_level: u32) -> u64 {
    source_pte & pte_flags_mask()
}

pub const fn pte_pd_split_exclude_mask() -> u64 {
    0
}

pub const fn pte_pd_split_clear_mask() -> u64 {
    0
}

pub const fn pte_user_flags() -> u64 {
    pte_present() | pte_writable() | pte_user()
}

pub const fn build_pte(pa: u64, flags: u64) -> u64 {
    (pa & pte_frame_mask()) | (flags & pte_flags_mask())
}

pub const fn pte_to_phys(pte: u64) -> u64 {
    pte & pte_frame_mask()
}

pub const fn pte_user_owned(pte: u64, _va: u64) -> bool {
    pte_is_user(pte)
}

/// A null handle: the kernel's `boot_cr3` consumers treat this as a real
/// translation root, so returning null makes them fault immediately and
/// visibly instead of touching an invented address.
pub fn boot_cr3() -> u64 {
    0
}

/// # Safety
///
/// No preconditions.
pub unsafe fn read_cr3() -> u64 {
    0
}

/// # Safety
///
/// No preconditions; nothing is switched.
pub unsafe fn write_cr3(_cr3: u64) {}

/// # Safety
///
/// No preconditions.
pub unsafe fn clear_rw(_cr3: u64, _va: u64) -> Result<(), PageNotMapped> {
    Err(PageNotMapped)
}

/// # Safety
///
/// No preconditions.
pub unsafe fn read_fault_addr() -> u64 {
    0
}

/// # Safety
///
/// No preconditions; always reports failure since no page tables exist.
pub unsafe fn vm_paging_fork(_parent_cr3: u64, _child_cr3: u64, _msg: &mut [u8; 64]) -> i32 {
    -1
}

/// # Safety
///
/// No preconditions.
pub unsafe fn exec_create_root(_boot_cr3: u64) -> u64 {
    0
}

// ------------------------------------------------------------- VA layout

pub const PAGE_SIZE: u64 = 4096;
pub const PAGE_SHIFT: u64 = 12;
pub const ELF_MACHINE: u16 = 62;
pub const FPU_STATE_SIZE: usize = 512;
pub const KERNBASE: u64 = 0xFFFF_8000_0000_0000;
pub const MAX_USER_ADDRESS: u64 = 0x0000_8000_0000_0000;

pub const MAP_PRESENT: u64 = 0x1;
pub const MAP_READ: u64 = 0;
pub const MAP_WRITE: u64 = 0x2;
pub const MAP_USER: u64 = 0x4;
pub const MAP_EXEC: u64 = 0;
pub const MAP_NX: u64 = 0x8000_0000_0000_0000;

pub const fn kern_vaddr() -> u64 {
    0x200000
}

pub const fn user_stack_base() -> u64 {
    0x0FE0_0000
}

pub const fn user_stack_size() -> usize {
    0x10_0000
}

pub const fn user_heap_base() -> u64 {
    0x3FE0_0000
}

pub const fn user_heap_limit() -> u64 {
    0x1_0000_0000
}

pub const fn user_priority() -> i8 {
    7
}

pub const fn user_quantum_ms() -> u32 {
    200
}

pub const fn user_quantum_cycles() -> u64 {
    500_000_000
}

pub const fn mmap_base() -> u64 {
    0x1_0000_0000
}

pub const fn vm_scratch_base() -> u64 {
    (MAX_USER_ADDRESS - 0x1_0000_0000) & !0xFFF
}

// ------------------------------------------------------- physical memory

/// # Safety
///
/// No preconditions; the arena is in-process memory.
pub unsafe fn init_phys_alloc(base: u64, size: u64) {
    crate::phys_init(base, size);
}

/// # Safety
///
/// No preconditions.
pub unsafe fn alloc_phys_page() -> Option<u64> {
    crate::phys_alloc_page()
}

/// # Safety
///
/// No preconditions.
pub unsafe fn alloc_phys_contig(count: usize) -> Option<u64> {
    crate::phys_alloc_contig(count)
}

/// # Safety
///
/// `addr` must come from `alloc_phys_contig`.
pub unsafe fn free_phys_contig(addr: u64, count: usize) {
    crate::phys_free_contig(addr, count);
}

pub fn phys_alloc_base() -> u64 {
    crate::phys_base()
}

pub fn phys_alloc_usable_size() -> u64 {
    crate::phys_usable_size()
}

pub fn phys_free_pages() -> usize {
    crate::phys_free_pages()
}

// -------------------------------------------------------- link / platform

/// No linker-provided BSS symbols exist for a host build. Reporting an empty
/// range means the kernel's BSS sweep is a no-op, which is correct: the host
/// runtime already zeroed it.
pub fn bss_start() -> u64 {
    0
}

pub fn bss_end() -> u64 {
    0
}

pub const fn has_port_io() -> bool {
    false
}

pub const fn fork_needs_child_flag_clear() -> bool {
    false
}

/// # Safety
///
/// No preconditions; there is no I/O port space.
pub unsafe fn inb(_port: u16) -> u8 {
    0
}

/// # Safety
///
/// No preconditions; there is no I/O port space.
pub unsafe fn outb(_port: u16, _val: u8) {}

/// # Safety
///
/// No preconditions; there is no I/O port space.
pub unsafe fn inw(_port: u16) -> u16 {
    0
}

/// # Safety
///
/// No preconditions; there is no I/O port space.
pub unsafe fn outw(_port: u16, _val: u16) {}

/// # Safety
///
/// No preconditions; there is no I/O port space.
pub unsafe fn inl(_port: u16) -> u32 {
    0
}

/// # Safety
///
/// No preconditions; there is no I/O port space.
pub unsafe fn outl(_port: u16, _val: u32) {}

/// # Safety
///
/// No preconditions; there is no I/O port space.
pub unsafe fn phys_insb(_port: u16, _buf: u64, _count: usize) {}

/// # Safety
///
/// No preconditions; there is no I/O port space.
pub unsafe fn phys_outsb(_port: u16, _buf: u64, _count: usize) {}

/// # Safety
///
/// No preconditions; there is no I/O port space.
pub unsafe fn phys_insw(_port: u16, _buf: u64, _count: usize) {}

/// # Safety
///
/// No preconditions; there is no I/O port space.
pub unsafe fn phys_outsw(_port: u16, _buf: u64, _count: usize) {}

pub const PCI_ADDR_PORT: u16 = 0xCF8;
pub const PCI_DATA_PORT: u16 = 0xCFC;

/// No PCI bus. Devices in the simulator are modelled by the test harness.
pub fn pci_config_addr(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    ((bus as u32) << 16)
        | (((dev as u32) & 0x1F) << 11)
        | (((func as u32) & 0x07) << 8)
        | (reg as u32 & 0xFC)
}

/// # Safety
///
/// No preconditions; there is no PCI bus.
pub unsafe fn pci_cfg_read8(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u8 {
    0xFF
}

/// # Safety
///
/// No preconditions; there is no PCI bus.
pub unsafe fn pci_cfg_read16(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u16 {
    0xFFFF
}

/// # Safety
///
/// No preconditions; there is no PCI bus.
pub unsafe fn pci_cfg_read32(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u32 {
    0xFFFF_FFFF
}

/// # Safety
///
/// No preconditions; there is no PCI bus.
pub unsafe fn pci_cfg_write32(_bus: u8, _dev: u8, _func: u8, _reg: u8, _val: u32) {}

pub const RTC_INDEX: u16 = 0x70;

/// Fixed RTC reading, so a test that formats a timestamp is reproducible.
/// Bit 7 of `reg` (the NMI-disable bit on the index port) is masked off, as real
/// hardware does.
///
/// # Safety
///
/// No preconditions; `unsafe` only to match the other ports' signatures.
pub unsafe fn cmos_read(reg: u8) -> u8 {
    match reg & 0x7F {
        0x00 => 0x00, // seconds
        0x02 => 0x00, // minutes
        0x04 => 0x12, // hours
        0x07 => 0x01, // day of month
        0x08 => 0x01, // month
        0x09 => 0x25, // year (2025)
        _ => 0,
    }
}

/// # Safety
///
/// No preconditions; writes are discarded.
pub unsafe fn cmos_write(_reg: u8, _val: u8) {}
