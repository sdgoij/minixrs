//! System profiling — adapted from `minix/kernel/profile.c`
//!
//! Statistical profiling (SPROFILE): sampling-based profiling using a
//! dedicated clock interrupt. Call profiling (CPROFILE): hash table-based
//! call path profiling for kernel-space processes.
//!
//! The arch-specific parts (clock init/stop, interrupt handler
//! registration, NMI handling) are stubs pending the interrupt subsystem.

use crate::r#priv::PrivFlags;
use crate::proc::*;

// ─────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────

/// Sample buffer size for statistical profiling.
pub const SAMPLE_BUFFER_SIZE: usize = 64 * 1024 * 1024; // 64 MB — matches C (64 << 20)

/// Call profiling table sizes.
pub const CPROF_TABLE_SIZE_OTHER: usize = 3000;
pub const CPROF_TABLE_SIZE_KERNEL: usize = 1500;
pub const CPROF_CPATH_MAX_LEN: usize = 256;
pub const CPROF_INDEX_SIZE: usize = 10 * 1024;
pub const CPROF_STACK_SIZE: usize = 24;
pub const CPROF_PROCNAME_LEN: usize = 8;

/// Call profiling announce values.
pub const CPROF_ANNOUNCE_OTHER: usize = 1;
pub const CPROF_ACCOUNCE_KERNEL: usize = 10000;

/// Call profiling error flags.
pub const CPROF_CPATH_OVERRUN: u32 = 0x1;
pub const CPROF_STACK_OVERRUN: u32 = 0x2;
pub const CPROF_TABLE_OVERRUN: u32 = 0x4;

/// Profiling action commands.
pub const PROF_START: i32 = 0;
pub const PROF_STOP: i32 = 1;
pub const PROF_GET: i32 = 2;
pub const PROF_RESET: i32 = 3;

/// Profiling clock types.
pub const PROF_RTC: i32 = 0;
pub const PROF_NMI: i32 = 1;

// ─────────────────────────────────────────────────────────────────────────
// Statistical profiling state
// ─────────────────────────────────────────────────────────────────────────

/// Whether statistical profiling is active.
pub static mut SPROFILING: bool = false;

/// Statistical profiling info.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct SprofInfo {
    pub mem_used: i32,
    pub total_samples: i32,
    pub idle_samples: i32,
    pub system_samples: i32,
    pub user_samples: i32,
}

/// Global profiling info.
pub static mut SPROF_INFO: SprofInfo = SprofInfo {
    mem_used: 0,
    total_samples: 0,
    idle_samples: 0,
    system_samples: 0,
    user_samples: 0,
};

/// Sample buffer for statistical profiling.
pub static mut SPROF_SAMPLE_BUFFER: [u8; SAMPLE_BUFFER_SIZE] = [0u8; SAMPLE_BUFFER_SIZE];

/// Profiling memory size (set by userspace via sys_sprofile).
pub static mut SPROF_MEM_SIZE: usize = 0;

// ─────────────────────────────────────────────────────────────────────────
// Statistical profiling data types
// ─────────────────────────────────────────────────────────────────────────

/// A single profiling sample (endpoint + program counter).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SprofSample {
    pub proc_: i32,
    pub pc: u64,
}

/// A profiling process record.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SprofProc {
    pub proc_: i32,
    pub name: [u8; crate::proc::PROC_NAME_LEN],
}

// ─────────────────────────────────────────────────────────────────────────
// Call profiling state
// ─────────────────────────────────────────────────────────────────────────

/// Number of call profiling processes registered.
pub static mut CPROF_PROCS_NO: usize = 0;

// ─────────────────────────────────────────────────────────────────────────
// Call profiling data types
// ─────────────────────────────────────────────────────────────────────────

/// Call profiling info.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct CprofInfo {
    pub mem_used: i32,
    pub err: i32,
}

/// Call profiling control structure (per process).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct CprofCtl {
    pub reset: i32,
    pub slots_used: i32,
    pub err: i32,
}

/// Call profiling table entry.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct CprofTbl {
    pub next: *mut CprofTbl,
    pub cpath: [u8; CPROF_CPATH_MAX_LEN],
    pub calls: i32,
    pub cycles: u64,
}

impl Default for CprofTbl {
    fn default() -> Self {
        Self {
            next: core::ptr::null_mut(),
            cpath: [0u8; CPROF_CPATH_MAX_LEN],
            calls: 0,
            cycles: 0,
        }
    }
}

/// Kernel call profiling table.
pub static mut CPROF_TBL: [CprofTbl; CPROF_TABLE_SIZE_KERNEL] = [CprofTbl {
    next: core::ptr::null_mut(),
    cpath: [0u8; CPROF_CPATH_MAX_LEN],
    calls: 0,
    cycles: 0,
}; CPROF_TABLE_SIZE_KERNEL];

/// Per-process profiling info entry.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct CprofProcInfo {
    pub endpt: i32,
    pub name: *mut u8,
    pub ctl_v: u64,
    pub buf_v: u64,
}

/// Array of registered profiling processes.
pub static mut CPROF_PROC_INFO: [CprofProcInfo; 64] = [CprofProcInfo {
    endpt: 0,
    name: core::ptr::null_mut(),
    ctl_v: 0,
    buf_v: 0,
}; 64];

// ─────────────────────────────────────────────────────────────────────────
// sprofile — start/stop statistical profiling
// ─────────────────────────────────────────────────────────────────────────

/// Start or stop statistical profiling.
///
/// # Safety
///
/// Must be called from a safe context (no concurrent profiling ops).
pub unsafe fn sprofile(
    action: i32,
    _size: i32,
    _freq: i32,
    _typ: i32,
    _ctl_ptr: *mut core::ffi::c_void,
    _mem_ptr: *mut core::ffi::c_void,
) -> i32 {
    unsafe {
        match action {
            PROF_START => {
                SPROF_INFO = SprofInfo::default();
                SPROFILING = true;
                crate::system::OK
            }
            PROF_STOP => {
                SPROFILING = false;
                crate::system::OK
            }
            _ => crate::system::EBADREQUEST,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Profile clock (skeletons — arch-specific)
// ─────────────────────────────────────────────────────────────────────────

/// Initialize the profiling clock.
///
/// Programs the RTC to generate periodic interrupts at `freq` Hz,
/// then registers a handler that calls `profile_sample()` on each tick.
///
/// # Safety
///
/// Must be called after interrupt system initialization.
pub unsafe fn init_profile_clock(freq: u32) {
    unsafe {
        // Convert Hz to RTC rate select code.
        // RTC rate = 32768 >> (rate_select - 1) Hz, so:
        //   2 Hz  → rate 15 (32768 >> 14)
        //   4 Hz  → rate 14 (32768 >> 13)
        //   8 Hz  → rate 13
        //   16 Hz → rate 12
        //   32 Hz → rate 11
        //   64 Hz → rate 10
        //   128 Hz → rate 9
        //   256 Hz → rate 8
        //   512 Hz → rate 7
        //   1024 Hz → rate 6
        //   2048 Hz → rate 5
        //   4096 Hz → rate 4
        //   8192 Hz → rate 3
        let rate_code = match freq {
            2 => 15,
            4 => 14,
            8 => 13,
            16 => 12,
            32 => 11,
            64 => 10,
            128 => 9,
            256 => 8,
            512 => 7,
            1024 => 6,
            2048 => 5,
            4096 => 4,
            8192 => 3,
            _ => 6, // default to 1024 Hz
        };

        let irq = arch_x86_64::apic::arch_init_profile_clock(rate_code);
        if irq >= 0 {
            // The handler will be registered in the IDT entry.
            // TODO: Phase 12.15 follow-up — register profile_clock_isr_entry
            // in the IDT at vector VECTOR_TIMER + irq, and call
            // set_profile_clock_handler with the Rust callback that
            // calls profile_sample(current_proc, pc).
            let _ = irq;
        }
    }
}

/// Stop the profiling clock.
///
/// Disables RTC periodic interrupts.
pub fn stop_profile_clock() {
    unsafe {
        arch_x86_64::apic::arch_stop_profile_clock();
    }
}

// ─────────────────────────────────────────────────────────────────────────
// sprof_save_sample / sprof_save_proc / profile_sample
// ─────────────────────────────────────────────────────────────────────────

/// Save a profiling sample to the buffer.
///
/// # Safety
///
/// Buffer must have space for the sample.
unsafe fn sprof_save_sample(p: *mut Proc, pc: u64) {
    unsafe {
        let offset = SPROF_INFO.mem_used as usize;
        if offset + size_of::<SprofSample>() > SAMPLE_BUFFER_SIZE {
            SPROF_INFO.mem_used = -1;
            return;
        }
        let buf = core::ptr::addr_of_mut!(SPROF_SAMPLE_BUFFER).cast::<u8>();
        let sample_ptr = buf.add(offset).cast::<SprofSample>();
        (*sample_ptr).proc_ = (*p).p_endpoint;
        (*sample_ptr).pc = pc;
        SPROF_INFO.mem_used = SPROF_INFO
            .mem_used
            .wrapping_add(size_of::<SprofSample>() as i32);
    }
}

/// Save a process record to the buffer.
///
/// # Safety
///
/// Buffer must have space for the record.
unsafe fn sprof_save_proc(p: *mut Proc) {
    unsafe {
        let offset = SPROF_INFO.mem_used as usize;
        if offset + size_of::<SprofProc>() > SAMPLE_BUFFER_SIZE {
            SPROF_INFO.mem_used = -1;
            return;
        }
        let buf = core::ptr::addr_of_mut!(SPROF_SAMPLE_BUFFER).cast::<u8>();
        let proc_ptr = buf.add(offset).cast::<SprofProc>();
        (*proc_ptr).proc_ = (*p).p_endpoint;
        // Copy name
        for (i, &c) in (*p).p_name.iter().enumerate() {
            if i >= PROC_NAME_LEN - 1 || c == 0 {
                break;
            }
            (*proc_ptr).name[i] = c as u8;
        }
        SPROF_INFO.mem_used = SPROF_INFO
            .mem_used
            .wrapping_add(size_of::<SprofProc>() as i32);
    }
}

/// Record a profiling sample for process `p` at program counter `pc`.
///
/// # Safety
///
/// `p` must point to a valid `Proc`.
pub unsafe fn profile_sample(p: *mut Proc, pc: u64) {
    unsafe {
        if !SPROFILING || SPROF_INFO.mem_used == -1 {
            return;
        }

        // Check memory space
        let needed =
            size_of::<SprofInfo>() + 2 * size_of::<SprofSample>() + 2 * size_of::<SprofSample>();
        if (SPROF_INFO.mem_used as usize) + needed > SPROF_MEM_SIZE {
            SPROF_INFO.mem_used = -1;
            return;
        }

        let ep = (*p).p_endpoint;

        if ep == -4 {
            // IDLE
            SPROF_INFO.idle_samples = SPROF_INFO.idle_samples.wrapping_add(1);
        } else if ep == -2 || {
            // KERNEL or system process
            let is_sys =
                !(*p).p_priv.is_null() && (*(*p).p_priv).s_flags.contains(PrivFlags::SYS_PROC);
            is_sys && (*p).is_runnable()
        } {
            if !(*p).mf_isset(MiscFlags::SPROF_SEEN) {
                (*p).p_misc_flags.fetch_or(
                    MiscFlags::SPROF_SEEN.bits(),
                    core::sync::atomic::Ordering::Relaxed,
                );
                sprof_save_proc(p);
            }
            sprof_save_sample(p, pc);
            SPROF_INFO.system_samples = SPROF_INFO.system_samples.wrapping_add(1);
        } else {
            // User process
            SPROF_INFO.user_samples = SPROF_INFO.user_samples.wrapping_add(1);
        }

        SPROF_INFO.total_samples = SPROF_INFO.total_samples.wrapping_add(1);
    }
}

/// NMI statistical profiling handler.
///
/// Called when the APIC LVT timer is configured in NMI delivery mode.
/// Records a profiling sample for the current process at `frame_pc`.
///
/// # Safety
///
/// Called from NMI context. Must be re-entrant safe.
pub unsafe fn nmi_sprofile_handler(frame_pc: u64) {
    unsafe {
        let proc = crate::ipc::current_proc();
        if !proc.is_null() {
            profile_sample(proc, frame_pc);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Call profiling
// ─────────────────────────────────────────────────────────────────────────

/// Return the kernel's call profiling table size.
pub fn profile_get_tbl_size() -> i32 {
    CPROF_TABLE_SIZE_KERNEL as i32
}

/// Return the announce value for kernel-space processes.
pub fn profile_get_announce() -> i32 {
    CPROF_ACCOUNCE_KERNEL as i32
}

/// Register a process's call profiling control struct and table.
///
/// # Safety
///
/// Pointers must be valid and remain valid while profiling is active.
pub unsafe fn profile_register(ctl_ptr: *mut core::ffi::c_void, tbl_ptr: *mut core::ffi::c_void) {
    unsafe {
        let count = core::ptr::addr_of_mut!(CPROF_PROCS_NO);
        let idx = *count;
        if idx >= 64 {
            return;
        }
        // Get the SYSTEM process
        let rp = crate::table::proc_addr(-2);
        if rp.is_null() {
            return;
        }
        let info = core::ptr::addr_of_mut!(CPROF_PROC_INFO).cast::<CprofProcInfo>();
        (*info.add(idx)).endpt = (*rp).p_endpoint;
        (*info.add(idx)).name = (*rp).p_name.as_mut_ptr().cast::<u8>();
        (*info.add(idx)).ctl_v = ctl_ptr as u64;
        (*info.add(idx)).buf_v = tbl_ptr as u64;
        *count += 1;
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sprof_info_default() {
        let info = SprofInfo::default();
        assert_eq!(info.mem_used, 0);
        assert_eq!(info.total_samples, 0);
    }

    #[test]
    fn test_sprof_sample_layout() {
        assert_eq!(size_of::<SprofSample>(), 16); // i32 + padding + u64
    }

    #[test]
    fn test_sprof_proc_layout() {
        assert_eq!(size_of::<SprofProc>(), 20); // i32 + [u8; 16]
    }

    #[test]
    fn test_cprof_tbl_layout() {
        let tbl = CprofTbl::default();
        assert!(tbl.next.is_null());
        assert_eq!(tbl.calls, 0);
    }

    #[test]
    fn test_cprof_constants() {
        assert_eq!(CPROF_TABLE_SIZE_KERNEL, 1500);
        assert_eq!(CPROF_CPATH_MAX_LEN, 256);
        assert_eq!(PROF_START, 0);
        assert_eq!(PROF_STOP, 1);
        assert_eq!(PROF_RTC, 0);
        assert_eq!(PROF_NMI, 1);
    }

    #[test]
    fn test_profile_get_tbl_size() {
        assert_eq!(profile_get_tbl_size(), 1500);
    }

    #[test]
    fn test_profile_get_announce() {
        assert_eq!(profile_get_announce(), 10000);
    }

    #[test]
    fn test_sprofile_start_stop() {
        unsafe {
            assert_eq!(
                sprofile(
                    PROF_START,
                    0,
                    0,
                    0,
                    core::ptr::null_mut(),
                    core::ptr::null_mut()
                ),
                0
            );
            assert!(SPROFILING);
            assert_eq!(
                sprofile(
                    PROF_STOP,
                    0,
                    0,
                    0,
                    core::ptr::null_mut(),
                    core::ptr::null_mut()
                ),
                0
            );
            assert!(!SPROFILING);
        }
    }

    #[test]
    fn test_sprofile_invalid_action() {
        unsafe {
            assert_eq!(
                sprofile(999, 0, 0, 0, core::ptr::null_mut(), core::ptr::null_mut()),
                -212
            );
        }
    }

    #[test]
    fn test_cprof_proc_info_default() {
        let info = CprofProcInfo::default();
        assert_eq!(info.endpt, 0);
        assert!(info.name.is_null());
    }
}
