//! Process table and scheduling types — adapted from `minix/kernel/proc.h`
//!
//! Defines the `Proc` struct, runtime flags (`RtsFlags`), misc flags
//! (`MiscFlags`), and supporting types used throughout the kernel.
//!
//! **x86_64 differences from i386:**
//! - `p_reg` uses `TrapFrame` (x86_64 184-byte frame) instead of i386
//!   `stackframe_s` (76 bytes)
//! - `SegFrame.p_cr3` is `u64` (not `u32`), `*mut u64` for `p_cr3_v`
//! - `endpoint_t = i32` matches C `int`
//! - `message` is an opaque 64-byte buffer (C `sizeof(message)` = 56 on
//!   i386; 64 bytes rounds up for x86_64 alignment)

use core::sync::atomic::AtomicU32;

use crate::hal;

// ─────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────

pub const NR_TASKS: usize = 5;
pub const NR_PROCS: usize = 256;
pub const NR_SYS_PROCS: usize = 64;
pub const NR_PROCS_TOTAL: usize = NR_TASKS + NR_PROCS;

pub const PROC_NAME_LEN: usize = 16;

/// Magic number for live process slots.
pub const PMAGIC: u32 = 0xC0FFEE1;

/// Number of scheduling priority queues (matches cpulocals).
pub const NR_SCHED_QUEUES: usize = 16;

/// Size of an opaque IPC message buffer.
/// C `sizeof(message)` = 56 on i386; we use 64 for x86_64 alignment.
pub const MESSAGE_SIZE: usize = 64;

// ─────────────────────────────────────────────────────────────────────────
// ProcVmrequest
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ProcVmrequest {
    /// Next in vmrestart chain.
    pub nextrestart: *mut Proc,
    /// Next in vmrequest chain.
    pub nextrequestor: *mut Proc,
    /// Suspended operation type.
    pub vmstype: Vmstype,
    /// Suspended request message.
    pub reqmsg: [u8; MESSAGE_SIZE],
    /// Request type to VM.
    pub req_type: i32,
    /// Target endpoint.
    pub target: i32,
    /// Check range start.
    pub check_start: u64,
    /// Check range length.
    pub check_length: u64,
    /// Nonzero for write access.
    pub check_writeflag: u8,
    _pad: [u8; 7],
    /// VM result when available.
    pub vmresult: i32,
}

/// VM suspend operation types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Vmstype {
    None = 0,
    Kernelcall = 1,
    Delivermsg = 2,
    Map = 3,
}

impl Default for ProcVmrequest {
    fn default() -> Self {
        Self {
            nextrestart: core::ptr::null_mut(),
            nextrequestor: core::ptr::null_mut(),
            vmstype: Vmstype::None,
            reqmsg: [0u8; MESSAGE_SIZE],
            req_type: 0,
            target: 0,
            check_start: 0,
            check_length: 0,
            check_writeflag: 0,
            _pad: [0u8; 7],
            vmresult: 0,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// ProcAccounting
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ProcAccounting {
    /// Time when enqueued (cycles).
    pub enter_queue: u64,
    /// Time spent in queue.
    pub time_in_queue: u64,
    /// Number of dequeues.
    pub dequeues: u32,
    /// Number of synchronous IPC operations.
    pub ipc_sync: u32,
    /// Number of asynchronous IPC operations.
    pub ipc_async: u32,
    /// Number of times preempted.
    pub preempted: u32,
}

// ─────────────────────────────────────────────────────────────────────────
// SegFrame
// ─────────────────────────────────────────────────────────────────────────

/// Segment frame — per-process page table and FPU state.
///
/// Adapted from i386 `archtypes.h` `struct segframe`:
/// - `p_cr3` is `u64` on x86_64 (vs `reg_t` = `u32` on i386)
/// - `p_cr3_v` is `*mut u64` (page table root virtual address)
/// - `fpu_state` is `*mut u8` (heap-allocated FXSAVE area)
/// - `p_kern_trap_style` matches C `int`
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct SegFrame {
    /// Physical address of the per-process page table root.
    pub p_cr3: u64,
    /// Virtual address of the per-process page table root.
    pub p_cr3_v: *mut u64,
    /// Pointer to FPU save area (FXSAVE/FXRSTOR, 512 bytes).
    pub fpu_state: *mut u8,
    /// Kernel trap style (0 = standard syscall, 1 = extended etc.).
    pub p_kern_trap_style: i32,
}

// ─────────────────────────────────────────────────────────────────────────
// RtsFlags (Runtime Flags)
// ─────────────────────────────────────────────────────────────────────────

bitflags::bitflags! {
    /// Runtime flags for a process. A process is runnable iff `rts_flags == 0`.
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct RtsFlags: u32 {
        /// Process slot is free.
        const SLOT_FREE      = 0x00001;
        /// Process has been stopped.
        const PROC_STOP      = 0x00002;
        /// Process blocked trying to send.
        const SENDING        = 0x00004;
        /// Process blocked trying to receive.
        const RECEIVING      = 0x00008;
        /// Set when new kernel signal arrives.
        const SIGNALED       = 0x00010;
        /// Unready while signal being processed.
        const SIG_PENDING    = 0x00020;
        /// Set when process is being traced.
        const P_STOP         = 0x00040;
        /// Keep forked system process from running.
        const NO_PRIV        = 0x00080;
        /// Process cannot send or receive messages.
        const NO_ENDPOINT    = 0x00100;
        /// Not scheduled until pagetable set by VM.
        const VMINHIBIT      = 0x00200;
        /// Process has unhandled pagefault.
        const PAGEFAULT      = 0x00400;
        /// Originator of VM memory request.
        const VMREQUEST      = 0x00800;
        /// Target of VM memory request.
        const VMREQTARGET    = 0x01000;
        /// Process was preempted by a higher priority process.
        const PREEMPTED      = 0x04000;
        /// Process ran out of its quantum.
        const NO_QUANTUM     = 0x08000;
        /// Not ready until VM has made it.
        const BOOTINHIBIT    = 0x10000;
    }
}

impl Default for RtsFlags {
    fn default() -> Self {
        RtsFlags::empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// MiscFlags
// ─────────────────────────────────────────────────────────────────────────

bitflags::bitflags! {
    /// Misc flags that do not suspend the process.
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct MiscFlags: u32 {
        /// Reply to IPC_REQUEST is pending.
        const REPLY_PEND           = 0x0001;
        /// Process-virtual timer is running.
        const VIRT_TIMER           = 0x0002;
        /// Process-virtual profile timer is running.
        const PROF_TIMER           = 0x0004;
        /// Resume kernel call (was interrupted by VM).
        const KCALL_RESUME         = 0x0008;
        /// Copy message for process before running.
        const DELIVERMSG           = 0x0040;
        /// Send signal when no longer sending.
        const SIG_DELAY            = 0x0080;
        /// Syscall tracing: in a system call now.
        const SC_ACTIVE            = 0x0100;
        /// Syscall tracing: deferred system call.
        const SC_DEFER             = 0x0200;
        /// Syscall tracing: trigger syscall events.
        const SC_TRACE             = 0x0400;
        /// Process already used math; FPU regs are initialized.
        const FPU_INITIALIZED      = 0x1000;
        /// Message of this process is from kernel.
        const SENDING_FROM_KERNEL  = 0x2000;
        /// Don't touch context.
        const CONTEXT_SET          = 0x4000;
        /// Profiling has seen this process.
        const SPROF_SEEN           = 0x8000;
        /// TLB must be flushed before running (SMP).
        const FLUSH_TLB            = 0x10000;
        /// VM miss on async send.
        const SENDA_VM_MISS        = 0x20000;
        /// Single-step process.
        const STEP                 = 0x40000;
    }
}

impl Default for MiscFlags {
    fn default() -> Self {
        MiscFlags::empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Proc
// ─────────────────────────────────────────────────────────────────────────

/// The process table entry — core kernel type.
///
/// Layout matches the C `struct proc` from `proc.h`, adapted for x86_64.
#[derive(Debug)]
#[repr(C)]
pub struct Proc {
    /// Process registers saved in stack frame (arch-specific layout as raw bytes).
    pub p_reg: [u8; 256],
    /// Segment descriptors (page table root, FPU state).
    pub p_seg: SegFrame,
    /// Process number (for fast access).
    pub p_nr: i32,
    /// System privileges structure.
    pub p_priv: *mut crate::r#priv::Priv,
    /// Runtime flags (runnable iff zero).
    pub p_rts_flags: AtomicU32,
    /// Misc flags (do not suspend the process).
    pub p_misc_flags: AtomicU32,

    /// Current process priority.
    pub p_priority: i8,
    _priority_pad: [u8; 7],
    /// Time left to use the CPU (in cycles).
    pub p_cpu_time_left: u64,
    /// Assigned time quantum in ms.
    pub p_quantum_size_ms: u32,
    _quantum_pad: [u8; 4],
    /// Who should get out-of-quantum message.
    pub p_scheduler: *mut Proc,
    /// What CPU the process is running on.
    pub p_cpu: u32,

    /// Accounting statistics passed to the scheduler.
    pub p_accounting: ProcAccounting,

    /// User time in ticks.
    pub p_user_time: u64,
    /// System time in ticks.
    pub p_sys_time: u64,
    /// Virtual timer ticks left.
    pub p_virt_left: u64,
    /// Profile timer ticks left.
    pub p_prof_left: u64,

    /// Total cycles used by this process.
    pub p_cycles: u64,
    /// Kernel call cycles caused by this process.
    pub p_kcall_cycles: u64,
    /// IPC cycles caused by this process.
    pub p_kipc_cycles: u64,

    /// Pointer to next ready process.
    pub p_nextready: *mut Proc,
    /// Head of list of processes wishing to send.
    pub p_caller_q: *mut Proc,
    /// Link to next process wishing to send.
    pub p_q_link: *mut Proc,
    /// From whom does process want to receive?
    pub p_getfrom_e: i32,
    /// To whom does process want to send?
    pub p_sendto_e: i32,

    /// Bit map for pending kernel signals.
    pub p_pending: u32,

    /// Process name (null-terminated).
    pub p_name: [u8; PROC_NAME_LEN],

    /// Endpoint number (generation-aware).
    pub p_endpoint: i32,

    /// Message from this process if SENDING.
    pub p_sendmsg: [u8; MESSAGE_SIZE],
    /// Message for this process if MF_DELIVERMSG.
    pub p_delivermsg: [u8; MESSAGE_SIZE],
    /// Virtual address this process wants message at.
    pub p_delivermsg_vir: u64,

    /// VM request state.
    pub p_vmrequest: ProcVmrequest,

    /// Consistency checking variable.
    pub p_found: i32,
    /// Magic number for pointer validation.
    pub p_magic: u32,

    /// Deferred syscall arguments (if MF_SC_DEFER).
    pub p_defer_r1: u64,
    pub p_defer_r2: u64,
    pub p_defer_r3: u64,

    /// Signal received bitmap.
    pub p_signal_received: u64,

    /// Saved per-process CR3 value (Phase 6.5.1).
    /// Set by the arch-level trap handler on syscall entry, before loading
    /// BOOT_CR3. Restored on syscall return so the process runs in its
    /// private address space.
    /// Zero means the process has no per-process page table (uses BOOT_CR3).
    pub p_cr3_saved: u64,
}

impl Default for Proc {
    fn default() -> Self {
        Self {
            p_reg: hal::frame_default(),
            p_seg: SegFrame::default(),
            p_nr: 0,
            p_priv: core::ptr::null_mut(),
            p_rts_flags: AtomicU32::new(RtsFlags::empty().bits()),
            p_misc_flags: AtomicU32::new(MiscFlags::empty().bits()),
            p_priority: 0,
            _priority_pad: [0u8; 7],
            p_cpu_time_left: 0,
            p_quantum_size_ms: 0,
            _quantum_pad: [0u8; 4],
            p_scheduler: core::ptr::null_mut(),
            p_cpu: 0,
            p_accounting: ProcAccounting::default(),
            p_user_time: 0,
            p_sys_time: 0,
            p_virt_left: 0,
            p_prof_left: 0,
            p_cycles: 0,
            p_kcall_cycles: 0,
            p_kipc_cycles: 0,
            p_nextready: core::ptr::null_mut(),
            p_caller_q: core::ptr::null_mut(),
            p_q_link: core::ptr::null_mut(),
            p_getfrom_e: 0,
            p_sendto_e: 0,
            p_pending: 0,
            p_name: [0u8; PROC_NAME_LEN],
            p_endpoint: 0,
            p_sendmsg: [0u8; MESSAGE_SIZE],
            p_delivermsg: [0u8; MESSAGE_SIZE],
            p_delivermsg_vir: 0,
            p_vmrequest: ProcVmrequest::default(),
            p_found: 0,
            p_magic: PMAGIC,
            p_defer_r1: 0,
            p_defer_r2: 0,
            p_defer_r3: 0,
            p_signal_received: 0,
            p_cr3_saved: 0,
        }
    }
}

impl Proc {
    /// Check if this process is runnable (`p_rts_flags == 0`).
    pub fn is_runnable(&self) -> bool {
        self.p_rts_flags.load(core::sync::atomic::Ordering::Relaxed) == 0
    }

    /// Quick check: is this a valid process pointer?
    pub fn ptr_ok(&self) -> bool {
        self.p_magic == PMAGIC
    }

    /// Check if the process was preempted.
    pub fn is_preempted(&self) -> bool {
        self.rts_isset(RtsFlags::PREEMPTED)
    }

    /// Check if the process has no quantum left.
    pub fn no_quantum(&self) -> bool {
        self.rts_isset(RtsFlags::NO_QUANTUM)
    }

    /// Check if the process has used the FPU.
    pub fn used_fpu(&self) -> bool {
        self.mf_isset(MiscFlags::FPU_INITIALIZED)
    }

    /// Check if scheduled by the kernel's default policy
    /// (scheduler is NULL or self).
    pub fn kernel_scheduler(&self) -> bool {
        self.p_scheduler.is_null() || core::ptr::eq(self.p_scheduler, self as *const _ as *mut Proc)
    }

    /// Get the process number.
    pub fn proc_nr(&self) -> i32 {
        self.p_nr
    }

    /// Set the magic number for pointer validation.
    pub fn set_magic(&mut self) {
        self.p_magic = PMAGIC;
    }

    /// Clear the magic number (slot free).
    pub fn clear_magic(&mut self) {
        self.p_magic = 0;
    }

    /// Test if a specific runtime flag is set.
    pub fn rts_isset(&self, flag: RtsFlags) -> bool {
        let bits = self.p_rts_flags.load(core::sync::atomic::Ordering::Relaxed);
        (bits & flag.bits()) == flag.bits()
    }

    /// Test if a specific misc flag is set.
    pub fn mf_isset(&self, flag: MiscFlags) -> bool {
        let bits = self
            .p_misc_flags
            .load(core::sync::atomic::Ordering::Relaxed);
        (bits & flag.bits()) == flag.bits()
    }

    /// What process is this process blocked on?
    /// Returns endpoint number (can be ANY) or NONE.
    /// Must check RTS_SENDING first, then RTS_RECEIVING.
    pub fn blocked_on(&self) -> i32 {
        let rts = self.p_rts_flags.load(core::sync::atomic::Ordering::Relaxed);
        if rts & RtsFlags::SENDING.bits() != 0 {
            self.p_sendto_e
        } else if rts & RtsFlags::RECEIVING.bits() != 0 {
            self.p_getfrom_e
        } else {
            -1 // NONE
        }
    }

    /// Check if the process slot is free.
    pub fn is_empty(&self) -> bool {
        self.p_rts_flags.load(core::sync::atomic::Ordering::Relaxed) == RtsFlags::SLOT_FREE.bits()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Proc must fit within IDLE_PROC_SIZE (1024 bytes) from cpulocals.
    #[test]
    fn test_proc_size_within_idle_proc() {
        assert!(
            size_of::<Proc>() <= 1024,
            "Proc size {} exceeds IDLE_PROC_SIZE 1024",
            size_of::<Proc>()
        );
    }

    #[test]
    fn test_default_proc_has_magic() {
        let p = Proc::default();
        assert!(p.ptr_ok());
        assert_eq!(p.p_magic, PMAGIC);
    }

    #[test]
    fn test_default_proc_is_runnable() {
        let p = Proc::default();
        assert!(p.is_runnable());
    }

    #[test]
    fn test_default_proc_not_preempted() {
        let p = Proc::default();
        assert!(!p.is_preempted());
    }

    #[test]
    fn test_default_proc_no_fpu() {
        let p = Proc::default();
        assert!(!p.used_fpu());
    }

    #[test]
    fn test_default_proc_kernel_scheduler() {
        let p = Proc::default();
        assert!(p.kernel_scheduler());
    }

    #[test]
    fn test_proc_nr() {
        let p = Proc::default();
        assert_eq!(p.proc_nr(), 0);
    }

    #[test]
    fn test_default_proc_cr3_saved_is_zero() {
        let p = Proc::default();
        assert_eq!(p.p_cr3_saved, 0, "p_cr3_saved should init to 0");
    }

    #[test]
    fn test_rts_isset() {
        let p = Proc::default();
        p.p_rts_flags.store(
            RtsFlags::SENDING.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
        assert!(p.rts_isset(RtsFlags::SENDING));
        assert!(!p.rts_isset(RtsFlags::RECEIVING));
    }

    #[test]
    fn test_mf_isset() {
        let p = Proc::default();
        p.p_misc_flags.store(
            MiscFlags::FPU_INITIALIZED.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
        assert!(p.mf_isset(MiscFlags::FPU_INITIALIZED));
        assert!(!p.mf_isset(MiscFlags::REPLY_PEND));
    }

    #[test]
    fn test_blocked_on_sending() {
        let p = Proc {
            p_sendto_e: 7,
            ..Default::default()
        };
        p.p_rts_flags.store(
            RtsFlags::SENDING.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
        assert_eq!(p.blocked_on(), 7);
    }

    #[test]
    fn test_blocked_on_receiving() {
        let p = Proc {
            p_getfrom_e: 42,
            ..Default::default()
        };
        p.p_rts_flags.store(
            RtsFlags::RECEIVING.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
        assert_eq!(p.blocked_on(), 42);
    }

    #[test]
    fn test_blocked_on_none() {
        let p = Proc::default();
        assert_eq!(p.blocked_on(), -1);
    }

    #[test]
    fn test_is_empty() {
        let p = Proc::default();
        assert!(!p.is_empty());

        p.p_rts_flags.store(
            RtsFlags::SLOT_FREE.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
        assert!(p.is_empty());
    }

    #[test]
    fn test_clear_magic_invalidates() {
        let mut p = Proc::default();
        p.clear_magic();
        assert!(!p.ptr_ok());
    }

    #[test]
    fn test_proc_accounting_default() {
        let a = ProcAccounting::default();
        assert_eq!(a.enter_queue, 0);
        assert_eq!(a.dequeues, 0);
        assert_eq!(a.preempted, 0);
    }

    #[test]
    fn test_seg_frame_default() {
        let s = SegFrame::default();
        assert_eq!(s.p_cr3, 0);
        assert!(s.p_cr3_v.is_null());
        assert!(s.fpu_state.is_null());
    }

    #[test]
    fn test_vmstype_values() {
        assert_eq!(Vmstype::None as i32, 0);
        assert_eq!(Vmstype::Kernelcall as i32, 1);
        assert_eq!(Vmstype::Delivermsg as i32, 2);
        assert_eq!(Vmstype::Map as i32, 3);
    }

    #[test]
    fn test_rts_flag_values() {
        assert_eq!(RtsFlags::SLOT_FREE.bits(), 0x00001);
        assert_eq!(RtsFlags::SENDING.bits(), 0x00004);
        assert_eq!(RtsFlags::BOOTINHIBIT.bits(), 0x10000);
    }

    #[test]
    fn test_mf_flag_values() {
        assert_eq!(MiscFlags::REPLY_PEND.bits(), 0x0001);
        assert_eq!(MiscFlags::FPU_INITIALIZED.bits(), 0x1000);
        assert_eq!(MiscFlags::STEP.bits(), 0x40000);
    }

    #[test]
    fn test_proc_size_reasonable() {
        // Should be at least a few hundred bytes
        assert!(size_of::<Proc>() > 200);
        // Must not exceed IDLE_PROC_SIZE
        assert!(size_of::<Proc>() <= 1024);
    }

    #[test]
    fn test_constants() {
        assert_eq!(NR_TASKS, 5);
        assert_eq!(NR_PROCS, 256);
        assert_eq!(PROC_NAME_LEN, 16);
        assert_eq!(PMAGIC, 0xC0FFEE1);
    }

    #[test]
    fn test_sending_overrides_receiving_blocked_on() {
        // When both SENDING and RECEIVING are set, sending takes priority
        let p = Proc {
            p_sendto_e: 10,
            p_getfrom_e: 20,
            ..Default::default()
        };
        p.p_rts_flags.store(
            RtsFlags::SENDING.bits() | RtsFlags::RECEIVING.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
        assert_eq!(p.blocked_on(), 10);
    }
}
