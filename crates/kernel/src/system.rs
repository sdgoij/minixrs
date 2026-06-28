//! System call dispatch infrastructure — adapted from `minix/kernel/system.c`
//!
//! Defines the kernel call dispatch table (`call_vec`), system
//! initialization, privilege management, signal delivery skeletons,
//! and IPC cleanup functions.
//!
//! **x86_64 differences from i386:**
//! - All vir_bytes are u64 (not u32)
//! - No i386-specific syscalls (SYS_DEVIO, SYS_SDEVIO, SYS_VDEVIO,
//!   SYS_READBIOS, SYS_IOPENABLE) — omitted from call_vec
//! - message copy uses raw pointer copy (no segmentation)

use core::sync::atomic::Ordering;

use arch_common::consts::{SEGMENT_INDEX, VM_GRANT};
use arch_common::endpoint::ANY;

use crate::r#priv::*;
use crate::proc::*;
use crate::sched::dequeue;
use crate::table;
use crate::table::proc_addr;

// ─────────────────────────────────────────────────────────────────────────
// Message field offset helpers
// ─────────────────────────────────────────────────────────────────────────
// These offsets match the C `mess_*` struct layouts from minix/ipc.h.
// Each message is [u8; MESSAGE_SIZE] with m_type at bytes 0-3.
// The message-specific fields follow at the offsets defined below.

/// Read an i32 field from the message at a given byte offset.
unsafe fn msg_read_i32(msg: &[u8; MESSAGE_SIZE], offset: usize) -> i32 {
    i32::from_ne_bytes(msg[offset..offset + 4].try_into().unwrap())
}

/// Write an i32 field to the message at a given byte offset.
unsafe fn msg_write_i32(msg: &mut [u8; MESSAGE_SIZE], offset: usize, val: i32) {
    msg[offset..offset + 4].copy_from_slice(&val.to_ne_bytes());
}

/// Read a u32 field from the message.
unsafe fn msg_read_u32(msg: &[u8; MESSAGE_SIZE], offset: usize) -> u32 {
    u32::from_ne_bytes(msg[offset..offset + 4].try_into().unwrap())
}

/// Read a u64 field from the message.
unsafe fn msg_read_u64(msg: &[u8; MESSAGE_SIZE], offset: usize) -> u64 {
    u64::from_ne_bytes(msg[offset..offset + 8].try_into().unwrap())
}

/// Write a u64 field to the message.
unsafe fn msg_write_u64(msg: &mut [u8; MESSAGE_SIZE], offset: usize, val: u64) {
    msg[offset..offset + 8].copy_from_slice(&val.to_ne_bytes());
}

// ── Message struct offset constants ────────────────────────────────────
//
// mess_lsys_krn_sys_fork (for do_fork):
//   offset 0: endpt (endpoint_t / i32) — parent endpoint
//   offset 4: slot  (endpoint_t / i32) — child slot number
//   offset 8: flags (uint32_t / u32)   — fork flags
//
// mess_lsys_krn_sys_clear (for do_clear):
//   offset 0: endpt (endpoint_t / i32) — endpoint of process to clear
//
// mess_sigcalls (for do_kill):
//   offset  0: map   (sigset_t / u128, 16 bytes)
//   offset 16: endpt (endpoint_t / i32)
//   offset 20: sig   (int / i32)
//   offset 24: sigctx (void* / u64)
//
// mess_lsys_krn_sys_fork (for do_fork):
//   offset 0: endpt (endpoint_t / i32) — parent endpoint
//   offset 4: slot  (endpoint_t / i32) — child slot number
//   offset 8: flags (uint32_t / u32)   — fork flags
//
// mess_lsys_krn_sys_clear (for do_clear):
//   offset 0: endpt (endpoint_t / i32) — endpoint of process to clear
//
// mess_sigcalls (for do_kill, do_getksig, do_endksig):
//   offset  0: map   (sigset_t / u128, 16 bytes)
//   offset 16: endpt (endpoint_t / i32)
//   offset 20: sig   (int / i32)
//   offset 24: sigctx (void* / u64)
//
// mess_krn_lsys_sys_fork (reply for do_fork):
//   offset  0: endpt   (endpoint_t / i32) — child endpoint
//   offset  8: msgaddr (vir_bytes / u64)  — parent's message delivery addr
//
// mess_lsys_krn_sys_times (for do_times):
//   offset 0: endpt (endpoint_t / i32)
//
// mess_krn_lsys_sys_times (reply for do_times):
//   offset  0: real_ticks   (u64)
//   offset  8: boot_ticks   (u64)
//   offset 16: boot_time    (u64)
//   offset 24: user_time    (u64)
//   offset 32: system_time  (u64)
//
// mess_lsys_krn_sys_setalarm (for do_setalarm):
//   offset  0: exp_time   (u64)
//   offset  8: time_left  (u64)
//   offset 16: abs_time   (i32)
//
// mess_lsys_krn_sys_abort (for do_abort):
//   offset 0: how (i32)
//
// mess_lsys_krn_sys_diagctl (for do_diagctl):
//   offset  0: code   (i32)
//   offset  8: buf    (u64 / vir_bytes)
//   offset 16: len    (i32)
//   offset 20: endpt  (i32)
//
// mess_lsys_krn_schedule (for do_schedule):
//   offset  0: endpoint (i32)
//   offset  4: quantum  (i32)
//   offset  8: priority (i32)
//   offset 12: cpu      (i32)
//
// mess_lsys_krn_schedctl (for do_schedctl):
//   offset  0: flags     (u32)
//   offset  4: endpoint  (i32)
//   offset  8: priority  (i32)
//   offset 12: quantum   (i32)
//   offset 16: cpu       (i32)
//
// mess_lsys_krn_sys_statectl (for do_statectl):
//   offset 0: request (i32)
//
// mess_1 (used by do_runctl, do_trace, etc.):
//   offset  0: m1ull1 (u64)
//   offset  8: m1i1   (i32)
//   offset 12: m1i2   (i32)
//   offset 16: m1i3   (i32)
//   offset 24: m1p1   (u64)
//   offset 32: m1p2   (u64)
//   offset 40: m1p3   (u64)
//   offset 48: m1p4   (u64)

// Offset constants
const FORK_ENDPT_OFF: usize = 0;
const FORK_SLOT_OFF: usize = 4;
const FORK_FLAGS_OFF: usize = 8;

const CLEAR_ENDPT_OFF: usize = 0;

const SIGCALLS_MAP_OFF: usize = 0;
const SIGCALLS_ENDPT_OFF: usize = 16;
const SIGCALLS_SIG_OFF: usize = 20;

const FORK_REPLY_ENDPT_OFF: usize = 0;
const FORK_REPLY_MSGADDR_OFF: usize = 8;

const TIMES_ENDPT_OFF: usize = 0;
const TIMES_REPLY_REAL_OFF: usize = 0;
const TIMES_REPLY_BOOTTICKS_OFF: usize = 8;
const TIMES_REPLY_BOOTTIME_OFF: usize = 16;
const TIMES_REPLY_USER_OFF: usize = 24;
const TIMES_REPLY_SYSTEM_OFF: usize = 32;

const ABORT_HOW_OFF: usize = 0;

const DIAGCTL_CODE_OFF: usize = 0;

const SCHEDULE_ENDPT_OFF: usize = 0;
const SCHEDULE_QUANTUM_OFF: usize = 4;
const SCHEDULE_PRIORITY_OFF: usize = 8;
const SCHEDULE_CPU_OFF: usize = 12;

const SCHEDCTL_FLAGS_OFF: usize = 0;
const SCHEDCTL_ENDPT_OFF: usize = 4;
const SCHEDCTL_PRIORITY_OFF: usize = 8;
const SCHEDCTL_QUANTUM_OFF: usize = 12;
const SCHEDCTL_CPU_OFF: usize = 16;

const STATECTL_REQUEST_OFF: usize = 0;

const M1_I1_OFF: usize = 8;
const M1_I2_OFF: usize = 12;
const M1_I3_OFF: usize = 16;

// Phase 6.13: do_setgrant message offset
const M1_P1_OFF: usize = 24;
// M1_P2_OFF = 32, M1_P3_OFF = 40, M1_P4_OFF = 48 — reserved for future use

// mess_lsys_krn_sys_umap (for do_umap, do_umap_remote):
//   offset  0: src_endpt  (endpoint_t / i32)
//   offset  4: segment    (int / i32)
//   offset  8: src_addr   (vir_bytes / u64)
//   offset 16: dst_endpt  (endpoint_t / i32)
//   offset 20: nr_bytes   (int / i32)
// mess_krn_lsys_sys_umap (reply):
//   offset  0: dst_addr   (phys_bytes / u64)
const UMAP_SRC_ENDPT_OFF: usize = 0;
const UMAP_SEGMENT_OFF: usize = 4;
const UMAP_SRC_ADDR_OFF: usize = 8;
const UMAP_DST_ENDPT_OFF: usize = 16;
const UMAP_NR_BYTES_OFF: usize = 20;

// mess_lsys_krn_sys_memset (for do_memset):
//   offset  0: base      (phys_bytes / u64)
//   offset  8: count     (phys_bytes / u64)
//   offset 16: pattern   (unsigned long / u64)
//   offset 24: process   (endpoint_t / i32)
const MEMSET_BASE_OFF: usize = 0;
const MEMSET_COUNT_OFF: usize = 8;
const MEMSET_PATTERN_OFF: usize = 16;
const MEMSET_PROC_OFF: usize = 24;

// mess_lsys_krn_sys_getinfo (for do_getinfo):
//   offset  0: request    (int / i32)
//   offset  4: endpt      (endpoint_t / i32)
//   offset  8: val_ptr    (vir_bytes / u64)
//   offset 16: val_len    (int / i32)
//   offset 24: val_ptr2   (vir_bytes / u64)
//   offset 32: val_len2_e (int / i32)
const GETINFO_REQUEST_OFF: usize = 0;
const GETINFO_VAL_PTR_OFF: usize = 8;
const GETINFO_VAL_LEN_OFF: usize = 16;
const GETINFO_VAL_PTR2_OFF: usize = 24;
const GETINFO_VAL_LEN2_E_OFF: usize = 32;

// mess_krn_lsys_sys_getwhoami (reply for GET_WHOAMI):
//   offset  0: endpt      (endpoint_t / i32)
//   offset  4: privflags  (int / i32)
//   offset  8: name       (char[48])
const WHOAMI_ENDPT_OFF: usize = 0;
const WHOAMI_PRIVFLAGS_OFF: usize = 4;
const WHOAMI_NAME_OFF: usize = 8;

// mess_lsys_krn_sys_setgrant (for do_setgrant):
//   offset  0: addr       (vir_bytes / u64)
//   offset  8: size       (int / i32)
const SETGRANT_ADDR_OFF: usize = 0;
const SETGRANT_SIZE_OFF: usize = 8;

// ─────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────

/// Base for kernel call numbers.
pub const KERNEL_CALL: i32 = 0x600;

/// Error codes.
pub const VMSUSPEND: i32 = -996;
pub const EDONTREPLY: i32 = -203; // pseudo-code: don't send a reply
pub const EBADREQUEST: i32 = -212;
pub const ECALLDENIED: i32 = -210;

/// IRQ hook count (single-CPU).
pub const NR_IRQ_HOOKS: usize = 16;

/// NONE endpoint constant — matches C `_ENDPOINT_SLOT_TOP - 2` (31743).
pub const NONE: i32 = 31743;

/// SELF endpoint constant — matches C `_ENDPOINT_SLOT_TOP - 3` (31742).
pub const SELF: i32 = 31742;
pub const OK: i32 = 0;

/// Maximum signal number (C `_NSIG` on x86_64).
pub const _NSIG: i32 = 128;

/// Fork flag: don't schedule until VM releases (C `PFF_VMINHIBIT`).
pub const PFF_VMINHIBIT: u32 = 0x01;

/// Fork name suffix (C `FORKSTR`).
pub const FORK_STR: &str = "*F";

// ─────────────────────────────────────────────────────────────────────────
// Kernel call billing
// ─────────────────────────────────────────────────────────────────────────

/// Process currently being billed for kernel calls.
pub static mut KBILL_KCALL: *mut Proc = core::ptr::null_mut();
/// Process currently being billed for IPC.
pub static mut KBILL_IPC: *mut Proc = core::ptr::null_mut();

// ─────────────────────────────────────────────────────────────────────────
// Call table
// ─────────────────────────────────────────────────────────────────────────

/// Type for kernel call handlers.
pub type CallHandler = unsafe fn(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32;

/// Kernel call dispatch table.
static mut CALL_VEC: [Option<CallHandler>; NR_SYS_CALLS] = [None; NR_SYS_CALLS];

/// Map a kernel call number to a handler.
///
/// # Safety
///
/// Must be called during system_init(), before any concurrent access.
pub unsafe fn map_call(call_nr: i32, handler: CallHandler) {
    unsafe {
        let idx = call_nr as usize;
        if idx < NR_SYS_CALLS {
            CALL_VEC[idx] = Some(handler);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// IrqHook
// ─────────────────────────────────────────────────────────────────────────

/// Interrupt handler hook.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IrqHook {
    pub next: *mut IrqHook,
    pub handler: Option<unsafe fn(*mut IrqHook) -> i32>,
    pub irq: i32,
    pub id: i32,
    pub proc_nr_e: i32, // endpoint NONE if not in use
    pub notify_id: u64,
    pub policy: u64,
}

impl Default for IrqHook {
    fn default() -> Self {
        Self {
            next: core::ptr::null_mut(),
            handler: None,
            irq: 0,
            id: 0,
            proc_nr_e: NONE,
            notify_id: 0,
            policy: 0,
        }
    }
}

/// Global IRQ hook table.
pub static mut IRQ_HOOKS: [IrqHook; NR_IRQ_HOOKS] = [IrqHook {
    next: core::ptr::null_mut(),
    handler: None,
    irq: 0,
    id: 0,
    proc_nr_e: NONE,
    notify_id: 0,
    policy: 0,
}; NR_IRQ_HOOKS];

/// Active IRQ IDs.
pub static mut IRQ_ACTIDS: [i32; 64] = [0i32; 64];

/// Map of all in-use IRQs.
pub static mut IRQ_USE: i32 = 0;

// ─────────────────────────────────────────────────────────────────────────
// system_init
// ─────────────────────────────────────────────────────────────────────────

/// Initialize the system call infrastructure.
///
/// Initializes IRQ hooks, alarm timers, and the kernel call dispatch
/// table with all supported handlers.
///
/// # Safety
///
/// Must be called exactly once during boot.
pub unsafe fn system_init() {
    unsafe {
        // Initialize IRQ hooks using raw pointer
        let hooks = core::ptr::addr_of_mut!(IRQ_HOOKS);
        for i in 0..NR_IRQ_HOOKS {
            (*hooks)[i].proc_nr_e = NONE;
        }

        // Initialize alarm timers for all privilege structures
        let base = core::ptr::addr_of_mut!(PRIV).cast::<Priv>();
        for i in 0..NR_SYS_PROCS {
            let sp = base.add(i);
            (*sp).s_alarm_timer = MinixTimer::default();
        }

        // Initialize call vector — map known calls
        // Process management (call indices)
        map_call(0, do_fork_handler); // SYS_FORK
        map_call(1, do_exec_stub); // SYS_EXEC — needs data_copy + arch_proc_init, deferred
        map_call(2, do_clear_handler); // SYS_CLEAR
        map_call(3, do_schedule_handler); // SYS_SCHEDULE
        map_call(4, do_privctl_stub); // SYS_PRIVCTL
        map_call(5, do_trace_stub); // SYS_TRACE
        map_call(6, do_kill_handler); // SYS_KILL
        map_call(7, do_getksig_handler); // SYS_GETKSIG
        map_call(8, do_endksig_handler); // SYS_ENDKSIG
        map_call(9, do_sigsend_handler); // SYS_SIGSEND
        map_call(10, do_sigreturn_handler); // SYS_SIGRETURN
        map_call(13, do_memset_handler); // SYS_MEMSET
        map_call(14, do_umap_handler); // SYS_UMAP
        map_call(15, do_vircopy_stub); // SYS_VIRCOPY
        map_call(16, do_physcopy_stub); // SYS_PHYSCOPY
        map_call(17, do_umap_remote_handler); // SYS_UMAP_REMOTE
        map_call(18, do_vumap_stub); // SYS_VUMAP
        map_call(19, do_irqctl_stub); // SYS_IRQCTL
        map_call(24, do_setalarm_stub); // SYS_SETALARM
        map_call(25, do_times_handler); // SYS_TIMES
        map_call(26, do_getinfo_handler); // SYS_GETINFO
        map_call(27, do_abort_handler); // SYS_ABORT
        map_call(31, do_safecopy_from_stub); // SYS_SAFECOPYFROM
        map_call(32, do_safecopy_to_stub); // SYS_SAFECOPYTO
        map_call(33, do_vsafecopy_stub); // SYS_VSAFECOPY
        map_call(34, do_setgrant_handler); // SYS_SETGRANT
        map_call(36, do_sprofile_stub); // SYS_SPROF
        map_call(37, do_cprofile_stub); // SYS_CPROF
        map_call(38, do_profbuf_stub); // SYS_PROFBUF
        map_call(39, do_stime_stub); // SYS_STIME
        map_call(40, do_settime_stub); // SYS_SETTIME
        map_call(43, do_vmctl_handler); // SYS_VMCTL
        map_call(44, do_diagctl_handler); // SYS_DIAGCTL
        map_call(45, do_vtimer_stub); // SYS_VTIMER
        map_call(46, do_runctl_handler); // SYS_RUNCTL
        map_call(50, do_getmcontext_stub); // SYS_GETMCONTEXT
        map_call(51, do_setmcontext_stub); // SYS_SETMCONTEXT
        map_call(52, do_update_stub); // SYS_UPDATE
        map_call(53, do_exit_handler); // SYS_EXIT
        map_call(54, do_schedctl_handler); // SYS_SCHEDCTL
        map_call(55, do_statectl_handler); // SYS_STATECTL
        map_call(56, do_safememset_stub); // SYS_SAFEMEMSET
    }
}

// ─────────────────────────────────────────────────────────────────────────
// kernel_call / kernel_call_dispatch / kernel_call_finish
// ─────────────────────────────────────────────────────────────────────────

/// Entry point for kernel calls.
///
/// Copies the message from user space, dispatches it, and handles
/// the result.
///
/// # Safety
///
/// `caller` must be a valid process pointer; `m_user` must point to
/// a message-sized buffer in the caller's address space.
pub unsafe fn kernel_call(m_user: *mut u8, caller: *mut Proc) {
    unsafe {
        let mut msg = [0u8; MESSAGE_SIZE];
        (*caller).p_delivermsg_vir = m_user as u64;

        // Copy message from user space
        core::ptr::copy_nonoverlapping(m_user, msg.as_mut_ptr(), MESSAGE_SIZE);

        // Set source endpoint
        let msg_source = (*caller).p_endpoint;
        msg[4..8].copy_from_slice(&msg_source.to_ne_bytes());

        let result = kernel_call_dispatch(caller, &mut msg);

        // Remember who invoked the kcall for billing
        KBILL_KCALL = caller;

        kernel_call_finish(caller, &mut msg, result);
    }
}

/// Dispatch a kernel call to its handler.
///
/// # Safety
///
/// `caller` must be a valid process pointer.
pub unsafe fn kernel_call_dispatch(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let call_nr_bytes = [msg[0], msg[1], msg[2], msg[3]];
        let call_nr = i32::from_ne_bytes(call_nr_bytes) - KERNEL_CALL;

        // Check call number validity
        if call_nr < 0 || call_nr >= NR_SYS_CALLS as i32 {
            return EBADREQUEST;
        }

        // Check permission via k_call_mask
        let idx = call_nr as usize;
        let mask = (*(*caller).p_priv).s_k_call_mask;
        let chunk = idx / 32;
        let bit = idx % 32;
        if chunk >= mask.len() || (mask[chunk] & (1u32 << bit)) == 0 {
            return ECALLDENIED;
        }

        // Dispatch
        match CALL_VEC[idx] {
            Some(handler) => handler(caller, msg),
            None => EBADREQUEST,
        }
    }
}

/// Finish a kernel call (handle result or VM suspend).
///
/// # Safety
///
/// `caller` must be a valid process pointer.
pub unsafe fn kernel_call_finish(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE], result: i32) {
    unsafe {
        if result == VMSUSPEND {
            return;
        }

        if result != EDONTREPLY {
            let vir = (*caller).p_delivermsg_vir;
            if vir != 0 {
                core::ptr::copy_nonoverlapping(msg.as_ptr(), vir as *mut u8, MESSAGE_SIZE);
            }
        }
    }
}

/// Resume a kernel call that was suspended for VM.
///
/// # Safety
///
/// `caller` must be a valid process pointer with MF_KCALL_RESUME set.
pub unsafe fn kernel_call_resume(caller: *mut Proc) {
    unsafe {
        // Restore saved request message
        let saved = &(*caller).p_vmrequest.reqmsg;
        let mut msg = *saved;

        let result = kernel_call_dispatch(caller, &mut msg);
        kernel_call_finish(caller, &mut msg, result);

        // Clear VM request state
        (*caller).p_vmrequest.vmstype = crate::proc::Vmstype::None;
    }
}

// ─────────────────────────────────────────────────────────────────────────
// get_priv — privilege structure allocation
// ─────────────────────────────────────────────────────────────────────────

/// Allocate a privilege structure (static or dynamic).
///
/// Returns the index of the allocated privilege structure, or `None`
/// if no slot is available.
///
/// # Safety
///
/// Must be called in a context where PRIV table access is safe.
pub unsafe fn get_priv(rp: *mut Proc) -> Option<usize> {
    unsafe {
        let base = core::ptr::addr_of_mut!(PRIV).cast::<Priv>();

        // Try to allocate a privilege structure
        for i in 0..NR_SYS_PROCS {
            let sp = base.add(i);
            if (*sp).s_proc_nr == NONE {
                // Found a free slot
                (*sp).s_proc_nr = (*rp).p_nr;
                (*sp).s_id = i as i16;
                // Set IPC map to all zeros
                let ipc = &mut (*sp).s_ipc_to;
                for chunk in ipc.chunk.iter_mut() {
                    *chunk = 0;
                }
                (*rp).p_priv = sp;
                return Some(i);
            }
        }
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────
// IPC send bit manipulation
// ─────────────────────────────────────────────────────────────────────────

/// Set a send-to bit for a process.
///
/// # Safety
///
/// `rc` must have a valid privilege structure.
pub unsafe fn set_sendto_bit(rc: &Proc, id: usize) {
    if let Some(p) = unsafe { rc.p_priv.as_mut() } {
        p.s_ipc_to.set(id);
    }
}

/// Unset a send-to bit for a process.
///
/// # Safety
///
/// `rc` must have a valid privilege structure.
pub unsafe fn unset_sendto_bit(rc: &Proc, id: usize) {
    if let Some(p) = unsafe { rc.p_priv.as_mut() } {
        p.s_ipc_to.clear(id);
    }
}

/// Fill the send-to mask for a process.
///
/// # Safety
///
/// `rc` must have a valid privilege structure.
pub unsafe fn fill_sendto_mask(rc: &Proc, map: &SysMap) {
    if let Some(p) = unsafe { rc.p_priv.as_mut() } {
        p.s_ipc_to = *map;
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Signal delivery (skeletons)
// ─────────────────────────────────────────────────────────────────────────

/// Send a signal to a process using the C path: set `s_sig_pending` in
/// the priv structure and notify SYSTEM if the process is not a system process.
///
/// Returns `OK` if the signal was registered, `EBADREQUEST` if the process
/// is invalid, `EPERM` if the process has no priv structure.
///
/// # Safety
///
/// Target process must be valid.
pub unsafe fn send_sig(proc_nr: i32, sig_nr: i32) -> i32 {
    unsafe {
        let rp = proc_addr(proc_nr);
        if rp.is_null() || (*rp).is_empty() {
            return EBADREQUEST;
        }
        if !(0..128).contains(&sig_nr) {
            return crate::ipc::EFAULT;
        }

        let priv_data = (*rp).p_priv;
        if priv_data.is_null() {
            return EBADREQUEST;
        }

        // Set the signal bit in the priv structure's pending signals
        (*priv_data).s_sig_pending |= 1u128 << sig_nr;

        // Set RTS_SIGNALED | RTS_SIG_PENDING, dequeue if was runnable
        let sig_flags = RtsFlags::SIGNALED | RtsFlags::SIG_PENDING;
        let old = (*rp).p_rts_flags.load(Ordering::Relaxed);
        (*rp)
            .p_rts_flags
            .store(old | sig_flags.bits(), Ordering::Relaxed);
        if old == 0 {
            dequeue(rp);
        }

        // Notify SYSTEM unconditionally (C: mini_notify(proc_addr(SYSTEM), rp->p_endpoint))
        // p_signal_received is not incremented here; signal tracking is via RTS flags
        crate::ipc::mini_notify(arch_common::com::SYSTEM, (*rp).p_endpoint);

        OK
    }
}

/// Cause a signal to be delivered to a process.
///
/// Sets `p_pending`, sets RTS_SIGNALED | RTS_SIG_PENDING, dequeues if
/// was runnable, and notifies the signal manager.
///
/// # Safety
///
/// Process must exist.
pub unsafe fn cause_sig(proc_nr: i32, sig_nr: i32) {
    unsafe {
        let rp = proc_addr(proc_nr);
        if rp.is_null() {
            return;
        }

        // Set the signal bit in p_pending
        if (0..32).contains(&sig_nr) {
            (*rp).p_pending |= 1u32 << sig_nr;
        }

        // Mark SIGNALED and SIG_PENDING, dequeue if was runnable
        let flags = RtsFlags::SIGNALED | RtsFlags::SIG_PENDING;
        let old = (*rp).p_rts_flags.load(Ordering::Relaxed);
        (*rp)
            .p_rts_flags
            .store(old | flags.bits(), Ordering::Relaxed);

        if old == 0 {
            dequeue(rp);
        }

        // Only notify on first signal (C: if(!RTS_ISSET(rp, RTS_SIGNALED)))
        if old & RtsFlags::SIGNALED.bits() == 0 && !(*rp).p_priv.is_null() {
            let sig_mgr = (*(*rp).p_priv).s_sig_mgr;
            if sig_mgr != crate::system::NONE {
                // Resolve SELF (C: if(sig_mgr == SELF) sig_mgr = rp->p_endpoint)
                let sig_mgr_ep = if sig_mgr == crate::system::SELF {
                    (*rp).p_endpoint
                } else {
                    sig_mgr
                };
                // Convert endpoint to proc_nr and call send_sig with SIGKSIG (74)
                let slot = crate::table::endpoint_slot(sig_mgr_ep);
                send_sig(slot, 74); // SIGKSIG = 74
            }
        }
    }
}

/// Signal delay done — check if signal can now be delivered.
///
/// # Safety
///
/// Process must be valid.
pub unsafe fn sig_delay_done(rp: *mut Proc) -> bool {
    unsafe {
        let mf = (*rp).p_misc_flags.load(Ordering::Relaxed);
        (mf & MiscFlags::SIG_DELAY.bits()) == 0
    }
}

// ─────────────────────────────────────────────────────────────────────────
// IPC cleanup (skeletons)
// ─────────────────────────────────────────────────────────────────────────

/// Clear IPC state for a process.
///
/// If the process is blocked on SENDING, unlink it from the target's
/// caller queue. Always clears SENDING and RECEIVING flags.
///
/// # Safety
///
/// Process must be valid.
pub unsafe fn clear_ipc(rp: *mut Proc) {
    unsafe {
        let rts = (*rp).p_rts_flags.load(Ordering::Relaxed);

        if rts & RtsFlags::SENDING.bits() != 0 {
            // Walk the target's caller queue to unlink this process
            let target_ep = (*rp).p_sendto_e;
            if crate::table::is_ok_endpoint(target_ep) {
                let target_slot = crate::table::endpoint_slot(target_ep);
                let target = crate::table::proc_addr(target_slot);
                if !target.is_null() {
                    let mut xpp = &mut (*target).p_caller_q as *mut *mut Proc;
                    while !(*xpp).is_null() {
                        if *xpp == rp {
                            *xpp = (*rp).p_q_link;
                            break;
                        }
                        xpp = &mut (**xpp).p_q_link as *mut *mut Proc;
                    }
                }
            }
            (*rp)
                .p_rts_flags
                .fetch_and(!RtsFlags::SENDING.bits(), Ordering::Relaxed);
        }

        (*rp)
            .p_rts_flags
            .fetch_and(!RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
        (*rp).p_getfrom_e = crate::system::NONE;
        (*rp).p_sendto_e = crate::system::NONE;

        // Set return code to EDEADSRCDST so the blocked process knows
        // the target endpoint is gone.  x86_64 syscall return is in rax.
        (*rp).p_reg.rax = crate::ipc::EDEADSRCDST as u64;
    }
}

/// Clear a process's endpoint.
///
/// Marks the process with RTS_NO_ENDPOINT, clears async size for
/// system processes, calls clear_ipc() and clear_ipc_refs().
///
/// # Safety
///
/// Process must be valid.
pub unsafe fn clear_endpoint(rp: *mut Proc) {
    unsafe {
        // Mark as having no endpoint
        let rts_flags = RtsFlags::NO_ENDPOINT;
        let old_flags = (*rp).p_rts_flags.load(Ordering::Relaxed);
        (*rp)
            .p_rts_flags
            .store(old_flags | rts_flags.bits(), Ordering::Relaxed);

        // Clear async size for system processes
        if !(*rp).p_priv.is_null() {
            let flags = (*(*rp).p_priv).s_flags;
            if flags.contains(crate::r#priv::PrivFlags::SYS_PROC) {
                (*(*rp).p_priv).s_asynsize = 0;
            }
        }

        // Clear IPC state
        clear_ipc(rp);

        // Clear IPC references from other processes
        clear_ipc_refs(rp);
    }
}

/// Clear IPC references to/from a process.
///
/// Walks the process table clearing notify pending, asyn pending,
/// and send/receive references to the given process.
///
/// # Safety
///
/// Process must be valid; process table must be accessible.
pub unsafe fn clear_ipc_refs(rp: *mut Proc) {
    unsafe {
        let base = crate::table::proc_table_base();
        let rp_endpoint = (*rp).p_endpoint;

        for i in 0..NR_PROCS_TOTAL {
            let p = base.add(i);
            if p == rp || (*p).is_empty() {
                continue;
            }

            // Clear notify and asyn pending if privilege structure exists
            if !(*p).p_priv.is_null() && !(*rp).p_priv.is_null() {
                let rc_id = (*(*rp).p_priv).s_id;
                if rc_id >= 0 {
                    (*(*p).p_priv).s_notify_pending.clear(rc_id as usize);
                    (*(*p).p_priv).s_asyn_pending.clear(rc_id as usize);
                }
            }

            // Cancel any outstanding async sends from this process to the target
            if !(*p).p_priv.is_null() {
                crate::ipc::cancel_async(p, rp);
            }

            // Check if this process is blocked on the target
            if (*p).blocked_on() == rp_endpoint {
                clear_ipc(p);
            }

            // Clear send/receive if targeting this process
            if (*p).p_sendto_e == rp_endpoint {
                (*p).p_sendto_e = crate::system::NONE;
            }
            if (*p).p_getfrom_e == rp_endpoint {
                (*p).p_getfrom_e = crate::system::NONE;
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// sched_proc (skeleton)
// ─────────────────────────────────────────────────────────────────────────

/// Notify a process's scheduler (skeleton).
///
/// # Safety
///
/// Process must be valid.
pub unsafe fn sched_proc(rp: *mut Proc, priority: i8) -> i32 {
    unsafe {
        (*rp).p_priority = priority;
        OK
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Stub handlers — need VM/data_copy/clock infrastructure (Phase 6+)
// ─────────────────────────────────────────────────────────────────────────

macro_rules! stub_handler {
    ($name:ident, $desc:expr) => {
        #[doc = concat!("Stub handler for ", $desc, ".")]
        ///
        /// # Safety
        ///
        /// Standard call handler signature.
        pub unsafe fn $name(_caller: *mut Proc, _msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
            // TODO: implement $desc
            EBADREQUEST
        }
    };
}

// Deferred stubs — need VM/data_copy infrastructure:
stub_handler!(do_exec_stub, "SYS_EXEC");
stub_handler!(do_privctl_stub, "SYS_PRIVCTL");
stub_handler!(do_trace_stub, "SYS_TRACE");
stub_handler!(do_vircopy_stub, "SYS_VIRCOPY");
stub_handler!(do_physcopy_stub, "SYS_PHYSCOPY");
stub_handler!(do_vumap_stub, "SYS_VUMAP");
stub_handler!(do_irqctl_stub, "SYS_IRQCTL");
stub_handler!(do_vtimer_stub, "SYS_VTIMER");
stub_handler!(do_setalarm_stub, "SYS_SETALARM"); // needs clock
stub_handler!(do_safecopy_from_stub, "SYS_SAFECOPYFROM");
stub_handler!(do_safecopy_to_stub, "SYS_SAFECOPYTO");
stub_handler!(do_vsafecopy_stub, "SYS_VSAFECOPY");
stub_handler!(do_sprofile_stub, "SYS_SPROF");
stub_handler!(do_cprofile_stub, "SYS_CPROF");
stub_handler!(do_profbuf_stub, "SYS_PROFBUF");
stub_handler!(do_stime_stub, "SYS_STIME");
stub_handler!(do_settime_stub, "SYS_SETTIME");
stub_handler!(do_getmcontext_stub, "SYS_GETMCONTEXT");
stub_handler!(do_setmcontext_stub, "SYS_SETMCONTEXT");
stub_handler!(do_update_stub, "SYS_UPDATE");
stub_handler!(do_safememset_stub, "SYS_SAFEMEMSET");
// ── Real implementations ───────────────────────────────────────────────

/// Handle SYS_EXIT: cause SIGABRT, don't reply.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`.
pub unsafe fn do_exit_handler(caller: *mut Proc, _msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        cause_sig((*caller).p_nr, 6); // SIGABRT
        EDONTREPLY
    }
}

/// Handle SYS_KILL: send a signal to a process.
///
/// # Safety
///
/// `msg` must contain valid sigcalls fields in the correct message layout.
pub unsafe fn do_kill_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let proc_nr_e = msg_read_i32(msg, SIGCALLS_ENDPT_OFF);
        let sig_nr = msg_read_i32(msg, SIGCALLS_SIG_OFF);
        if !table::is_ok_endpoint(proc_nr_e) {
            return crate::ipc::EFAULT;
        }
        let proc_nr = table::endpoint_slot(proc_nr_e);
        if sig_nr >= _NSIG {
            return crate::ipc::EFAULT;
        }
        if table::is_kernel_nr(proc_nr) {
            return crate::ipc::EPERM;
        }
        cause_sig(proc_nr, sig_nr);
        OK
    }
}

/// Handle SYS_GETKSIG: signal manager queries pending signals.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`; msg must be a valid message buffer.
pub unsafe fn do_getksig_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let caller_ep = (*caller).p_endpoint;
        let base = table::beg_user_addr();
        let end = table::end_proc_addr();
        let mut rp = base;
        while rp < end {
            let rts = (*rp).p_rts_flags.load(Ordering::Relaxed);
            if rts & RtsFlags::SIGNALED.bits() != 0
                && !(*rp).p_priv.is_null()
                && (*(*rp).p_priv).s_sig_mgr == caller_ep
            {
                msg_write_i32(msg, SIGCALLS_ENDPT_OFF, (*rp).p_endpoint);
                msg[SIGCALLS_MAP_OFF..SIGCALLS_MAP_OFF + 16]
                    .copy_from_slice(&(*rp).p_pending.to_ne_bytes());
                (*rp).p_pending = 0;
                (*rp)
                    .p_rts_flags
                    .fetch_and(!RtsFlags::SIGNALED.bits(), Ordering::Relaxed);
                return OK;
            }
            rp = rp.add(1);
        }
        msg_write_i32(msg, SIGCALLS_ENDPT_OFF, NONE);
        OK
    }
}

/// Handle SYS_ENDKSIG: signal manager done handling a kernel signal.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`; msg must contain valid sigcalls fields.
pub unsafe fn do_endksig_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let target_ep = msg_read_i32(msg, SIGCALLS_ENDPT_OFF);
        if !table::is_ok_endpoint(target_ep) {
            return crate::ipc::EFAULT;
        }
        let rp = proc_addr(table::endpoint_slot(target_ep));
        if rp.is_null() {
            return crate::ipc::EFAULT;
        }
        if (*rp).p_priv.is_null() {
            return crate::ipc::EPERM;
        }
        if (*(*rp).p_priv).s_sig_mgr != (*caller).p_endpoint {
            return crate::ipc::EPERM;
        }
        let rts = (*rp).p_rts_flags.load(Ordering::Relaxed);
        if rts & RtsFlags::SIG_PENDING.bits() == 0 {
            return crate::ipc::EFAULT;
        }
        if rts & RtsFlags::SIGNALED.bits() == 0 {
            (*rp)
                .p_rts_flags
                .fetch_and(!RtsFlags::SIG_PENDING.bits(), Ordering::Relaxed);
        }
        OK
    }
}

/// Handle SYS_FORK: clone a process table entry.
///
/// # Safety
///
/// `msg` must contain valid fork message fields in the correct layout.
pub unsafe fn do_fork_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let parent_ep = msg_read_i32(msg, FORK_ENDPT_OFF);
        let child_slot = msg_read_i32(msg, FORK_SLOT_OFF);
        let fork_flags = msg_read_u32(msg, FORK_FLAGS_OFF);
        if !table::is_ok_endpoint(parent_ep) || child_slot < 0 {
            return crate::ipc::EFAULT;
        }
        let rpp = proc_addr(table::endpoint_slot(parent_ep));
        if rpp.is_null() || table::is_empty_proc(rpp) {
            return crate::ipc::EFAULT;
        }
        if (*rpp).p_rts_flags.load(Ordering::Relaxed) & RtsFlags::RECEIVING.bits() == 0 {
            return crate::ipc::EFAULT;
        }
        let rpc = proc_addr(child_slot);
        if rpc.is_null() || !table::is_empty_proc(rpc) {
            return crate::ipc::EFAULT;
        }

        let mut new_gen = table::endpoint_gen((*rpc).p_endpoint) + 1;
        if new_gen >= table::EP_MAX_GENERATION {
            new_gen = 1;
        }

        core::ptr::copy_nonoverlapping(rpp, rpc, 1);
        (*rpc).p_nr = child_slot;
        (*rpc).p_endpoint = table::make_endpoint(new_gen, child_slot);
        (*rpc).p_reg.rax = 0;
        (*rpc).p_user_time = 0;
        (*rpc).p_sys_time = 0;
        let clear_mf = (MiscFlags::VIRT_TIMER
            | MiscFlags::PROF_TIMER
            | MiscFlags::SC_TRACE
            | MiscFlags::SPROF_SEEN
            | MiscFlags::STEP)
            .bits();
        (*rpc).p_misc_flags.store(
            (*rpp).p_misc_flags.load(Ordering::Relaxed) & !clear_mf,
            Ordering::Relaxed,
        );
        (*rpc).p_virt_left = 0;
        (*rpc).p_prof_left = 0;
        (*rpc).p_cpu_time_left = 0;
        (*rpc).p_cycles = 0;
        (*rpc).p_kcall_cycles = 0;
        (*rpc).p_kipc_cycles = 0;
        (*rpc).p_signal_received = 0;

        // Append "*F" to name
        let mut end = (*rpc).p_name.len();
        for i in 0..(*rpc).p_name.len() {
            if (*rpc).p_name[i] == 0 {
                end = i;
                break;
            }
        }
        if end + 3 < (*rpc).p_name.len() {
            (*rpc).p_name[end] = b'*' as i8;
            (*rpc).p_name[end + 1] = b'F' as i8;
            (*rpc).p_name[end + 2] = 0i8;
        }

        (*rpc)
            .p_rts_flags
            .store(RtsFlags::NO_QUANTUM.bits(), Ordering::Relaxed);
        crate::sched::reset_proc_accounting(rpc);

        if !(*rpp).p_priv.is_null() && (*(*rpp).p_priv).s_flags.contains(PrivFlags::SYS_PROC) {
            let priv_arr = core::ptr::addr_of_mut!(crate::r#priv::PPRIV_ADDR);
            (*rpc).p_priv = *((priv_arr as *mut *mut Priv).add(crate::r#priv::USER_PRIV_ID));
            (*rpc)
                .p_rts_flags
                .fetch_or(RtsFlags::NO_PRIV.bits(), Ordering::Relaxed);
        }

        msg_write_i32(msg, FORK_REPLY_ENDPT_OFF, (*rpc).p_endpoint);
        msg_write_u64(msg, FORK_REPLY_MSGADDR_OFF, (*rpp).p_delivermsg_vir);

        if fork_flags & PFF_VMINHIBIT != 0 {
            (*rpc)
                .p_rts_flags
                .fetch_or(RtsFlags::VMINHIBIT.bits(), Ordering::Relaxed);
        }
        let clear_rts = RtsFlags::SIGNALED | RtsFlags::SIG_PENDING | RtsFlags::P_STOP;
        (*rpc)
            .p_rts_flags
            .fetch_and(!clear_rts.bits(), Ordering::Relaxed);
        (*rpc).p_pending = 0;
        OK
    }
}

/// Handle SYS_CLEAR: clean up after process exit.
///
/// # Safety
///
/// `msg` must contain a valid clear endpoint field.
pub unsafe fn do_clear_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let exit_ep = msg_read_i32(msg, CLEAR_ENDPT_OFF);
        if !table::is_ok_endpoint(exit_ep) {
            return crate::ipc::EFAULT;
        }
        let rc = proc_addr(table::endpoint_slot(exit_ep));
        if rc.is_null() {
            return crate::ipc::EFAULT;
        }
        release_address_space(rc);
        if table::is_empty_proc(rc) {
            return OK;
        }
        let hooks = core::ptr::addr_of_mut!(IRQ_HOOKS);
        for i in 0..NR_IRQ_HOOKS {
            if (*rc).p_endpoint == (*hooks)[i].proc_nr_e {
                (*hooks)[i].proc_nr_e = NONE;
            }
        }
        clear_endpoint(rc);
        if !(*rc).p_priv.is_null() {
            (*(*rc).p_priv).s_alarm_timer = MinixTimer::default();
        }
        (*rc)
            .p_rts_flags
            .fetch_or(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        if !(*rc).p_priv.is_null() && (*(*rc).p_priv).s_flags.contains(PrivFlags::SYS_PROC) {
            (*(*rc).p_priv).s_proc_nr = NONE;
        }
        OK
    }
}

/// Handle SYS_ABORT: system shutdown.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_abort.c`
///
/// # Safety
///
/// `msg` must contain a valid abort message.
pub unsafe fn do_abort_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let _how = msg_read_i32(msg, ABORT_HOW_OFF);
        // prepare_shutdown(how) would halt the system — in the kernel, this
        // sends a shutdown message to the clock task. For now, no-op.
        // TODO: wire to clock task when available
        OK
    }
}

/// Handle SYS_TIMES: retrieve process timing info.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_times.c`
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_times_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let e_proc_nr = msg_read_i32(msg, TIMES_ENDPT_OFF);
        let target_ep = if e_proc_nr == SELF {
            (*caller).p_endpoint
        } else {
            e_proc_nr
        };
        if target_ep != NONE && table::is_ok_endpoint(target_ep) {
            let p = table::endpoint_slot(target_ep);
            let rp = proc_addr(p);
            if !rp.is_null() {
                msg_write_u64(msg, TIMES_REPLY_USER_OFF, (*rp).p_user_time);
                msg_write_u64(msg, TIMES_REPLY_SYSTEM_OFF, (*rp).p_sys_time);
            }
        }
        // Clock values are zero until the clock task is running
        msg_write_u64(msg, TIMES_REPLY_BOOTTICKS_OFF, 0);
        msg_write_u64(msg, TIMES_REPLY_REAL_OFF, 0);
        msg_write_u64(msg, TIMES_REPLY_BOOTTIME_OFF, 0);
        OK
    }
}

/// Handle SYS_RUNCTL: stop/resume a process.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_runctl.c`
///
/// # Safety
///
/// `msg` must contain valid runctl fields (m1_i1, m1_i2, m1_i3).
pub unsafe fn do_runctl_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let proc_nr_e = msg_read_i32(msg, M1_I1_OFF);
        let action = msg_read_i32(msg, M1_I2_OFF);
        let flags = msg_read_i32(msg, M1_I3_OFF);

        if !table::is_ok_endpoint(proc_nr_e) {
            return crate::ipc::EFAULT;
        }
        let proc_nr = table::endpoint_slot(proc_nr_e);
        if table::is_kernel_nr(proc_nr) {
            return crate::ipc::EPERM;
        }
        let rp = proc_addr(proc_nr);
        if rp.is_null() {
            return crate::ipc::EFAULT;
        }

        if action == arch_common::com::RC_STOP as i32 {
            if (flags & arch_common::com::RC_DELAY as i32) != 0 {
                let rts = (*rp).p_rts_flags.load(Ordering::Relaxed);
                if rts & RtsFlags::SENDING.bits() != 0 {
                    (*rp)
                        .p_misc_flags
                        .fetch_or(MiscFlags::SIG_DELAY.bits(), Ordering::Relaxed);
                }
                let mf = (*rp).p_misc_flags.load(Ordering::Relaxed);
                if mf & MiscFlags::SC_DEFER.bits() != 0 {
                    (*rp)
                        .p_misc_flags
                        .fetch_or(MiscFlags::SIG_DELAY.bits(), Ordering::Relaxed);
                }
                if (*rp).p_misc_flags.load(Ordering::Relaxed) & MiscFlags::SIG_DELAY.bits() != 0 {
                    return arch_common::ipc::EBUSY;
                }
            }
            (*rp)
                .p_rts_flags
                .fetch_or(RtsFlags::PROC_STOP.bits(), Ordering::Relaxed);
            OK
        } else if action == arch_common::com::RC_RESUME as i32 {
            (*rp)
                .p_rts_flags
                .fetch_and(!RtsFlags::PROC_STOP.bits(), Ordering::Relaxed);
            OK
        } else {
            crate::ipc::EFAULT
        }
    }
}

/// Handle SYS_STATECTL: process state control.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_statectl.c`
///
/// # Safety
///
/// `msg` must contain a valid statectl request field.
pub unsafe fn do_statectl_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let request = msg_read_i32(msg, STATECTL_REQUEST_OFF);
        match request {
            1 => {
                // SYS_STATE_CLEAR_IPC_REFS
                // In C: clear_ipc_refs(caller, EDEADSRCDST);
                // Our clear_ipc_refs takes only the target process
                // For now, just return OK
                OK
            }
            _ => crate::ipc::EFAULT,
        }
    }
}

/// Handle SYS_SCHEDULE: schedule a process.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_schedule.c`
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_schedule_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let proc_nr_e = msg_read_i32(msg, SCHEDULE_ENDPT_OFF);
        if !table::is_ok_endpoint(proc_nr_e) {
            return crate::ipc::EFAULT;
        }
        let proc_nr = table::endpoint_slot(proc_nr_e);
        let p = proc_addr(proc_nr);
        if p.is_null() {
            return crate::ipc::EFAULT;
        }

        // Only this process' scheduler can schedule it
        if (*p).p_scheduler != caller {
            return crate::ipc::EPERM;
        }

        let _quantum = msg_read_i32(msg, SCHEDULE_QUANTUM_OFF);
        let priority = msg_read_i32(msg, SCHEDULE_PRIORITY_OFF);
        let _cpu = msg_read_i32(msg, SCHEDULE_CPU_OFF);

        sched_proc(p, priority as i8);
        // C also clears RTS_NO_QUANTUM after scheduling
        (*p).p_rts_flags
            .fetch_and(!RtsFlags::NO_QUANTUM.bits(), Ordering::Relaxed);
        // Re-enqueue now that it's runnable
        if (*p).is_runnable() {
            crate::sched::enqueue(p);
        }
        OK
    }
}

/// Handle SYS_SCHEDCTL: scheduling control.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_schedctl.c`
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_schedctl_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let flags = msg_read_i32(msg, SCHEDCTL_FLAGS_OFF);
        if (flags as u32) & !arch_common::com::SCHEDCTL_FLAG_KERNEL != 0 {
            return crate::ipc::EFAULT;
        }
        let proc_nr_e = msg_read_i32(msg, SCHEDCTL_ENDPT_OFF);
        if !table::is_ok_endpoint(proc_nr_e) {
            return crate::ipc::EFAULT;
        }
        let proc_nr = table::endpoint_slot(proc_nr_e);
        let p = proc_addr(proc_nr);
        if p.is_null() {
            return crate::ipc::EFAULT;
        }

        if (flags as u32) & arch_common::com::SCHEDCTL_FLAG_KERNEL != 0 {
            let _priority = msg_read_i32(msg, SCHEDCTL_PRIORITY_OFF);
            let _quantum = msg_read_i32(msg, SCHEDCTL_QUANTUM_OFF);
            let _cpu = msg_read_i32(msg, SCHEDCTL_CPU_OFF);
            // Kernel becomes scheduler
            (*p).p_scheduler = core::ptr::null_mut();
            // Clear NO_QUANTUM to start scheduling
            (*p).p_rts_flags
                .fetch_and(!RtsFlags::NO_QUANTUM.bits(), Ordering::Relaxed);
            if (*p).is_runnable() {
                crate::sched::enqueue(p);
            }
        } else {
            // Caller becomes the scheduler
            (*p).p_scheduler = caller;
        }
        OK
    }
}

/// Handle SYS_DIAGCTL: diagnostic control.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_diagctl.c`
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_diagctl_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let code = msg_read_i32(msg, DIAGCTL_CODE_OFF);
        match code as u32 {
            arch_common::com::DIAGCTL_CODE_DIAG => {
                // Simplified: data_copy_vmcheck not available, skip copy
                // TODO: add data_copy when VM is available
                OK
            }
            arch_common::com::DIAGCTL_CODE_STACKTRACE => {
                // Stub: proc_stacktrace not yet implemented
                // TODO: call proc_stacktrace(proc_addr(..)) when available
                OK
            }
            arch_common::com::DIAGCTL_CODE_REGISTER => {
                if !(*caller).p_priv.is_null() {
                    let pf = (*(*caller).p_priv).s_flags;
                    if pf.contains(PrivFlags::SYS_PROC) {
                        (*(*caller).p_priv).s_diag_sig = 1;
                        return OK;
                    }
                }
                crate::ipc::EPERM
            }
            arch_common::com::DIAGCTL_CODE_UNREGISTER => {
                if !(*caller).p_priv.is_null() {
                    let pf = (*(*caller).p_priv).s_flags;
                    if pf.contains(PrivFlags::SYS_PROC) {
                        (*(*caller).p_priv).s_diag_sig = 0;
                        return OK;
                    }
                }
                crate::ipc::EPERM
            }
            _ => crate::ipc::EFAULT,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Address space switching
// ─────────────────────────────────────────────────────────────────────────

/// Switch to a process's address space by loading its per-process page
/// table root (CR3). If the process has no private page table
/// (`p_cr3 == 0`), this is a no-op — execution continues in the kernel's
/// identity-mapped address space (BOOT_CR3).
///
/// Called from the scheduler (`switch_to_user`), device I/O handlers
/// (`do_sdevio`), and the idle loop (`switch_address_space_idle`).
///
/// # Safety
///
/// `proc` must point to a valid, fully initialized `Proc`.
pub unsafe fn switch_address_space(proc: *const Proc) {
    unsafe {
        let cr3 = (*proc).p_seg.p_cr3;
        if cr3 != 0 {
            arch_x86_64::asm::write_cr3(cr3);
        }
    }
}

/// Release a process's address space. Currently a no-op — page table
/// deallocation is managed by the VM server (Phase 6+). In the C code,
/// this frees the page table pages allocated for the process.
/// Release a process's address space, freeing all page table pages.
///
/// Walks the 4-level page table hierarchy (PML4 → PDP → PD → PT)
/// via the identity map, frees all physical frames for user pages
/// and page table pages, then zeros the process's CR3.
///
/// # Safety
///
/// `proc` must point to a valid `Proc` with a page table that was
/// allocated through `kernel::vm::alloc_mem()`. Must run on BOOT_CR3
/// so the identity map is active for page table access.
pub unsafe fn release_address_space(proc: *mut Proc) {
    unsafe {
        let cr3 = (*proc).p_seg.p_cr3;
        if cr3 == 0 {
            return; // no per-process page table (kernel task or init)
        }

        // Walk the 4-level page table hierarchy.
        // The identity map covers 0-1GB, which is where page tables
        // are allocated (via alloc_mem within the boot memory chunks).

        let pml4 = cr3 as *const u64;

        // Process only user-space PML4 entries (0-255).
        // Kernel entries (256-511) are shared BOOT_PDP references.
        for pml4_idx in 0..256 {
            let pml4e = core::ptr::read(pml4.add(pml4_idx));
            if pml4e & arch_x86_64::pte::PG_P == 0 {
                continue; // not mapped
            }

            let pdpt_phys = pml4e & arch_x86_64::pte::PG_FRAME;
            let pdpt = pdpt_phys as *const u64;

            for pdpt_idx in 0..512 {
                let pdpte = core::ptr::read(pdpt.add(pdpt_idx));
                if pdpte & arch_x86_64::pte::PG_P == 0 {
                    continue;
                }
                if pdpte & arch_x86_64::pte::PG_PS != 0 {
                    // 1GB huge page — free the single physical frame
                    let pa = pdpte & arch_x86_64::pte::PG_FRAME;
                    let page = pa / crate::vm::VM_PAGE_SIZE as u64;
                    crate::vm::free_mem(page, 1);
                    continue;
                }

                let pd_phys = pdpte & arch_x86_64::pte::PG_FRAME;
                let pd = pd_phys as *const u64;

                for pd_idx in 0..512 {
                    let pde = core::ptr::read(pd.add(pd_idx));
                    if pde & arch_x86_64::pte::PG_P == 0 {
                        continue;
                    }
                    if pde & arch_x86_64::pte::PG_PS != 0 {
                        // 2MB huge page — free the single physical frame
                        let pa = pde & arch_x86_64::pte::PG_FRAME;
                        let page = pa / crate::vm::VM_PAGE_SIZE as u64;
                        crate::vm::free_mem(page, 1);
                        continue;
                    }

                    let pt_phys = pde & arch_x86_64::pte::PG_FRAME;
                    let pt = pt_phys as *const u64;

                    for pt_idx in 0..512 {
                        let pte = core::ptr::read(pt.add(pt_idx));
                        if pte & arch_x86_64::pte::PG_P == 0 {
                            continue;
                        }
                        // Free the 4KB user page
                        let pa = pte & arch_x86_64::pte::PG_FRAME;
                        let page = pa / crate::vm::VM_PAGE_SIZE as u64;
                        crate::vm::free_mem(page, 1);
                    }

                    // Free the PT page itself
                    let pt_page = pt_phys / crate::vm::VM_PAGE_SIZE as u64;
                    crate::vm::free_mem(pt_page, 1);
                }

                // Free the PD page itself
                let pd_page = pd_phys / crate::vm::VM_PAGE_SIZE as u64;
                crate::vm::free_mem(pd_page, 1);
            }

            // Free the PDP page itself
            let pdpt_page = pdpt_phys / crate::vm::VM_PAGE_SIZE as u64;
            crate::vm::free_mem(pdpt_page, 1);
        }

        // Free the PML4 page itself
        let pml4_page = cr3 / crate::vm::VM_PAGE_SIZE as u64;
        crate::vm::free_mem(pml4_page, 1);

        // Zero the process's CR3 fields
        (*proc).p_seg.p_cr3 = 0;
        (*proc).p_seg.p_cr3_v = core::ptr::null_mut();
        (*proc).p_cr3_saved = 0;
    }
}

/// Switch to the idle process's address space. On a uniprocessor
/// system, the idle process runs in the kernel's address space, so
/// this is a no-op. On SMP, the C code switches to VM_PROC_NR's
/// address space to ensure kernel pages are accessible on all CPUs.
///
/// Currently a no-op (no SMP support).
pub fn switch_address_space_idle() {
    // No-op on UP. On SMP, switch to VM_PROC_NR's address space:
    //     switch_address_space(proc_addr(VM_PROC_NR));
}

// ─────────────────────────────────────────────────────────────────────────
// Phase 6.13 — VM-dependent system call handlers
// ─────────────────────────────────────────────────────────────────────────

/// Handle SYS_UMAP: map virtual address to physical (subset of SYS_UMAP_REMOTE).
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_umap.c`
///
/// Allows mapping virtual addresses in the caller's address space and grants
/// where the caller is specified as grantee. Delegates to `do_umap_remote`.
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_umap_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let seg_index = msg_read_i32(msg, UMAP_SEGMENT_OFF) as u32 & SEGMENT_INDEX;
        let endpt = msg_read_i32(msg, UMAP_SRC_ENDPT_OFF);

        // This call is a subset of umap_remote: it allows mapping virtual
        // addresses in the caller's address space and grants where the caller
        // is specified as grantee.
        // In C: if (seg_index != MEM_GRANT && endpt != SELF) return EPERM;
        // MEM_GRANT = 3 in C, VM_GRANT = 2 in Rust arch-common encoding.
        if seg_index != 2 && endpt != SELF {
            return crate::ipc::EPERM;
        }
        // Set dst_endpt to SELF (caller is the grantee)
        msg_write_i32(msg, UMAP_DST_ENDPT_OFF, SELF);
        do_umap_remote_handler(caller, msg)
    }
}

/// Handle SYS_UMAP_REMOTE: map virtual address to physical for any process.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_umap_remote.c`
///
/// Translates a virtual address in a target process's address space to a
/// physical address using `vm_lookup()`. Supports grant-based access checks.
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_umap_remote_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let _seg_type =
            msg_read_i32(msg, UMAP_SEGMENT_OFF) as u32 & arch_common::consts::SEGMENT_TYPE;
        let seg_index = msg_read_i32(msg, UMAP_SEGMENT_OFF) as u32 & SEGMENT_INDEX;
        let src_addr = msg_read_u64(msg, UMAP_SRC_ADDR_OFF);
        let count = msg_read_i32(msg, UMAP_NR_BYTES_OFF);
        let mut endpt = msg_read_i32(msg, UMAP_SRC_ENDPT_OFF);
        let mut grantee = msg_read_i32(msg, UMAP_DST_ENDPT_OFF);

        // Resolve SELF
        if endpt == SELF {
            endpt = (*_caller).p_endpoint;
        }
        if grantee == SELF {
            grantee = (*_caller).p_endpoint;
        }

        // Validate source endpoint
        if !table::is_ok_endpoint(endpt) {
            return crate::ipc::EFAULT;
        }
        let proc_nr = table::endpoint_slot(endpt);
        if table::is_kernel_nr(proc_nr) {
            return crate::ipc::EPERM;
        }
        let targetpr = proc_addr(proc_nr);
        if targetpr.is_null() {
            return crate::ipc::EFAULT;
        }

        // Handle the segment type
        let mut lin_addr = src_addr;
        let mut lookup_proc = proc_nr;

        if seg_index == VM_GRANT {
            // VM_GRANT (MEM_GRANT in C) — verify grant first
            if !table::is_ok_endpoint(grantee) && grantee != NONE && grantee != ANY {
                return crate::ipc::EINVAL;
            }
            let grant_id = src_addr as u32;
            let verify_result = crate::grants::verify_grant(
                endpt,
                grantee,
                grant_id as i32,
                count.max(0) as u64,
                0,
                0,
            );
            match verify_result {
                Ok((newoffset, newep, _flags)) => {
                    if !table::is_ok_endpoint(newep) {
                        return crate::ipc::EFAULT;
                    }
                    let new_proc_nr = table::endpoint_slot(newep);
                    lin_addr = newoffset;
                    lookup_proc = new_proc_nr;
                }
                Err(_) => return crate::ipc::EFAULT,
            }
        }

        // Perform the VM lookup
        let phys_addr = crate::vm::vm_lookup(lookup_proc, lin_addr);
        if phys_addr == crate::vm::NO_MEM {
            return crate::ipc::EFAULT;
        }
        if phys_addr == 0 {
            return crate::ipc::EFAULT;
        }

        // Write the result
        msg_write_u64(msg, UMAP_SRC_ENDPT_OFF, phys_addr);
        OK
    }
}

/// Handle SYS_VMCTL: VM control operations.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_vmctl.c`
///
/// Dispatches on `SVMCTL_PARAM` (at msg offset M1_I2_OFF).
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_vmctl_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let who_ep = msg_read_i32(msg, M1_I1_OFF);
        let param = msg_read_i32(msg, M1_I2_OFF);
        let _value = msg_read_i32(msg, M1_I3_OFF);

        let ep = if who_ep == SELF {
            (*caller).p_endpoint
        } else {
            who_ep
        };

        if !table::is_ok_endpoint(ep) {
            return crate::ipc::EINVAL;
        }
        let proc_nr = table::endpoint_slot(ep);
        let p = proc_addr(proc_nr);
        if p.is_null() {
            return crate::ipc::EINVAL;
        }

        match param as u32 {
            arch_common::com::VMCTL_CLEAR_PAGEFAULT => {
                (*p).p_rts_flags
                    .fetch_and(!RtsFlags::PAGEFAULT.bits(), Ordering::Relaxed);
                OK
            }
            arch_common::com::VMCTL_GET_PDBR => {
                // Return the process's CR3 value
                let cr3 = (*p).p_seg.p_cr3;
                msg_write_u64(msg, M1_P1_OFF, cr3);
                OK
            }
            arch_common::com::VMCTL_FLUSHTLB => {
                // Flush TLB by rewriting CR3
                let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
                if boot_cr3 != 0 {
                    arch_x86_64::asm::tlb_flush();
                }
                OK
            }
            arch_common::com::VMCTL_VMINHIBIT_SET => {
                (*p).p_rts_flags
                    .fetch_or(RtsFlags::VMINHIBIT.bits(), Ordering::Relaxed);
                OK
            }
            arch_common::com::VMCTL_VMINHIBIT_CLEAR => {
                (*p).p_rts_flags
                    .fetch_and(!RtsFlags::VMINHIBIT.bits(), Ordering::Relaxed);
                OK
            }
            arch_common::com::VMCTL_BOOTINHIBIT_CLEAR => {
                (*p).p_rts_flags
                    .fetch_and(!RtsFlags::BOOTINHIBIT.bits(), Ordering::Relaxed);
                OK
            }
            arch_common::com::VMCTL_CLEARMAPCACHE => {
                // No map cache to clear in the Rust port yet
                OK
            }
            _ => crate::ipc::ENOSYS,
        }
    }
}

/// Handle SYS_MEMSET: write a pattern byte to physical memory.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_memset.c`
///
/// # Safety
///
/// `msg` must contain valid memset fields (base, count, pattern, process).
pub unsafe fn do_memset_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let _base = msg_read_u64(msg, MEMSET_BASE_OFF);
        let _count = msg_read_u64(msg, MEMSET_COUNT_OFF);
        let _pattern = msg_read_u64(msg, MEMSET_PATTERN_OFF);
        let _process = msg_read_i32(msg, MEMSET_PROC_OFF);

        // Delegate to vm_memset (physical address write)
        crate::vm::vm_memset(_base, _pattern as u8, _count as usize);
        OK
    }
}

/// Handle SYS_GETINFO: retrieve system information.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_getinfo.c`
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_getinfo_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let request = msg_read_i32(msg, GETINFO_REQUEST_OFF);
        let val_ptr = msg_read_u64(msg, GETINFO_VAL_PTR_OFF);
        let val_len = msg_read_i32(msg, GETINFO_VAL_LEN_OFF);
        let _val_ptr2 = msg_read_u64(msg, GETINFO_VAL_PTR2_OFF);
        let val_len2_e = msg_read_i32(msg, GETINFO_VAL_LEN2_E_OFF);

        match request as u32 {
            arch_common::com::GET_WHOAMI => {
                // Fill in the whoami reply fields in the message
                msg_write_i32(msg, WHOAMI_ENDPT_OFF, (*caller).p_endpoint);
                let priv_flags = if !(*caller).p_priv.is_null() {
                    (*(*caller).p_priv).s_flags.bits() as i32
                } else {
                    0
                };
                msg_write_i32(msg, WHOAMI_PRIVFLAGS_OFF, priv_flags);
                // Copy process name (up to 48 bytes)
                let name = (*caller).p_name;
                let name_bytes: &[u8] = core::slice::from_raw_parts(
                    name.as_ptr() as *const u8,
                    core::cmp::min(name.len(), 48),
                );
                let dst = &mut msg[WHOAMI_NAME_OFF..WHOAMI_NAME_OFF + 48];
                let copy_len = core::cmp::min(name_bytes.len(), dst.len() - 1);
                dst[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
                dst[copy_len] = 0;
                OK
            }
            arch_common::com::GET_MACHINE => {
                // Copy the machine info struct to the caller's buffer
                let machine = core::ptr::addr_of!(crate::glo::MACHINE).cast::<u8>();
                let machine_size = core::mem::size_of::<crate::glo::Machine>();
                if val_len > 0 && machine_size > val_len as usize {
                    return crate::ipc::E2BIG;
                }
                // Use virtual_copy to copy from kernel (boot address space)
                // Since the machine struct is in the kernel's identity-mapped space,
                // we can copy directly if BOOT_CR3 is active.
                let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
                if boot_cr3 == 0 {
                    // Pre-init / test mode: direct copy works
                    core::ptr::copy_nonoverlapping(
                        machine,
                        val_ptr as *mut u8,
                        core::cmp::min(machine_size, val_len.max(0) as usize),
                    );
                } else {
                    // Use virtual_copy to copy into caller's address space
                    crate::vm::virtual_copy(
                        -1, // KERNEL (proc_nr = -1 = HARDWARE/KERNEL)
                        machine as u64,
                        (*caller).p_nr,
                        val_ptr,
                        core::cmp::min(machine_size, val_len.max(0) as usize),
                    );
                }
                OK
            }
            arch_common::com::GET_KINFO => {
                let kinfo = core::ptr::addr_of!(crate::glo::KINFO).cast::<u8>();
                let kinfo_size = core::mem::size_of::<crate::glo::KInfo>();
                if val_len > 0 && kinfo_size > val_len as usize {
                    return crate::ipc::E2BIG;
                }
                let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
                if boot_cr3 == 0 {
                    core::ptr::copy_nonoverlapping(
                        kinfo,
                        val_ptr as *mut u8,
                        core::cmp::min(kinfo_size, val_len.max(0) as usize),
                    );
                } else {
                    crate::vm::virtual_copy(
                        -1,
                        kinfo as u64,
                        (*caller).p_nr,
                        val_ptr,
                        core::cmp::min(kinfo_size, val_len.max(0) as usize),
                    );
                }
                OK
            }
            arch_common::com::GET_HZ => {
                let hz = crate::glo::SYSTEM_HZ.load(core::sync::atomic::Ordering::Relaxed);
                let src_slice = core::slice::from_raw_parts(
                    core::ptr::addr_of!(hz).cast::<u8>(),
                    core::mem::size_of::<u32>(),
                );
                let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
                if boot_cr3 == 0 {
                    core::ptr::copy_nonoverlapping(
                        src_slice.as_ptr(),
                        val_ptr as *mut u8,
                        core::mem::size_of::<u32>(),
                    );
                } else {
                    crate::vm::virtual_copy(
                        -1,
                        src_slice.as_ptr() as u64,
                        (*caller).p_nr,
                        val_ptr,
                        core::mem::size_of::<u32>(),
                    );
                }
                OK
            }
            arch_common::com::GET_IRQHOOKS => {
                let hooks = core::ptr::addr_of!(IRQ_HOOKS).cast::<u8>();
                let hooks_size = core::mem::size_of::<[IrqHook; NR_IRQ_HOOKS]>();
                if val_len > 0 && hooks_size > val_len as usize {
                    return crate::ipc::E2BIG;
                }
                let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
                if boot_cr3 == 0 {
                    core::ptr::copy_nonoverlapping(
                        hooks,
                        val_ptr as *mut u8,
                        core::cmp::min(hooks_size, val_len.max(0) as usize),
                    );
                } else {
                    crate::vm::virtual_copy(
                        -1,
                        hooks as u64,
                        (*caller).p_nr,
                        val_ptr,
                        core::cmp::min(hooks_size, val_len.max(0) as usize),
                    );
                }
                OK
            }
            arch_common::com::GET_PROCTAB => {
                let proc_base = crate::table::proc_table_base() as *const u8;
                let proctab_size =
                    core::mem::size_of::<crate::proc::Proc>() * crate::proc::NR_PROCS_TOTAL;
                if val_len > 0 && proctab_size > val_len as usize {
                    return crate::ipc::E2BIG;
                }
                let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
                if boot_cr3 == 0 {
                    core::ptr::copy_nonoverlapping(
                        proc_base,
                        val_ptr as *mut u8,
                        core::cmp::min(proctab_size, val_len.max(0) as usize),
                    );
                } else {
                    crate::vm::virtual_copy(
                        -1,
                        proc_base as u64,
                        (*caller).p_nr,
                        val_ptr,
                        core::cmp::min(proctab_size, val_len.max(0) as usize),
                    );
                }
                OK
            }
            arch_common::com::GET_PRIVTAB => {
                let priv_base = core::ptr::addr_of!(crate::r#priv::PRIV).cast::<u8>();
                let privtab_size =
                    core::mem::size_of::<crate::r#priv::Priv>() * crate::proc::NR_SYS_PROCS;
                if val_len > 0 && privtab_size > val_len as usize {
                    return crate::ipc::E2BIG;
                }
                let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
                if boot_cr3 == 0 {
                    core::ptr::copy_nonoverlapping(
                        priv_base,
                        val_ptr as *mut u8,
                        core::cmp::min(privtab_size, val_len.max(0) as usize),
                    );
                } else {
                    crate::vm::virtual_copy(
                        -1,
                        priv_base as u64,
                        (*caller).p_nr,
                        val_ptr,
                        core::cmp::min(privtab_size, val_len.max(0) as usize),
                    );
                }
                OK
            }
            arch_common::com::GET_PROC => {
                let target_ep = if val_len2_e == SELF {
                    (*caller).p_endpoint
                } else {
                    val_len2_e
                };
                if !table::is_ok_endpoint(target_ep) {
                    return crate::ipc::EINVAL;
                }
                let target_nr = table::endpoint_slot(target_ep);
                let target_pr = proc_addr(target_nr);
                if target_pr.is_null() {
                    return crate::ipc::EINVAL;
                }
                let proc_size = core::mem::size_of::<crate::proc::Proc>();
                if val_len > 0 && proc_size > val_len as usize {
                    return crate::ipc::E2BIG;
                }
                let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
                if boot_cr3 == 0 {
                    core::ptr::copy_nonoverlapping(
                        target_pr.cast::<u8>(),
                        val_ptr as *mut u8,
                        core::cmp::min(proc_size, val_len.max(0) as usize),
                    );
                } else {
                    crate::vm::virtual_copy(
                        -1,
                        target_pr as u64,
                        (*caller).p_nr,
                        val_ptr,
                        core::cmp::min(proc_size, val_len.max(0) as usize),
                    );
                }
                OK
            }
            arch_common::com::GET_PRIV => {
                let target_ep = if val_len2_e == SELF {
                    (*caller).p_endpoint
                } else {
                    val_len2_e
                };
                if !table::is_ok_endpoint(target_ep) {
                    return crate::ipc::EINVAL;
                }
                let target_nr = table::endpoint_slot(target_ep);
                let priv_entry = crate::r#priv::priv_addr(target_nr as usize);
                let priv_size = core::mem::size_of::<crate::r#priv::Priv>();
                if val_len > 0 && priv_size > val_len as usize {
                    return crate::ipc::E2BIG;
                }
                let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
                if boot_cr3 == 0 {
                    core::ptr::copy_nonoverlapping(
                        core::ptr::addr_of!(*priv_entry).cast::<u8>(),
                        val_ptr as *mut u8,
                        core::cmp::min(priv_size, val_len.max(0) as usize),
                    );
                } else {
                    crate::vm::virtual_copy(
                        -1,
                        core::ptr::addr_of!(*priv_entry) as u64,
                        (*caller).p_nr,
                        val_ptr,
                        core::cmp::min(priv_size, val_len.max(0) as usize),
                    );
                }
                OK
            }
            arch_common::com::GET_IRQACTIDS => {
                let irq_actids = core::ptr::addr_of!(IRQ_ACTIDS).cast::<u8>();
                let irq_actids_size = core::mem::size_of::<[i32; 64]>();
                if val_len > 0 && irq_actids_size > val_len as usize {
                    return crate::ipc::E2BIG;
                }
                let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
                if boot_cr3 == 0 {
                    core::ptr::copy_nonoverlapping(
                        irq_actids,
                        val_ptr as *mut u8,
                        core::cmp::min(irq_actids_size, val_len.max(0) as usize),
                    );
                } else {
                    crate::vm::virtual_copy(
                        -1,
                        irq_actids as u64,
                        (*caller).p_nr,
                        val_ptr,
                        core::cmp::min(irq_actids_size, val_len.max(0) as usize),
                    );
                }
                OK
            }
            arch_common::com::GET_MONPARAMS => {
                // Use a local empty params buffer
                let params = [0u8; 1024];
                let params_size = params.len();
                if val_len > 0 && params_size > val_len as usize {
                    return crate::ipc::E2BIG;
                }
                let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
                if boot_cr3 == 0 {
                    core::ptr::copy_nonoverlapping(
                        params.as_ptr(),
                        val_ptr as *mut u8,
                        core::cmp::min(params_size, val_len.max(0) as usize),
                    );
                } else {
                    crate::vm::virtual_copy(
                        -1,
                        params.as_ptr() as u64,
                        (*caller).p_nr,
                        val_ptr,
                        core::cmp::min(params_size, val_len.max(0) as usize),
                    );
                }
                OK
            }
            _ => crate::ipc::ENOSYS,
        }
    }
}

/// Handle SYS_SIGSEND: deliver a signal (minimal version).
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_sigsend.c`
///
/// Validates the target process and sets the pending signal.
/// The full C implementation builds a sigframe on the target's stack;
/// for now, we set the pending signal and let the signal manager handle it.
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_sigsend_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let target_ep = msg_read_i32(msg, SIGCALLS_ENDPT_OFF);
        let _sigctx = msg_read_u64(msg, 24); // sigctx at offset 24 in sigcalls

        if !table::is_ok_endpoint(target_ep) {
            return crate::ipc::EINVAL;
        }
        let proc_nr = table::endpoint_slot(target_ep);
        if table::is_kernel_nr(proc_nr) {
            return crate::ipc::EPERM;
        }
        let rp = proc_addr(proc_nr);
        if rp.is_null() {
            return crate::ipc::EINVAL;
        }

        // Set the pending signal so the signal manager picks it up.
        // The full implementation would copy the sigmsg from the caller,
        // build a sigframe on the target's stack, and set registers.
        // For now, just mark pending and notify.
        cause_sig(proc_nr, 6); // SIGABRT as a generic signal
        OK
    }
}

/// Handle SYS_SIGRETURN: return from a signal handler (minimal version).
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_sigreturn.c`
///
/// Validates the target process. The full C implementation restores
/// registers from a sigcontext; for now, we clear the signal flags.
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_sigreturn_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let target_ep = msg_read_i32(msg, SIGCALLS_ENDPT_OFF);

        if !table::is_ok_endpoint(target_ep) {
            return crate::ipc::EINVAL;
        }
        let proc_nr = table::endpoint_slot(target_ep);
        if table::is_kernel_nr(proc_nr) {
            return crate::ipc::EPERM;
        }
        let rp = proc_addr(proc_nr);
        if rp.is_null() {
            return crate::ipc::EINVAL;
        }

        // Clear the signaled flags so the process can resume
        let clear_flags = RtsFlags::SIGNALED | RtsFlags::SIG_PENDING;
        (*rp)
            .p_rts_flags
            .fetch_and(!clear_flags.bits(), Ordering::Relaxed);
        (*rp).p_pending = 0;

        OK
    }
}

/// Handle SYS_SETGRANT: set the grant table for a process.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_setgrant.c`
///
/// Copies the grant table address and entry count into the caller's
/// privilege structure.
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_setgrant_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let grant_addr = msg_read_u64(msg, SETGRANT_ADDR_OFF);
        let grant_entries = msg_read_i32(msg, SETGRANT_SIZE_OFF);

        // Check that the caller has a valid privilege structure
        let rts = (*caller).p_rts_flags.load(Ordering::Relaxed);
        if rts & RtsFlags::NO_PRIV.bits() != 0 || (*caller).p_priv.is_null() {
            return crate::ipc::EPERM;
        }

        // Set the grant table pointer and entry count in the priv structure
        // This mirrors the C `_K_SET_GRANT_TABLE` macro:
        //   priv(rp)->s_grant_table = (ptr);
        //   priv(rp)->s_grant_entries = (entries);
        (*(*caller).p_priv).s_grant_table = grant_addr;
        (*(*caller).p_priv).s_grant_entries = grant_entries;

        OK
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::proc_init;
    use arch_common::com::GET_WHOAMI;

    // ── Static Priv pool for test pointer stability ─────────────────
    // Prevents dangling pointers when tests set p_priv and later
    // tests invoke clear_ipc_refs / cancel_async on the same process.

    const TEST_PRIV_SLOT_BYTES: usize = 2048;
    #[repr(C, align(64))]
    struct TestPrivPool {
        data: [u8; TEST_PRIV_SLOT_BYTES * 8],
    }
    static mut TEST_PRIV_POOL: TestPrivPool = TestPrivPool {
        data: [0u8; TEST_PRIV_SLOT_BYTES * 8],
    };

    /// Get a zeroed `*mut Priv` from the static pool at the given slot.
    unsafe fn setup_test_priv(slot: usize) -> *mut Priv {
        unsafe {
            let base = &raw mut TEST_PRIV_POOL as *mut TestPrivPool as *mut u8;
            let p = base.add(slot.min(7) * TEST_PRIV_SLOT_BYTES).cast::<Priv>();
            core::ptr::write_bytes(p.cast::<u8>(), 0, TEST_PRIV_SLOT_BYTES);
            (*p).s_sig_mgr = i32::MIN;
            (*p).s_flags = PrivFlags::empty();
            p
        }
    }

    fn init_signal_env() {
        unsafe {
            arch_x86_64::cpulocals::init_cpulocals();
            proc_init();
        }
    }

    #[test]
    fn test_system_init_registers_handlers() {
        unsafe {
            system_init();
            // Check that some handlers are registered
            assert!(CALL_VEC[0].is_some()); // SYS_FORK = KERNEL_CALL + 0
            assert!(CALL_VEC[3].is_some()); // SYS_SCHEDULE = KERNEL_CALL + 3
            assert!(CALL_VEC[56].is_some()); // SYS_SAFEMEMSET = KERNEL_CALL + 56
        }
    }

    #[test]
    fn test_kernel_call_dispatch_invalid_call() {
        unsafe {
            proc_init();
            system_init();
            let rp = crate::table::proc_addr(0);
            // Set up privilege with k_call_mask
            let priv0 = setup_test_priv(0);
            (*priv0).s_k_call_mask = [!0u32; SYS_CALL_MASK_SIZE];
            (*rp).p_priv = priv0;

            let mut msg = [0u8; MESSAGE_SIZE];
            msg[0..4].copy_from_slice(&(KERNEL_CALL + 999).to_ne_bytes());
            let result = kernel_call_dispatch(rp, &mut msg);
            assert_eq!(result, EBADREQUEST);
        }
    }

    #[test]
    fn test_kernel_call_dispatch_denied() {
        unsafe {
            proc_init();
            system_init();
            let rp = crate::table::proc_addr(0);
            let priv0 = setup_test_priv(0);
            (*priv0).s_k_call_mask = [0u32; SYS_CALL_MASK_SIZE];
            (*rp).p_priv = priv0;

            let mut msg = [0u8; MESSAGE_SIZE];
            msg[0..4].copy_from_slice(&(KERNEL_CALL + 3).to_ne_bytes());
            let result = kernel_call_dispatch(rp, &mut msg);
            assert_eq!(result, ECALLDENIED);
        }
    }

    #[test]
    fn test_kernel_call_dispatch_allowed() {
        unsafe {
            proc_init();
            system_init();
            let rp = crate::table::proc_addr(0);
            let priv0 = setup_test_priv(0);
            (*priv0).s_k_call_mask = [!0u32; SYS_CALL_MASK_SIZE];
            (*rp).p_priv = priv0;

            // Call handler directly (bypassing kernel_call_dispatch which reads call from msg[0..4])
            let mut msg = [0u8; MESSAGE_SIZE];
            let ep = crate::table::make_endpoint(0, 1); // valid endpoint
            msg_write_i32(&mut msg, SCHEDULE_ENDPT_OFF, ep);
            let result = do_schedule_handler(rp, &mut msg);
            // Should return EPERM because p_scheduler != caller (p_scheduler is null)
            assert_eq!(result, crate::ipc::EPERM);
        }
    }

    #[test]
    fn test_get_priv_returns_slot() {
        unsafe {
            proc_init();
            system_init();
            let rp = crate::table::proc_addr(0);

            // Set all proc_nr to NONE to free slots
            let base = core::ptr::addr_of_mut!(PRIV).cast::<Priv>();
            for i in 0..NR_SYS_PROCS {
                (*base.add(i)).s_proc_nr = NONE;
            }

            let slot = get_priv(rp);
            assert!(slot.is_some());
        }
    }

    #[test]
    fn test_set_unset_sendto_bit() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            // Set up a Priv structure for the process
            if !(*rp).p_priv.is_null() {
                set_sendto_bit(&*rp, 5);
                assert!((*(*rp).p_priv).s_ipc_to.test(5));

                unset_sendto_bit(&*rp, 5);
                assert!(!(*(*rp).p_priv).s_ipc_to.test(5));
            }
        }
    }
    // ── Signal delivery tests ────────────────────────────────────────────
    //
    // Tests for cause_sig (signal manager notification) and send_sig
    // (C path with priv->s_sig_pending + SYSTEM notification).

    fn init_signal_test_env() {
        unsafe {
            arch_x86_64::cpulocals::init_cpulocals();
            proc_init();
        }
    }

    #[test]
    fn test_cause_sig_sets_flags() {
        unsafe {
            init_signal_test_env();
            let rp = crate::table::proc_addr(0);
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);

            cause_sig(0, 1); // SIG1 to process 0

            let flags = (*rp).p_rts_flags.load(Ordering::Relaxed);
            assert!(flags & RtsFlags::SIGNALED.bits() != 0);
            assert!(flags & RtsFlags::SIG_PENDING.bits() != 0);
        }
    }

    #[test]
    fn test_cause_sig_sets_p_pending() {
        unsafe {
            init_signal_test_env();
            let rp = crate::table::proc_addr(0);
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);
            (*rp).p_pending = 0;

            // Signal 5 should set bit 5 in p_pending
            cause_sig(0, 5);
            assert!(
                (*rp).p_pending & (1u32 << 5) != 0,
                "p_pending should have bit 5 set after cause_sig(0, 5)"
            );

            // Signal 17 should also be OR'd in
            cause_sig(0, 17);
            assert!(
                (*rp).p_pending & (1u32 << 5) != 0,
                "bit 5 should still be set"
            );
            assert!(
                (*rp).p_pending & (1u32 << 17) != 0,
                "p_pending should have bit 17 set after cause_sig(0, 17)"
            );

            // Signal -1 (invalid) should be silently ignored
            let before = (*rp).p_pending;
            cause_sig(0, -1);
            assert_eq!(
                (*rp).p_pending,
                before,
                "p_pending should not change for invalid sig_nr"
            );
        }
    }

    #[test]
    fn test_clear_ipc_clears() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            (*rp).p_rts_flags.store(
                RtsFlags::SENDING.bits() | RtsFlags::RECEIVING.bits(),
                Ordering::Relaxed,
            );
            (*rp).p_getfrom_e = 42;
            (*rp).p_sendto_e = 7;

            clear_ipc(rp);

            let flags = (*rp).p_rts_flags.load(Ordering::Relaxed);
            assert_eq!(flags & RtsFlags::SENDING.bits(), 0);
            assert_eq!(flags & RtsFlags::RECEIVING.bits(), 0);
            assert_eq!((*rp).p_getfrom_e, NONE);
            assert_eq!((*rp).p_sendto_e, NONE);
        }
    }

    #[test]
    fn test_clear_endpoint_clears() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            (*rp).p_endpoint = 123;

            clear_endpoint(rp);

            // C semantics: endpoint value is NOT cleared, but RTS_NO_ENDPOINT is set
            // to prevent further use. The endpoint remains for reference cleanup.
            let flags = (*rp).p_rts_flags.load(Ordering::Relaxed);
            assert!(
                flags & RtsFlags::NO_ENDPOINT.bits() != 0,
                "RTS_NO_ENDPOINT should be set"
            );
        }
    }

    #[test]
    fn test_sched_proc_sets_priority() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let result = sched_proc(rp, 7);
            assert_eq!(result, OK);
            assert_eq!((*rp).p_priority, 7);
        }
    }

    // ── Signal delivery tests ────────────────────────────────────────────
    //
    // Tests for cause_sig (signal manager notification) and send_sig
    // (C path with priv->s_sig_pending + SYSTEM notification).

    #[test]
    fn test_cause_sig_notifies_signal_manager() {
        unsafe {
            init_signal_test_env();
            // Set up proc 0 with a signal manager (proc 1)
            let rp = crate::table::proc_addr(0);
            let mgr = crate::table::proc_addr(1);
            let mgr_ep = crate::table::make_endpoint(0, 1);
            (*mgr).p_endpoint = mgr_ep;
            // Set up proc 0's priv with sig_mgr pointing to proc 1
            let priv0 = setup_test_priv(0);
            (*priv0).s_sig_mgr = mgr_ep;
            (*rp).p_priv = priv0;
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);

            // Set up sig mgr (proc 1) with a priv so send_sig can set s_sig_pending
            let priv1 = setup_test_priv(1);
            (*priv1).s_sig_pending = 0;
            (*mgr).p_priv = priv1;

            // Set mgr as RECEIVING from ANY so mini_notify will wake it
            (*mgr)
                .p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            (*mgr).p_getfrom_e = crate::system::NONE;

            cause_sig(0, 1);

            // send_sig should set SIGKSIG (74) bit in mgr's s_sig_pending
            assert!(
                (*mgr).p_priv.as_ref().unwrap().s_sig_pending & (1u128 << 74) != 0,
                "send_sig should set SIGKSIG (74) in s_sig_pending"
            );

            // send_sig also sets SIGNALED and SIG_PENDING on the mgr process
            let mgr_rts = (*mgr).p_rts_flags.load(Ordering::Relaxed);
            assert!(
                mgr_rts & RtsFlags::SIGNALED.bits() != 0,
                "SIGNALED should be set on mgr via send_sig"
            );
            assert!(
                mgr_rts & RtsFlags::SIG_PENDING.bits() != 0,
                "SIG_PENDING should be set on mgr via send_sig"
            );
        }
    }

    #[test]
    fn test_cause_sig_skips_notify_when_no_sig_mgr() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            (*rp).p_priv = core::ptr::null_mut();
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);

            // Should not crash when p_priv is null
            cause_sig(0, 1);

            let flags = (*rp).p_rts_flags.load(Ordering::Relaxed);
            assert!(
                flags & RtsFlags::SIGNALED.bits() != 0,
                "SIGNALED should be set even without priv"
            );
        }
    }

    #[test]
    fn test_send_sig_returns_error_for_invalid_proc() {
        unsafe {
            assert_eq!(send_sig(-999, 1), EBADREQUEST);
        }
    }

    #[test]
    fn test_send_sig_uses_priv_pending_not_pending() {
        unsafe {
            init_signal_env();
            // Set up proc 0 with a priv structure
            let rp = crate::table::proc_addr(0);
            let priv0 = setup_test_priv(0);
            (*priv0).s_flags = PrivFlags::SYS_PROC;
            (*priv0).s_sig_pending = 0;
            (*rp).p_priv = priv0;
            (*rp).p_pending = 0;
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);

            let result = send_sig(0, 3);
            assert_eq!(result, OK);

            // s_sig_pending should have bit 3 set
            assert!(
                (*rp).p_priv.as_ref().unwrap().s_sig_pending & (1u128 << 3) != 0,
                "send_sig should set s_sig_pending, not p_pending"
            );
            // p_pending should NOT be set (send_sig uses priv path)
            assert_eq!((*rp).p_pending, 0, "send_sig should NOT modify p_pending");
        }
    }

    #[test]
    fn test_send_sig_dequeues_runnable_proc() {
        unsafe {
            init_signal_env();
            let rp = crate::table::proc_addr(0);
            let priv0 = setup_test_priv(0);
            (*priv0).s_flags = PrivFlags::SYS_PROC;
            (*priv0).s_sig_pending = 0;
            (*rp).p_priv = priv0;
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);
            // Set run queue so dequeue has something to work with
            crate::sched::enqueue(rp);

            send_sig(0, 3);

            // Process should be dequeued (SIGNALED | SIG_PENDING set)
            let flags = (*rp).p_rts_flags.load(Ordering::Relaxed);
            assert!(
                flags & (RtsFlags::SIGNALED | RtsFlags::SIG_PENDING).bits() != 0,
                "send_sig should set SIGNALED and SIG_PENDING"
            );
        }
    }

    #[test]
    fn test_send_sig_notifies_system_for_user_proc() {
        unsafe {
            init_signal_env();
            // Set up proc 0 as a user process (no SYS_PROC flag)
            let rp = crate::table::proc_addr(0);
            let priv0 = setup_test_priv(0);
            (*priv0).s_flags = PrivFlags::empty(); // not SYS_PROC
            (*priv0).s_sig_pending = 0;
            (*rp).p_priv = priv0;
            (*rp).p_endpoint = crate::table::make_endpoint(0, 0);
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);

            // The notification goes FROM SYSTEM TO the target process (proc 0).
            // Set proc 0 as RECEIVING from SYSTEM to verify wake-up.
            let sys_ep = arch_common::com::SYSTEM;
            (*rp)
                .p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            (*rp).p_getfrom_e = sys_ep;

            send_sig(0, 3);

            // Proc 0 should have been woken by mini_notify
            let rts = (*rp).p_rts_flags.load(Ordering::Relaxed);
            assert_eq!(
                rts & RtsFlags::RECEIVING.bits(),
                0,
                "send_sig should notify the target process via SYSTEM"
            );
        }
    }
    //
    // These functions call privileged instructions (write_cr3) that cannot
    // be executed from usermode test binaries. We verify:
    // - No-op behavior when p_cr3 == 0 (doesn't touch CR3)
    // - Function signatures compile and are callable
    // - No panics / crashes on valid inputs

    #[test]
    fn test_switch_address_space_null_cr3_noop() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            // Process with p_cr3 == 0 (default) should be a no-op
            (*rp).p_seg.p_cr3 = 0;
            // Should not crash or change anything visible
            switch_address_space(rp);
        }
    }

    #[test]
    fn test_switch_address_space_nonzero_cr3_type_check() {
        // Verify the function signature compiles.
        // We cannot actually call write_cr3 with a fake value from
        // usermode (privileged instruction), but the function
        // is callable with a valid proc pointer.
        fn _fn(_f: unsafe fn(*const Proc)) {}
        _fn(switch_address_space);
    }

    #[test]
    fn test_release_address_space_zero_cr3() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            // With CR3=0 (kernel task), should do nothing and not crash
            (*rp).p_seg.p_cr3 = 0;
            release_address_space(rp);
            assert_eq!((*rp).p_seg.p_cr3, 0);
        }
    }

    // test_release_address_space_clears_cr3 removed — it dereferences
    // physical page table addresses which requires the kernel's identity
    // mapping and crashes on host test binaries.

    #[test]
    fn test_switch_address_space_idle_noop() {
        // Should not crash or panic
        switch_address_space_idle();
    }

    // ── Syscall handler tests ──────────────────────────────────────────

    #[test]
    fn test_do_exit_handler_returns_edontreply() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            (*rp).p_nr = 0;
            let mut msg = [0u8; MESSAGE_SIZE];
            let result = do_exit_handler(rp, &mut msg);
            // Returns EDONTREPLY (no reply sent)
            assert_eq!(result, EDONTREPLY);
            // Should have set SIGNALED | SIG_PENDING on the caller
            let flags = (*rp).p_rts_flags.load(Ordering::Relaxed);
            assert!(
                flags & RtsFlags::SIGNALED.bits() != 0,
                "do_exit should cause SIGABRT"
            );
        }
    }

    #[test]
    fn test_do_kill_handler_invalid_endpoint() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            // Invalid endpoint
            msg_write_i32(&mut msg, SIGCALLS_ENDPT_OFF, 99999);
            msg_write_i32(&mut msg, SIGCALLS_SIG_OFF, 1);
            let result = do_kill_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EFAULT);
        }
    }

    #[test]
    fn test_do_kill_handler_sig_out_of_range() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            // Valid endpoint but sig >= _NSIG
            let ep = crate::table::make_endpoint(0, 1);
            msg_write_i32(&mut msg, SIGCALLS_ENDPT_OFF, ep);
            msg_write_i32(&mut msg, SIGCALLS_SIG_OFF, _NSIG);
            let result = do_kill_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EFAULT);
        }
    }

    #[test]
    fn test_do_kill_handler_kernel_target_rejected() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            // Kernel task endpoint (negative proc nr)
            let ep = arch_common::com::CLOCK; // -3
            msg_write_i32(&mut msg, SIGCALLS_ENDPT_OFF, ep);
            msg_write_i32(&mut msg, SIGCALLS_SIG_OFF, 1);
            let result = do_kill_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EPERM);
        }
    }

    #[test]
    fn test_do_kill_handler_sends_signal() {
        unsafe {
            init_signal_env();
            let rp = crate::table::proc_addr(0);
            (*rp).p_priv = core::ptr::null_mut();
            let target = crate::table::proc_addr(1);
            let target_ep = crate::table::make_endpoint(0, 1);
            (*target).p_endpoint = target_ep;
            (*target).p_rts_flags.store(0, Ordering::Relaxed);
            // Give target a priv so send_sig can work
            let priv1 = setup_test_priv(1);
            (*priv1).s_flags = PrivFlags::SYS_PROC;
            (*priv1).s_sig_pending = 0;
            (*target).p_priv = priv1;

            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, SIGCALLS_ENDPT_OFF, target_ep);
            msg_write_i32(&mut msg, SIGCALLS_SIG_OFF, 3);

            let result = do_kill_handler(rp, &mut msg);
            assert_eq!(result, OK);

            // Target should have signal pending
            let flags = (*target).p_rts_flags.load(Ordering::Relaxed);
            assert!(
                flags & RtsFlags::SIGNALED.bits() != 0,
                "do_kill should set SIGNALED on target"
            );
        }
    }

    #[test]
    fn test_do_fork_handler_invalid_parent() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            // Invalid endpoint
            msg_write_i32(&mut msg, FORK_ENDPT_OFF, 99999);
            let result = do_fork_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EFAULT);
        }
    }

    #[test]
    fn test_do_fork_handler_child_slot_in_use() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let parent_ep = crate::table::make_endpoint(0, 0);
            (*rp).p_endpoint = parent_ep;
            // Set parent as receiving
            (*rp)
                .p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);

            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, FORK_ENDPT_OFF, parent_ep);
            // Slot 1 is a boot proc, not empty, so fork should fail
            msg_write_i32(&mut msg, FORK_SLOT_OFF, 1);
            msg_write_i32(&mut msg, FORK_FLAGS_OFF, 0);

            let result = do_fork_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EFAULT);
        }
    }

    #[test]
    fn test_do_fork_handler_child_not_receiving() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let parent_ep = crate::table::make_endpoint(0, 0);
            (*rp).p_endpoint = parent_ep;
            // Parent is NOT receiving
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);

            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, FORK_ENDPT_OFF, parent_ep);
            // Slot 0 is parent itself (not empty)
            msg_write_i32(&mut msg, FORK_SLOT_OFF, 99);
            msg_write_i32(&mut msg, FORK_FLAGS_OFF, 0);

            let result = do_fork_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EFAULT);
        }
    }

    #[test]
    fn test_do_clear_handler_invalid_endpoint() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, CLEAR_ENDPT_OFF, 99999); // invalid endpoint
            let result = do_clear_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EFAULT);
        }
    }

    #[test]
    fn test_do_clear_handler_already_cleared() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            // Mark slot as free
            (*rp)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
            let ep = crate::table::make_endpoint(0, 0);
            (*rp).p_endpoint = ep;

            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, CLEAR_ENDPT_OFF, ep);
            let result = do_clear_handler(rp, &mut msg);
            // Already cleared should return OK
            assert_eq!(result, OK);
        }
    }

    // ── Phase 6.13 handler tests ───────────────────────────────────

    #[test]
    fn test_do_umap_handler_invalid_endpoint() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, UMAP_SRC_ENDPT_OFF, 99999);
            let result = do_umap_handler(rp, &mut msg);
            assert_eq!(
                result,
                crate::ipc::EPERM,
                "non-SELF endpoint should be EPERM"
            );
        }
    }

    #[test]
    fn test_do_umap_handler_self_ok() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, UMAP_SRC_ENDPT_OFF, SELF);
            let result = do_umap_handler(rp, &mut msg);
            // Delegates to do_umap_remote -> vm_lookup with zero CR3 -> returns error
            assert!(result != OK, "should fail with no CR3");
        }
    }

    #[test]
    fn test_do_vmctl_invalid_endpoint() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, M1_I1_OFF, 99999);
            msg_write_i32(&mut msg, M1_I2_OFF, 12);
            let result = do_vmctl_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_do_vmctl_clear_pagefault() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let ep = crate::table::make_endpoint(0, 0);
            (*rp).p_endpoint = ep;
            (*rp)
                .p_rts_flags
                .store(RtsFlags::PAGEFAULT.bits(), Ordering::Relaxed);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, M1_I1_OFF, ep); // SVMCTL_WHO
            msg_write_i32(&mut msg, M1_I2_OFF, 12); // VMCTL_CLEAR_PAGEFAULT
            let result = do_vmctl_handler(rp, &mut msg);
            assert_eq!(result, OK);
            let flags = (*rp).p_rts_flags.load(Ordering::Relaxed);
            assert_eq!(
                flags & RtsFlags::PAGEFAULT.bits(),
                0,
                "PAGEFAULT should be cleared"
            );
        }
    }

    #[test]
    fn test_do_memset_handler_zero_count() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_u64(&mut msg, MEMSET_BASE_OFF, 0x1000);
            msg_write_u64(&mut msg, MEMSET_COUNT_OFF, 0);
            msg_write_u64(&mut msg, MEMSET_PATTERN_OFF, 0x42);
            msg_write_i32(&mut msg, MEMSET_PROC_OFF, 0);
            let result = do_memset_handler(rp, &mut msg);
            assert_eq!(result, OK);
        }
    }

    #[test]
    fn test_do_getinfo_handler_whoami() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let ep = crate::table::make_endpoint(0, 0);
            (*rp).p_endpoint = ep;
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, GETINFO_REQUEST_OFF, GET_WHOAMI as i32);
            let result = do_getinfo_handler(rp, &mut msg);
            assert_eq!(result, OK);
        }
    }

    #[test]
    fn test_do_getinfo_handler_invalid_request() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, GETINFO_REQUEST_OFF, 99999);
            let result = do_getinfo_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::ENOSYS);
        }
    }

    #[test]
    fn test_do_sigsend_invalid_endpoint() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, SIGCALLS_ENDPT_OFF, 99999);
            let result = do_sigsend_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_do_sigreturn_invalid_endpoint() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, SIGCALLS_ENDPT_OFF, 99999);
            let result = do_sigreturn_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_do_setgrant_handler() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let priv0 = setup_test_priv(0);
            (*rp).p_priv = priv0;
            (*rp)
                .p_rts_flags
                .fetch_and(!RtsFlags::NO_PRIV.bits(), Ordering::Relaxed);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_u64(&mut msg, SETGRANT_ADDR_OFF, 0x1000);
            msg_write_i32(&mut msg, SETGRANT_SIZE_OFF, 16);
            let result = do_setgrant_handler(rp, &mut msg);
            assert_eq!(result, OK);
            assert_eq!((*priv0).s_grant_table, 0x1000);
            assert_eq!((*priv0).s_grant_entries, 16);
        }
    }
}
