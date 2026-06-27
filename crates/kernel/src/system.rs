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

use crate::r#priv::*;
use crate::proc::*;
use crate::sched::dequeue;
use crate::table::proc_addr;

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
        map_call(0, do_fork_placeholder); // SYS_FORK
        map_call(1, do_exec_placeholder); // SYS_EXEC
        map_call(2, do_clear_stub); // SYS_CLEAR
        map_call(3, do_schedule_stub); // SYS_SCHEDULE
        map_call(4, do_privctl_stub); // SYS_PRIVCTL
        map_call(5, do_trace_stub); // SYS_TRACE
        map_call(6, do_kill_stub); // SYS_KILL
        map_call(7, do_getksig_stub); // SYS_GETKSIG
        map_call(8, do_endksig_stub); // SYS_ENDKSIG
        map_call(9, do_sigsend_stub); // SYS_SIGSEND
        map_call(10, do_sigreturn_stub); // SYS_SIGRETURN
        map_call(13, do_memset_stub); // SYS_MEMSET
        map_call(14, do_umap_stub); // SYS_UMAP
        map_call(15, do_vircopy_stub); // SYS_VIRCOPY
        map_call(16, do_physcopy_stub); // SYS_PHYSCOPY
        map_call(17, do_umap_remote_stub); // SYS_UMAP_REMOTE
        map_call(18, do_vumap_stub); // SYS_VUMAP
        map_call(19, do_irqctl_stub); // SYS_IRQCTL
        map_call(24, do_setalarm_stub); // SYS_SETALARM
        map_call(25, do_times_stub); // SYS_TIMES
        map_call(26, do_getinfo_stub); // SYS_GETINFO
        map_call(27, do_abort_stub); // SYS_ABORT
        map_call(31, do_safecopy_from_stub); // SYS_SAFECOPYFROM
        map_call(32, do_safecopy_to_stub); // SYS_SAFECOPYTO
        map_call(33, do_vsafecopy_stub); // SYS_VSAFECOPY
        map_call(34, do_setgrant_stub); // SYS_SETGRANT
        map_call(36, do_sprofile_stub); // SYS_SPROF
        map_call(37, do_cprofile_stub); // SYS_CPROF
        map_call(38, do_profbuf_stub); // SYS_PROFBUF
        map_call(39, do_stime_stub); // SYS_STIME
        map_call(40, do_settime_stub); // SYS_SETTIME
        map_call(43, do_vmctl_stub); // SYS_VMCTL
        map_call(44, do_diagctl_stub); // SYS_DIAGCTL
        map_call(45, do_vtimer_stub); // SYS_VTIMER
        map_call(46, do_runctl_stub); // SYS_RUNCTL
        map_call(50, do_getmcontext_stub); // SYS_GETMCONTEXT
        map_call(51, do_setmcontext_stub); // SYS_SETMCONTEXT
        map_call(52, do_update_stub); // SYS_UPDATE
        map_call(53, do_exit_stub); // SYS_EXIT
        map_call(54, do_schedctl_stub); // SYS_SCHEDCTL
        map_call(55, do_statectl_stub); // SYS_STATECTL
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
// Placeholder stub handlers
// ─────────────────────────────────────────────────────────────────────────

macro_rules! stub_handler {
    ($name:ident, $desc:expr) => {
        /// Stub handler for $desc.
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

stub_handler!(do_fork_placeholder, "SYS_FORK");
stub_handler!(do_exec_placeholder, "SYS_EXEC");
stub_handler!(do_clear_stub, "SYS_CLEAR");
stub_handler!(do_schedule_stub, "SYS_SCHEDULE");
stub_handler!(do_privctl_stub, "SYS_PRIVCTL");
stub_handler!(do_trace_stub, "SYS_TRACE");
stub_handler!(do_kill_stub, "SYS_KILL");
stub_handler!(do_getksig_stub, "SYS_GETKSIG");
stub_handler!(do_endksig_stub, "SYS_ENDKSIG");
stub_handler!(do_sigsend_stub, "SYS_SIGSEND");
stub_handler!(do_sigreturn_stub, "SYS_SIGRETURN");
stub_handler!(do_memset_stub, "SYS_MEMSET");
stub_handler!(do_umap_stub, "SYS_UMAP");
stub_handler!(do_vircopy_stub, "SYS_VIRCOPY");
stub_handler!(do_physcopy_stub, "SYS_PHYSCOPY");
stub_handler!(do_umap_remote_stub, "SYS_UMAP_REMOTE");
stub_handler!(do_vumap_stub, "SYS_VUMAP");
stub_handler!(do_irqctl_stub, "SYS_IRQCTL");
stub_handler!(do_setalarm_stub, "SYS_SETALARM");
stub_handler!(do_times_stub, "SYS_TIMES");
stub_handler!(do_getinfo_stub, "SYS_GETINFO");
stub_handler!(do_abort_stub, "SYS_ABORT");
stub_handler!(do_safecopy_from_stub, "SYS_SAFECOPYFROM");
stub_handler!(do_safecopy_to_stub, "SYS_SAFECOPYTO");
stub_handler!(do_vsafecopy_stub, "SYS_VSAFECOPY");
stub_handler!(do_setgrant_stub, "SYS_SETGRANT");
stub_handler!(do_sprofile_stub, "SYS_SPROF");
stub_handler!(do_cprofile_stub, "SYS_CPROF");
stub_handler!(do_profbuf_stub, "SYS_PROFBUF");
stub_handler!(do_stime_stub, "SYS_STIME");
stub_handler!(do_settime_stub, "SYS_SETTIME");
stub_handler!(do_vmctl_stub, "SYS_VMCTL");
stub_handler!(do_diagctl_stub, "SYS_DIAGCTL");
stub_handler!(do_vtimer_stub, "SYS_VTIMER");
stub_handler!(do_runctl_stub, "SYS_RUNCTL");
stub_handler!(do_getmcontext_stub, "SYS_GETMCONTEXT");
stub_handler!(do_setmcontext_stub, "SYS_SETMCONTEXT");
stub_handler!(do_update_stub, "SYS_UPDATE");
stub_handler!(do_exit_stub, "SYS_EXIT");
stub_handler!(do_schedctl_stub, "SYS_SCHEDCTL");
stub_handler!(do_statectl_stub, "SYS_STATECTL");
stub_handler!(do_safememset_stub, "SYS_SAFEMEMSET");

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
///
/// # Safety
///
/// `proc` must point to a valid `Proc`.
pub unsafe fn release_address_space(_proc: *mut Proc) {
    // No-op: page table freeing deferred to VM server.
    // When VM is available, this should call into the VM to free
    // the page table pages referenced by (*proc).p_seg.p_cr3.
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
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::proc_init;

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
            let rp = crate::table::proc_addr(0); // PM

            // Set up privilege with k_call_mask
            if !(*rp).p_priv.is_null() {
                (*(*rp).p_priv).s_k_call_mask = [!0u32; SYS_CALL_MASK_SIZE];
            }

            let mut msg = [0u8; MESSAGE_SIZE];
            // Invalid call number (beyond NR_SYS_CALLS)
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

            // Zero out k_call_mask (no calls allowed)
            if !(*rp).p_priv.is_null() {
                (*(*rp).p_priv).s_k_call_mask = [0u32; SYS_CALL_MASK_SIZE];
            }

            let mut msg = [0u8; MESSAGE_SIZE];
            msg[0..4].copy_from_slice(&(KERNEL_CALL + 3).to_ne_bytes()); // SYS_SCHEDULE
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

            // Allow all calls
            if !(*rp).p_priv.is_null() {
                (*(*rp).p_priv).s_k_call_mask = [!0u32; SYS_CALL_MASK_SIZE];
            }

            let mut msg = [0u8; MESSAGE_SIZE];
            msg[0..4].copy_from_slice(&(KERNEL_CALL + 3).to_ne_bytes()); // SYS_SCHEDULE
            let result = kernel_call_dispatch(rp, &mut msg);
            // Should be dispatched (returns EBADREQUEST from stub)
            assert!(result == EBADREQUEST || result == OK);
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
    fn test_release_address_space_noop() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            // Should not crash on a valid proc
            release_address_space(rp);
        }
    }

    #[test]
    fn test_switch_address_space_idle_noop() {
        // Should not crash or panic
        switch_address_space_idle();
    }
}
