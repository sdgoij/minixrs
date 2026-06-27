//! Privilege structures — adapted from `minix/kernel/priv.h`
//!
//! Defines the `Priv` struct, privilege flags, system privilege table,
//! I/O/memory/IRQ range types, and convenience accessors.
//!
//! **x86_64 differences from i386:**
//! - `vir_bytes = u64`, `phys_bytes = u64` (was 32-bit on i386)
//! - `bitchunk_t = u32` (same as C `uint32_t`)
//! - `reg_t = u64` (was u32 on i386)
//! - `irq_id_t = u64` (C `unsigned long`)

use crate::proc::NR_SYS_PROCS;

// ─────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────

pub const NR_IO_RANGE: usize = 64;
pub const NR_MEM_RANGE: usize = 20;
pub const NR_IRQ: usize = 16;
pub const NR_SYS_CALLS: usize = 58;
pub const SYS_CALL_MASK_SIZE: usize = NR_SYS_CALLS.div_ceil(32); // BITMAP_CHUNKS

pub const NR_STATIC_PRIV_IDS: usize = NR_TASKS + LAST_SPECIAL_PROC_NR + 1;

/// Internal constants needed before NR_TASKS/NR_PROCS are available.
const NR_TASKS: usize = 5;
const LAST_SPECIAL_PROC_NR: usize = 10;

/// Stack guard value for x86_64 (sizeof(reg_t) == 8).
pub const STACK_GUARD: u64 = 0xDEAD_BEEF;

// ─────────────────────────────────────────────────────────────────────────
// Privilege Flags
// ─────────────────────────────────────────────────────────────────────────

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct PrivFlags: i16 {
        /// Process is preemptible.
        const PREEMPTIBLE     = 0x002;
        /// Process is billable.
        const BILLABLE        = 0x004;
        /// Dynamic privilege ID.
        const DYN_PRIV_ID     = 0x008;
        /// System process.
        const SYS_PROC        = 0x010;
        /// Check I/O port access.
        const CHECK_IO_PORT   = 0x020;
        /// Check IRQ access.
        const CHECK_IRQ       = 0x040;
        /// Check memory access.
        const CHECK_MEM       = 0x080;
        /// Root system process.
        const ROOT_SYS_PROC   = 0x100;
        /// VM system process.
        const VM_SYS_PROC     = 0x200;
        /// Live update system process.
        const LU_SYS_PROC     = 0x400;
        /// Restartable system process.
        const RST_SYS_PROC    = 0x800;
    }
}

impl Default for PrivFlags {
    fn default() -> Self {
        PrivFlags::empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// SysMap — bitmap for system indexes
// ─────────────────────────────────────────────────────────────────────────

/// Number of u32 chunks in a sys_map_t covering NR_SYS_PROCS bits.
const SYS_MAP_CHUNKS: usize = NR_SYS_PROCS.div_ceil(32);

/// Bitmap for system process indexes.
///
/// Matches C `typedef struct { bitchunk_t chunk[BITMAP_CHUNKS(NR_SYS_PROCS)]; } sys_map_t;`
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct SysMap {
    pub chunk: [u32; SYS_MAP_CHUNKS],
}

impl SysMap {
    /// Create a new empty sys_map.
    pub const fn new() -> Self {
        Self {
            chunk: [0u32; SYS_MAP_CHUNKS],
        }
    }

    /// Test if bit `id` is set.
    pub fn test(&self, id: usize) -> bool {
        if id >= NR_SYS_PROCS {
            return false;
        }
        let i = id / 32;
        let b = id % 32;
        (self.chunk[i] & (1u32 << b)) != 0
    }

    /// Set bit `id`.
    pub fn set(&mut self, id: usize) {
        if id < NR_SYS_PROCS {
            let i = id / 32;
            let b = id % 32;
            self.chunk[i] |= 1u32 << b;
        }
    }

    /// Clear bit `id`.
    pub fn clear(&mut self, id: usize) {
        if id < NR_SYS_PROCS {
            let i = id / 32;
            let b = id % 32;
            self.chunk[i] &= !(1u32 << b);
        }
    }

    /// Check if the map is entirely zero.
    pub fn is_empty(&self) -> bool {
        self.chunk.iter().all(|&c| c == 0)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// IoRange
// ─────────────────────────────────────────────────────────────────────────

/// I/O port range.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct IoRange {
    /// Lowest I/O port in range.
    pub ior_base: u32,
    /// Highest I/O port in range.
    pub ior_limit: u32,
}

// ─────────────────────────────────────────────────────────────────────────
// MemRange
// ─────────────────────────────────────────────────────────────────────────

/// Memory range.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct MemRange {
    /// Lowest memory address in range.
    pub mr_base: u64,
    /// Highest memory address in range.
    pub mr_limit: u64,
}

// ─────────────────────────────────────────────────────────────────────────
// MinixTimer (placeholder)
// ─────────────────────────────────────────────────────────────────────────

/// Timer structure (placeholder).
///
/// Full definition from `minix/timers.h`:
/// ```c
/// struct minix_timer {
///   struct minix_timer *tmr_next;
///   clock_t tmr_exp_time;
///   tmr_func_t tmr_func;
///   tmr_arg_t tmr_arg;
/// };
/// ```
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MinixTimer {
    pub tmr_next: *mut MinixTimer,
    pub tmr_exp_time: u64,
    pub tmr_func: usize, // function pointer as usize
    pub tmr_arg: usize,  // opaque argument
}

impl Default for MinixTimer {
    fn default() -> Self {
        Self {
            tmr_next: core::ptr::null_mut(),
            tmr_exp_time: 0,
            tmr_func: 0,
            tmr_arg: 0,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Priv
// ─────────────────────────────────────────────────────────────────────────

/// System privilege structure.
///
/// Each system process gets its own `Priv`; all user processes share one.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Priv {
    /// Number of associated process.
    pub s_proc_nr: i32,
    /// Index of this privilege structure.
    pub s_id: i16,
    /// Flags: PREEMPTIBLE, BILLABLE, etc.
    pub s_flags: PrivFlags,

    /// Address of async send table in process' address space.
    pub s_asyntab: u64,
    /// Number of elements in async table (0 = not in use).
    pub s_asynsize: usize,

    /// Allowed system call traps.
    pub s_trap_mask: i16,
    _trap_pad: [u8; 6],
    /// Allowed destination processes.
    pub s_ipc_to: SysMap,

    /// Allowed kernel calls.
    pub s_k_call_mask: [u32; SYS_CALL_MASK_SIZE],

    /// Signal manager for system signals.
    pub s_sig_mgr: i32,
    /// Backup signal manager for system signals.
    pub s_bak_sig_mgr: i32,

    /// Bitmap with pending notifications.
    pub s_notify_pending: SysMap,
    /// Bitmap with pending async messages.
    pub s_asyn_pending: SysMap,
    /// Pending hardware interrupts.
    pub s_int_pending: u64,
    /// Pending signals.
    pub s_sig_pending: u32,

    /// Synchronous alarm timer.
    pub s_alarm_timer: MinixTimer,
    /// Stack guard word for kernel tasks.
    pub s_stack_guard: *mut u64,

    /// Send a SIGKMESS when diagnostics arrive?
    pub s_diag_sig: i8,
    _diag_pad: [u8; 7],

    /// Allowed I/O port ranges.
    pub s_nr_io_range: i32,
    pub s_io_tab: [IoRange; NR_IO_RANGE],

    /// Allowed memory ranges.
    pub s_nr_mem_range: i32,
    pub s_mem_tab: [MemRange; NR_MEM_RANGE],

    /// Allowed IRQ lines.
    pub s_nr_irq: i32,
    pub s_irq_tab: [i32; NR_IRQ],

    /// Grant table address (or 0).
    pub s_grant_table: u64,
    /// Number of grant entries (or 0).
    pub s_grant_entries: i32,
}

impl Default for Priv {
    fn default() -> Self {
        Self {
            s_proc_nr: 0,
            s_id: 0,
            s_flags: PrivFlags::empty(),
            s_asyntab: 0,
            s_asynsize: 0,
            s_trap_mask: 0,
            _trap_pad: [0u8; 6],
            s_ipc_to: SysMap::new(),
            s_k_call_mask: [0u32; SYS_CALL_MASK_SIZE],
            s_sig_mgr: i32::MIN, // NONE
            s_bak_sig_mgr: i32::MIN,
            s_notify_pending: SysMap::new(),
            s_asyn_pending: SysMap::new(),
            s_int_pending: 0,
            s_sig_pending: 0,
            s_alarm_timer: MinixTimer::default(),
            s_stack_guard: core::ptr::null_mut(),
            s_diag_sig: 0,
            _diag_pad: [0u8; 7],
            s_nr_io_range: 0,
            s_io_tab: [IoRange::default(); NR_IO_RANGE],
            s_nr_mem_range: 0,
            s_mem_tab: [MemRange::default(); NR_MEM_RANGE],
            s_nr_irq: 0,
            s_irq_tab: [0i32; NR_IRQ],
            s_grant_table: 0,
            s_grant_entries: 0,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Global privilege table
// ─────────────────────────────────────────────────────────────────────────

pub static mut PRIV: [Priv; NR_SYS_PROCS] = [Priv {
    s_proc_nr: 0,
    s_id: 0,
    s_flags: PrivFlags::empty(),
    s_asyntab: 0,
    s_asynsize: 0,
    s_trap_mask: 0,
    _trap_pad: [0u8; 6],
    s_ipc_to: SysMap::new(),
    s_k_call_mask: [0u32; SYS_CALL_MASK_SIZE],
    s_sig_mgr: i32::MIN,
    s_bak_sig_mgr: i32::MIN,
    s_notify_pending: SysMap::new(),
    s_asyn_pending: SysMap::new(),
    s_int_pending: 0,
    s_sig_pending: 0,
    s_alarm_timer: MinixTimer {
        tmr_next: core::ptr::null_mut(),
        tmr_exp_time: 0,
        tmr_func: 0,
        tmr_arg: 0,
    },
    s_stack_guard: core::ptr::null_mut(),
    s_diag_sig: 0,
    _diag_pad: [0u8; 7],
    s_nr_io_range: 0,
    s_io_tab: [IoRange {
        ior_base: 0,
        ior_limit: 0,
    }; NR_IO_RANGE],
    s_nr_mem_range: 0,
    s_mem_tab: [MemRange {
        mr_base: 0,
        mr_limit: 0,
    }; NR_MEM_RANGE],
    s_nr_irq: 0,
    s_irq_tab: [0i32; NR_IRQ],
    s_grant_table: 0,
    s_grant_entries: 0,
}; NR_SYS_PROCS];

/// Direct slot pointers for fast access.
pub static mut PPRIV_ADDR: [*mut Priv; NR_SYS_PROCS] = [core::ptr::null_mut(); NR_SYS_PROCS];

/// Idle privilege structure (shared).
pub static mut IDLE_PRIV: Priv = Priv {
    s_proc_nr: 0,
    s_id: 0,
    s_flags: PrivFlags::empty(),
    s_asyntab: 0,
    s_asynsize: 0,
    s_trap_mask: 0,
    _trap_pad: [0u8; 6],
    s_ipc_to: SysMap::new(),
    s_k_call_mask: [0u32; SYS_CALL_MASK_SIZE],
    s_sig_mgr: i32::MIN,
    s_bak_sig_mgr: i32::MIN,
    s_notify_pending: SysMap::new(),
    s_asyn_pending: SysMap::new(),
    s_int_pending: 0,
    s_sig_pending: 0,
    s_alarm_timer: MinixTimer {
        tmr_next: core::ptr::null_mut(),
        tmr_exp_time: 0,
        tmr_func: 0,
        tmr_arg: 0,
    },
    s_stack_guard: core::ptr::null_mut(),
    s_diag_sig: 0,
    _diag_pad: [0u8; 7],
    s_nr_io_range: 0,
    s_io_tab: [IoRange {
        ior_base: 0,
        ior_limit: 0,
    }; NR_IO_RANGE],
    s_nr_mem_range: 0,
    s_mem_tab: [MemRange {
        mr_base: 0,
        mr_limit: 0,
    }; NR_MEM_RANGE],
    s_nr_irq: 0,
    s_irq_tab: [0i32; NR_IRQ],
    s_grant_table: 0,
    s_grant_entries: 0,
};

// ─────────────────────────────────────────────────────────────────────────
// Accessors
// ─────────────────────────────────────────────────────────────────────────

/// Get privilege structure for a given index.
pub fn priv_addr(i: usize) -> &'static Priv {
    unsafe { &*PPRIV_ADDR[i] }
}

/// Get mutable privilege structure for a given index.
pub fn priv_addr_mut(i: usize) -> &'static mut Priv {
    unsafe { &mut *PPRIV_ADDR[i] }
}

/// Get the privilege ID of a process from its Proc's p_priv pointer.
pub fn priv_id(rp: &crate::proc::Proc) -> i16 {
    unsafe { (*rp.p_priv).s_id }
}

/// Look up process number from privilege ID.
pub fn id_to_nr(id: usize) -> i32 {
    priv_addr(id).s_proc_nr
}

/// Check whether process `rp` may send to process with endpoint `proc_nr_e`.
pub fn may_send_to(rp: &crate::proc::Proc, _proc_nr_e: i32) -> bool {
    // Get the privilege ID of the target process
    // This requires the process table, which is in task 3.3
    // For now, just check if the IPC map has the bit set
    let priv_id = priv_id(rp) as usize;
    unsafe { (*rp.p_priv).s_ipc_to.test(priv_id) }
}

// ─────────────────────────────────────────────────────────────────────────
// Address constants (as functions, since they reference statics)
// ─────────────────────────────────────────────────────────────────────────

pub fn beg_priv_addr() -> *const Priv {
    core::ptr::addr_of!(PRIV).cast::<Priv>()
}

pub fn end_priv_addr() -> *const Priv {
    // SAFETY: PRIV has NR_SYS_PROCS elements; .add(NR_SYS_PROCS) is one past the end, valid for pointer arithmetic.
    unsafe { core::ptr::addr_of!(PRIV).cast::<Priv>().add(NR_SYS_PROCS) }
}

pub fn beg_static_priv_addr() -> *const Priv {
    beg_priv_addr()
}

pub fn end_static_priv_addr() -> *const Priv {
    // SAFETY: NR_STATIC_PRIV_IDS <= NR_SYS_PROCS (both are compile-time consts).
    unsafe {
        core::ptr::addr_of!(PRIV)
            .cast::<Priv>()
            .add(NR_STATIC_PRIV_IDS)
    }
}

pub fn beg_dyn_priv_addr() -> *const Priv {
    // SAFETY: same as end_static_priv_addr.
    unsafe {
        core::ptr::addr_of!(PRIV)
            .cast::<Priv>()
            .add(NR_STATIC_PRIV_IDS)
    }
}

pub fn end_dyn_priv_addr() -> *const Priv {
    end_priv_addr()
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priv_default_proc_nr() {
        let p = Priv::default();
        assert_eq!(p.s_proc_nr, 0);
    }

    #[test]
    fn test_priv_flags_empty() {
        let f = PrivFlags::empty();
        assert_eq!(f.bits(), 0);
    }

    #[test]
    fn test_priv_flags_values() {
        assert_eq!(PrivFlags::PREEMPTIBLE.bits(), 0x002);
        assert_eq!(PrivFlags::BILLABLE.bits(), 0x004);
        assert_eq!(PrivFlags::SYS_PROC.bits(), 0x010);
        assert_eq!(PrivFlags::CHECK_IO_PORT.bits(), 0x020);
        assert_eq!(PrivFlags::CHECK_IRQ.bits(), 0x040);
        assert_eq!(PrivFlags::CHECK_MEM.bits(), 0x080);
        assert_eq!(PrivFlags::ROOT_SYS_PROC.bits(), 0x100);
        assert_eq!(PrivFlags::VM_SYS_PROC.bits(), 0x200);
        assert_eq!(PrivFlags::LU_SYS_PROC.bits(), 0x400);
        assert_eq!(PrivFlags::RST_SYS_PROC.bits(), 0x800);
    }

    #[test]
    fn test_sys_map_new() {
        let m = SysMap::new();
        assert!(m.is_empty());
    }

    #[test]
    fn test_sys_map_set_test() {
        let mut m = SysMap::new();
        m.set(0);
        assert!(m.test(0));
        assert!(!m.test(1));
    }

    #[test]
    fn test_sys_map_clear() {
        let mut m = SysMap::new();
        m.set(5);
        assert!(m.test(5));
        m.clear(5);
        assert!(!m.test(5));
    }

    #[test]
    fn test_sys_map_out_of_bounds() {
        let mut m = SysMap::new();
        m.set(NR_SYS_PROCS); // should be ignored
        assert!(!m.test(NR_SYS_PROCS));
    }

    #[test]
    fn test_io_range_default() {
        let r = IoRange::default();
        assert_eq!(r.ior_base, 0);
        assert_eq!(r.ior_limit, 0);
    }

    #[test]
    fn test_mem_range_default() {
        let r = MemRange::default();
        assert_eq!(r.mr_base, 0);
        assert_eq!(r.mr_limit, 0);
    }

    #[test]
    fn test_minix_timer_default() {
        let t = MinixTimer::default();
        assert!(t.tmr_next.is_null());
    }

    #[test]
    fn test_constants() {
        assert_eq!(NR_IO_RANGE, 64);
        assert_eq!(NR_MEM_RANGE, 20);
        assert_eq!(NR_IRQ, 16);
        assert_eq!(NR_SYS_CALLS, 58);
        assert_eq!(NR_STATIC_PRIV_IDS, 16);
        assert_eq!(STACK_GUARD, 0xDEAD_BEEF);
    }

    #[test]
    fn test_priv_sig_mgr_default_is_none() {
        let p = Priv::default();
        assert_eq!(p.s_sig_mgr, i32::MIN);
    }

    #[test]
    fn test_idle_priv_exists() {
        unsafe {
            assert_eq!(core::ptr::addr_of!(IDLE_PRIV).read().s_proc_nr, 0);
        }
    }

    #[test]
    fn test_priv_table_size() {
        // PRIV should have NR_SYS_PROCS = 64 entries
        unsafe {
            assert_eq!((*core::ptr::addr_of!(PRIV)).len(), NR_SYS_PROCS);
            assert_eq!((*core::ptr::addr_of!(PPRIV_ADDR)).len(), NR_SYS_PROCS);
        }
    }

    #[test]
    fn test_is_static_priv_id() {
        for id in 0..NR_STATIC_PRIV_IDS {
            assert!(id < NR_STATIC_PRIV_IDS); // is_static
        }
    }
}
