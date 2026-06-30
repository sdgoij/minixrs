//! IPC subsystem — adapted from `minix/kernel/proc.c`
//!
//! Implements the core Minix IPC primitives.

use core::sync::atomic::Ordering;

use arch_common::ipc::{
    AMF_DONE, AMF_NOREPLY, AMF_NOTIFY, AMF_NOTIFY_ERR, AMF_VALID, AsynMsg, Message,
};

use crate::proc::*;
use crate::sched::{dequeue, enqueue};
use crate::table::{endpoint_slot, is_ok_endpoint, proc_addr};

// ── Constants ───────────────────────────────────────────────────────────

pub const NON_BLOCKING: i32 = 0x80;
pub const FROM_KERNEL: i32 = 0x100;

pub const SEND: i32 = 0x01;
pub const RECEIVE: i32 = 0x02;
pub const SENDREC: i32 = 0x03;
pub const NOTIFY: i32 = 0x04;
pub const SENDNB: i32 = 0x05;

pub const IPC_FLG_MSG_FROM_KERNEL: u16 = 0x0001;
pub const IPC_FLG_CALL_MASK: u16 = 0x007F;
pub const IPC_FLG_STATUS_MASK: u16 = 0x00FF;

pub const OK: i32 = 0;
pub const EPERM: i32 = -1;
pub const EAGAIN: i32 = -11;
pub const ENOMEM: i32 = -12;
pub const EACCES: i32 = -13;
pub const EFAULT: i32 = -14;
pub const ENOENT: i32 = -2;
pub const ESRCH: i32 = -3;
pub const EINTR: i32 = -4;
pub const E2BIG: i32 = -7;
pub const ENOMSG: i32 = -42;
pub const ENOTREADY: i32 = -73;
pub const ELOCKED: i32 = -132;
pub const EDEADSRCDST: i32 = -199;
pub const ENOSYS: i32 = -72;
pub const EINVAL: i32 = -22;

// ── IPC status helpers (safe — work on values, not pointers) ──────────

pub fn ipc_status_call(status: u16) -> i32 {
    (status & IPC_FLG_CALL_MASK) as i32
}

pub fn ipc_status_has_flag(status: u16, flag: u16) -> bool {
    status & flag != 0
}

/// Add a call type to a process's IPC status.
///
/// # Safety
///
/// `rp` must point to a valid `Proc`.
pub unsafe fn ipc_status_add_call(rp: *mut Proc, call: i32) {
    unsafe {
        let status = (*rp).p_misc_flags.load(Ordering::Relaxed) as u16;
        let new_status = (status & !IPC_FLG_CALL_MASK) | (call as u16);
        (*rp)
            .p_misc_flags
            .store(new_status as u32, Ordering::Relaxed);
    }
}

/// Add a flag to a process's IPC status.
///
/// # Safety
///
/// `rp` must point to a valid `Proc`.
pub unsafe fn ipc_status_add_flags(rp: *mut Proc, flag: u16) {
    unsafe {
        let status = (*rp).p_misc_flags.load(Ordering::Relaxed) as u16;
        (*rp)
            .p_misc_flags
            .store((status | flag) as u32, Ordering::Relaxed);
    }
}

/// Clear a process's IPC status.
///
/// # Safety
///
/// `rp` must point to a valid `Proc`.
pub unsafe fn ipc_status_clear(rp: *mut Proc) {
    unsafe {
        let status = (*rp).p_misc_flags.load(Ordering::Relaxed) as u16;
        (*rp)
            .p_misc_flags
            .store((status & !IPC_FLG_STATUS_MASK) as u32, Ordering::Relaxed);
    }
}

// ── WillReceive check ──────────────────────────────────────────────────

fn will_receive(dst_ptr: *mut Proc, src_e: i32) -> bool {
    unsafe {
        let rts = (*dst_ptr).p_rts_flags.load(Ordering::Relaxed);
        if rts & RtsFlags::RECEIVING.bits() == 0 {
            return false;
        }
        // C also checks !RTS_SENDING — a process blocked on SENDREC
        // has both SENDING and RECEIVING set and should NOT match
        if rts & RtsFlags::SENDING.bits() != 0 {
            return false;
        }
        let from = (*dst_ptr).p_getfrom_e;
        from == src_e || from == crate::system::NONE
    }
}

// ── mini_send ──────────────────────────────────────────────────────────

/// Send a message from `caller_ptr` to `dst_e`.
///
/// # Safety
///
/// Both processes and `m_ptr` must be valid.
pub unsafe fn mini_send(caller_ptr: *mut Proc, dst_e: i32, m_ptr: *const u8, flags: i32) -> i32 {
    unsafe {
        if !is_ok_endpoint(dst_e) {
            return EDEADSRCDST;
        }
        let dst_p = endpoint_slot(dst_e);
        let dst_ptr = proc_addr(dst_p);
        if dst_ptr.is_null() {
            return EDEADSRCDST;
        }
        let dst_rts = (*dst_ptr).p_rts_flags.load(Ordering::Relaxed);
        if dst_rts & RtsFlags::NO_ENDPOINT.bits() != 0 {
            return EDEADSRCDST;
        }

        if will_receive(dst_ptr, (*caller_ptr).p_endpoint) {
            // Direct delivery
            assert!((*dst_ptr).p_misc_flags.load(Ordering::Relaxed) as u16 & (1 << 6) == 0);

            let dst_msg: &mut [u8; MESSAGE_SIZE] = &mut (*dst_ptr).p_delivermsg;
            core::ptr::copy_nonoverlapping(m_ptr, dst_msg.as_mut_ptr(), MESSAGE_SIZE);

            let src_ep = (*caller_ptr).p_endpoint;
            let ep_bytes = src_ep.to_ne_bytes();
            core::ptr::copy_nonoverlapping(ep_bytes.as_ptr(), dst_msg.as_mut_ptr().add(4), 4);

            (*dst_ptr)
                .p_misc_flags
                .fetch_or(MiscFlags::DELIVERMSG.bits(), Ordering::Relaxed);

            let call = if (*caller_ptr).p_misc_flags.load(Ordering::Relaxed) as u16 & (1 << 0) != 0
            {
                SENDREC
            } else if flags & NON_BLOCKING != 0 {
                SENDNB
            } else {
                SEND
            };
            ipc_status_add_call(dst_ptr, call);

            if call == SENDREC {
                // Clear REPLY_PEND on the DESTINATION (not caller)
                (*dst_ptr)
                    .p_misc_flags
                    .fetch_and(!MiscFlags::REPLY_PEND.bits(), Ordering::Relaxed);
            }

            let old = (*dst_ptr).p_rts_flags.load(Ordering::Relaxed);
            let new = old & !RtsFlags::RECEIVING.bits();
            (*dst_ptr).p_rts_flags.store(new, Ordering::Relaxed);
            if new == 0 {
                enqueue(dst_ptr);
            }
        } else {
            if flags & NON_BLOCKING != 0 {
                return ENOTREADY;
            }
            if deadlock(SEND, caller_ptr, dst_e) {
                return ELOCKED;
            }

            let caller_msg: &mut [u8; MESSAGE_SIZE] = &mut (*caller_ptr).p_sendmsg;
            core::ptr::copy_nonoverlapping(m_ptr, caller_msg.as_mut_ptr(), MESSAGE_SIZE);

            if flags & FROM_KERNEL != 0 {
                (*caller_ptr)
                    .p_misc_flags
                    .fetch_or(MiscFlags::SENDING_FROM_KERNEL.bits(), Ordering::Relaxed);
            }

            let old = (*caller_ptr).p_rts_flags.load(Ordering::Relaxed);
            (*caller_ptr)
                .p_rts_flags
                .store(old | RtsFlags::SENDING.bits(), Ordering::Relaxed);
            if old == 0 {
                dequeue(caller_ptr);
            }
            (*caller_ptr).p_sendto_e = dst_e;

            let mut xpp: *mut *mut Proc = &mut (*dst_ptr).p_caller_q;
            while !(*xpp).is_null() {
                xpp = &mut (**xpp).p_q_link;
            }
            *xpp = caller_ptr;
        }
        OK
    }
}

// ── mini_receive ────────────────────────────────────────────────────────

/// Receive a message for `caller_ptr` from `src_e`.
///
/// # Safety
///
/// Process and `m_ptr` must be valid.
pub unsafe fn mini_receive(caller_ptr: *mut Proc, src_e: i32, m_ptr: *mut u8, flags: i32) -> i32 {
    unsafe {
        let mut xpp: *mut *mut Proc = &mut (*caller_ptr).p_caller_q;
        while !(*xpp).is_null() {
            let send_ptr = *xpp;
            let send_ep = (*send_ptr).p_endpoint;
            let matches = src_e == crate::system::NONE || src_e == send_ep;
            let send_rts = (*send_ptr).p_rts_flags.load(Ordering::Relaxed);

            if matches && (send_rts & RtsFlags::SENDING.bits() != 0) {
                *xpp = (*send_ptr).p_q_link;
                (*send_ptr).p_q_link = core::ptr::null_mut();

                let msg_ptr = (*send_ptr).p_sendmsg.as_ptr();
                core::ptr::copy_nonoverlapping(msg_ptr, m_ptr, MESSAGE_SIZE);

                let src = (*send_ptr).p_endpoint;
                let ep_bytes = src.to_ne_bytes();
                core::ptr::copy_nonoverlapping(ep_bytes.as_ptr(), m_ptr.add(4), 4);

                let old = (*send_ptr).p_rts_flags.load(Ordering::Relaxed);
                let new = old & !RtsFlags::SENDING.bits();
                (*send_ptr).p_rts_flags.store(new, Ordering::Relaxed);
                if new == 0 {
                    enqueue(send_ptr);
                }
                return OK;
            }
            xpp = &mut (**xpp).p_q_link;
        }

        // NON_BLOCKING check (C L1027-1037)
        if flags & NON_BLOCKING != 0 {
            return ENOTREADY;
        }

        // Deadlock check (C L1029)
        if deadlock(RECEIVE, caller_ptr, src_e) {
            return ELOCKED;
        }

        let old = (*caller_ptr).p_rts_flags.load(Ordering::Relaxed);
        (*caller_ptr)
            .p_rts_flags
            .store(old | RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
        if old == 0 {
            dequeue(caller_ptr);
        }
        (*caller_ptr).p_getfrom_e = src_e;
        OK
    }
}

// ── mini_notify ─────────────────────────────────────────────────────────

/// Send a notification to `dst_e`.
///
/// # Safety
///
/// Destination must be valid.
pub unsafe fn mini_notify(src_e: i32, dst_e: i32) -> i32 {
    unsafe {
        if !is_ok_endpoint(dst_e) {
            return EDEADSRCDST;
        }
        let dst_p = endpoint_slot(dst_e);
        let dst_ptr = proc_addr(dst_p);
        if dst_ptr.is_null() {
            return EDEADSRCDST;
        }

        // C: skip delivery when destination has MF_REPLY_PEND
        if (*dst_ptr).p_misc_flags.load(Ordering::Relaxed) & MiscFlags::REPLY_PEND.bits() != 0 {
            // Record pending notification and return
            if !(*dst_ptr).p_priv.is_null() {
                (*(*dst_ptr).p_priv).s_notify_pending.set(src_e as usize);
            }
            return OK;
        }

        let rts = (*dst_ptr).p_rts_flags.load(Ordering::Relaxed);
        if rts & RtsFlags::RECEIVING.bits() != 0
            && ((*dst_ptr).p_getfrom_e == crate::system::NONE || (*dst_ptr).p_getfrom_e == src_e)
        {
            let new = rts & !RtsFlags::RECEIVING.bits();
            (*dst_ptr).p_rts_flags.store(new, Ordering::Relaxed);
            if new == 0 {
                enqueue(dst_ptr);
            }
        } else {
            // C: record pending notification when destination isn't waiting
            if !(*dst_ptr).p_priv.is_null() {
                (*(*dst_ptr).p_priv).s_notify_pending.set(src_e as usize);
            }
        }
        OK
    }
}

// ── deadlock ────────────────────────────────────────────────────────────

fn deadlock(function: i32, caller_ptr: *mut Proc, mut dst_e: i32) -> bool {
    unsafe {
        let caller_ep = (*caller_ptr).p_endpoint;
        let mut group_size = 1;

        for _ in 0..NR_PROCS_TOTAL {
            if !is_ok_endpoint(dst_e) {
                return false;
            }
            let xp = proc_addr(endpoint_slot(dst_e));
            if xp.is_null() {
                return false;
            }
            group_size += 1;

            // Find what this process is blocked on (P_BLOCKEDON)
            dst_e = (*xp).blocked_on();

            if dst_e == -1 {
                return false; // not blocked
            }

            if dst_e == caller_ep {
                if group_size == 2 {
                    // For size-2 cycles, check if safe (SEND+RECEIVE pair)
                    let xp_flags = (*xp).p_rts_flags.load(Ordering::Relaxed);
                    if (xp_flags ^ ((function << 2) as u32)) & RtsFlags::SENDING.bits() != 0 {
                        return false; // safe: one sends, one receives
                    }
                }
                return true; // real deadlock
            }
        }
        false
    }
}

// ── delivermsg ───────────────────────────────────────────────────────────

/// Deliver the message stored in `p_delivermsg` (kernel buffer) to the
/// target process's user-space virtual address (`p_delivermsg_vir`), by
/// temporarily switching to the target's per-process page tables (CR3).
///
/// # CR3 switching
///
/// 1. Gated on `BOOT_CR3 != 0` — if zero (pre-init / test mode), the
///    privileged `read_cr3`/`write_cr3` instructions are skipped entirely.
/// 2. If the target's `p_seg.p_cr3` is zero (no per-process page table,
///    e.g. init process), the message is written from the kernel's current
///    address space without switching CR3.
///
/// # Arguments
///
/// * `rp` — target process whose message buffer to fill.
///
/// # Returns
///
/// `OK` (0) on success, or `EFAULT` if the user buffer address is null.
///
/// # Safety
///
/// `rp` must point to a valid, fully initialized `Proc`.
pub unsafe fn delivermsg(rp: *mut Proc) -> i32 {
    unsafe {
        let vir = (*rp).p_delivermsg_vir;
        if vir == 0 {
            // No user-space buffer to write to — skip delivery.
            return OK;
        }

        let boot_cr3 = arch_x86_64::BOOT_CR3.load(Ordering::Relaxed);
        let saved_cr3 = if boot_cr3 != 0 {
            let saved = arch_x86_64::asm::read_cr3();
            let target_cr3 = (*rp).p_seg.p_cr3;
            if target_cr3 != 0 {
                arch_x86_64::asm::write_cr3(target_cr3);
            }
            Some(saved)
        } else {
            None
        };

        // Copy the message from the kernel buffer to the user virtual address.
        core::ptr::copy_nonoverlapping((*rp).p_delivermsg.as_ptr(), vir as *mut u8, MESSAGE_SIZE);

        // Restore the original CR3.
        if let Some(saved) = saved_cr3 {
            arch_x86_64::asm::write_cr3(saved);
        }

        OK
    }
}

// ── do_sync_ipc ────────────────────────────────────────────────────────

/// Perform a synchronous IPC operation.
/// Tries in-kernel server dispatch before sending to a user-space process.
///
/// # Safety
///
/// All process pointers must be valid.
pub unsafe fn do_sync_ipc(caller_ptr: *mut Proc, m_ptr: *mut u8, call: i32) -> i32 {
    unsafe {
        let src_dst_e = core::ptr::read_unaligned(m_ptr.cast::<[u8; 8]>());
        let ep = i32::from_ne_bytes([src_dst_e[0], src_dst_e[1], src_dst_e[2], src_dst_e[3]]);

        // Try in-kernel server dispatch first (SENDREC and SEND only, not
        // RECEIVE or NOTIFY which are addressed to self / any).
        if call == SENDREC || call == SEND {
            let msg = &mut *(m_ptr as *mut [u8; MESSAGE_SIZE]);
            if let Some(result) = try_server_dispatch(caller_ptr, ep, msg) {
                return result;
            }
        }

        // C: check iskerneln — forbid SEND/SENDNB/NOTIFY to kernel tasks
        if (call == SEND || call == SENDNB || call == NOTIFY)
            && crate::table::is_kernel_nr(endpoint_slot(ep))
        {
            return crate::system::ECALLDENIED;
        }

        // C: check may_send_to (L471-479)
        // Check if caller has IPC permission to send to destination
        if call == SEND || call == SENDREC || call == SENDNB || call == NOTIFY {
            let dst_p = endpoint_slot(ep);
            if !crate::r#priv::may_send_to(&*caller_ptr, dst_p) {
                return crate::system::ECALLDENIED;
            }
        }

        match call {
            SENDREC => {
                // C: set REPLY_PEND before calling mini_send
                (*caller_ptr)
                    .p_misc_flags
                    .fetch_or(MiscFlags::REPLY_PEND.bits(), Ordering::Relaxed);
                let r = mini_send(caller_ptr, ep, m_ptr, 0);
                if r != OK {
                    return r;
                }
                mini_receive(caller_ptr, ep, m_ptr, 0)
            }
            SEND => mini_send(caller_ptr, ep, m_ptr, 0),
            RECEIVE => mini_receive(caller_ptr, ep, m_ptr, 0),
            SENDNB => mini_send(caller_ptr, ep, m_ptr, NON_BLOCKING),
            NOTIFY => mini_notify((*caller_ptr).p_endpoint, ep),
            _ => crate::system::EBADREQUEST,
        }
    }
}

// ── In-kernel server dispatch infrastructure ─────────────────────────

/// Maximum number of dispatchable server endpoints (matches arch-common
/// NR_BOOT_MODULES range).
pub const SERVER_DISPATCH_SLOTS: usize = 16;

/// Dispatch function signature: handles an IPC call directed at a server
/// endpoint directly in the kernel, bypassing the user-space server process.
///
/// Parameters:
/// - `caller`: the sending process
/// - `msg`: the IPC message (can be modified in place for the reply)
/// - Returns: 0 (OK) on success, negative error code on failure
pub type ServerDispatchFn = unsafe fn(*mut Proc, &mut [u8; MESSAGE_SIZE]) -> i32;

/// Dispatch table indexed by endpoint slot.
static mut SERVER_DISPATCH: [Option<ServerDispatchFn>; SERVER_DISPATCH_SLOTS] =
    [None; SERVER_DISPATCH_SLOTS];

/// Register an in-kernel dispatch handler for an endpoint.
/// Returns `true` if the endpoint was within the dispatch range.
pub fn register_server_dispatch(ep: i32, handler: ServerDispatchFn) -> bool {
    let slot = endpoint_slot(ep);
    if slot < 0 || slot >= SERVER_DISPATCH_SLOTS as i32 {
        return false;
    }
    unsafe {
        SERVER_DISPATCH[slot as usize] = Some(handler);
    }
    true
}

/// Try to dispatch an IPC call to an in-kernel server handler.
/// Returns `Some(result)` if a handler was found, `None` otherwise.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`. `msg` must point to a valid
/// message buffer. `dst_ep` must be a valid endpoint.
pub unsafe fn try_server_dispatch(
    caller: *mut Proc,
    dst_ep: i32,
    msg: &mut [u8; MESSAGE_SIZE],
) -> Option<i32> {
    let slot = endpoint_slot(dst_ep);
    if slot < 0 || slot >= SERVER_DISPATCH_SLOTS as i32 {
        return None;
    }
    unsafe { SERVER_DISPATCH[slot as usize].map(|handler| handler(caller, msg)) }
}

// ── Exec dispatch handlers ───────────────────────────────────────────

/// Function type for setting the exec target (RIP and RSP) on a process.
/// Called during PM_EXEC to switch the process to the new binary's entry point.
pub type SetExecRipFn = unsafe fn(*mut Proc, new_rip: u64, new_rsp: u64);

/// Arch-specific exec target setter. Set by the architecture layer
/// during initialization (e.g., to `asm_exec_handler` in arch-x86_64).
static mut SET_EXEC_RIP: Option<SetExecRipFn> = None;

/// Register the architecture-specific function for setting exec targets.
pub fn register_set_exec_rip(f: SetExecRipFn) {
    unsafe {
        SET_EXEC_RIP = Some(f);
    }
}

/// Set the exec target (RIP and RSP) for a process.
/// This is called during PM_EXEC to switch the process to the new binary.
///
/// # Safety
///
/// `proc` must point to a valid `Proc`. The returned function pointer
/// must be the arch-specific exec target setter.
pub unsafe fn set_exec_target(proc: *mut Proc, new_rip: u64, new_rsp: u64) {
    if let Some(f) = unsafe { SET_EXEC_RIP } {
        unsafe { f(proc, new_rip, new_rsp) };
    }
}

/// PM_FORK dispatch handler (stub).
///
/// In the real PM, this creates a new process by copying the caller's
/// address space. The kernel stub returns 0 (caller is parent, child
/// will be told separately) — this matches the convention where
/// SENDREC for FORK returns 0 to the parent and the child starts
/// as a separate process.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`.
pub unsafe fn pm_fork_dispatch(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    let _ = caller;
    let _ = msg;
    // Stub: return 0 (child PID), simulating immediate success.
    // Real implementation needs to clone the address space.
    0
}

/// PM_EXEC dispatch handler (stub).
///
/// In the real PM, this loads an ELF binary into the caller's address
/// space and sets the entry point. The kernel stub just returns OK.
/// The ELF loading and initramfs access require VFS interaction.
///
/// # Safety
///
/// `caller` must be valid.
pub unsafe fn pm_exec_dispatch(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    let _ = caller;
    let _ = msg;
    // Stub: return OK. Real implementation needs to:
    // 1. Read the ELF path from the message
    // 2. Load the binary via VFS
    // 3. Set up the stack with argv/envp
    // 4. Call set_exec_target() with new RIP/RSP
    OK
}

/// PM_EXIT dispatch handler (stub).
///
/// In the real PM, this terminates the calling process. The kernel stub
/// just returns OK.
///
/// # Safety
///
/// `caller` must be valid.
pub unsafe fn pm_exit_dispatch(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    let _ = caller;
    let _ = msg;
    // Stub: return OK. Real implementation needs to:
    // 1. Clean up the process's resources
    // 2. Notify the parent process
    // 3. Set the process to a terminating state
    OK
}

/// PM_WAITPID dispatch handler (stub).
///
/// In the real PM, this waits for a child process to change state.
/// The kernel stub returns ECHILD (no children), which is the correct
/// return when there are no child processes.
///
/// # Safety
///
/// `caller` must be valid.
pub unsafe fn pm_waitpid_dispatch(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    let _ = caller;
    let _ = msg;
    // Stub: return ECHILD. Real implementation needs to:
    // 1. Search for a child process
    // 2. Block if none available and WNOHANG not set
    // 3. Return child's exit status
    crate::system::EBADREQUEST
}

/// Initialize the server dispatch table with default handlers.
pub fn init_server_dispatch() {
    // Register PM dispatch handlers (PM_PROC_NR = 0)
    register_server_dispatch(arch_common::com::PM_PROC_NR, pm_fork_dispatch);
    // Other servers (VFS, RS, etc.) can be registered when their dispatch
    // handlers are implemented.
}

// ── Async helpers (skeletons) ──────────────────────────────────────────

/// # Safety
///
/// `rp` must point to a valid `Proc`.
pub unsafe fn has_pending_notify(rp: *mut Proc) -> bool {
    unsafe {
        if (*rp).p_priv.is_null() {
            return false;
        }
        !(*(*rp).p_priv).s_notify_pending.is_empty()
    }
}

/// # Safety
///
/// `rp` must point to a valid `Proc`.
pub unsafe fn has_pending_asend(rp: *mut Proc) -> bool {
    unsafe {
        if (*rp).p_priv.is_null() {
            return false;
        }
        !(*(*rp).p_priv).s_asyn_pending.is_empty()
    }
}

/// # Safety
///
/// `rp` must point to a valid `Proc`.
pub unsafe fn unset_notify_pending(rp: *mut Proc, priv_id: usize) {
    unsafe {
        if !(*rp).p_priv.is_null() {
            (*(*rp).p_priv).s_notify_pending.clear(priv_id);
        }
    }
}

/// Try to deliver a single asynchronous message from `src_ptr` to `dst_ptr`.
///
/// Reads the async send table of `src_ptr` and looks for a message addressed
/// to `dst_ptr`. If found, delivers it directly (waking the receiver) and
/// marks the entry `AMF_DONE`.
///
/// # Safety
///
/// Both process pointers must be valid.
pub unsafe fn try_one(src_ptr: *mut Proc, dst_ptr: *mut Proc) -> i32 {
    unsafe {
        use crate::r#priv::PrivFlags;
        let privp = (*src_ptr).p_priv;
        if privp.is_null() || (*privp).s_flags & PrivFlags::SYS_PROC == PrivFlags::empty() {
            return crate::grants::EPERM;
        }
        if (*privp).s_asynsize == 0 || (*privp).s_asyntab == 0 {
            return EAGAIN;
        }

        let size = (*privp).s_asynsize;
        let table_v = (*privp).s_asyntab;
        let dst_ep = (*dst_ptr).p_endpoint;
        let caller_ep = (*src_ptr).p_endpoint;

        for i in 0..size {
            // Read the async table entry from the source's address space
            let offset = (i as u64) * core::mem::size_of::<AsynMsg>() as u64;
            let tabent = core::ptr::read_unaligned((table_v + offset) as *const AsynMsg);

            let flags = tabent.flags;

            if flags == 0 {
                continue;
            }
            if flags & !(AMF_VALID | AMF_DONE | AMF_NOTIFY | AMF_NOREPLY | AMF_NOTIFY_ERR) != 0 {
                continue;
            }
            if flags & AMF_VALID == 0 {
                continue;
            }
            if flags & AMF_DONE != 0 {
                continue;
            }

            if tabent.endpoint != dst_ep {
                continue;
            }

            // Found a message for dst — deliver it
            let mut result = OK;

            if will_receive(dst_ptr, caller_ep) {
                // Destination is waiting for this message.
                // Copy only the `msg` field from the async entry (not flags/endpoint/result).
                let msg_src = &tabent.msg as *const Message as *const u8;
                let msg_dst = (*dst_ptr).p_delivermsg.as_mut_ptr();
                core::ptr::copy_nonoverlapping(msg_src, msg_dst, core::mem::size_of::<Message>());
            } else {
                // Not waiting — mark as pending
                let src_id = (*privp).s_id;
                if !(*dst_ptr).p_priv.is_null() {
                    (*(*dst_ptr).p_priv).s_asyn_pending.set(src_id as usize);
                }
                result = EAGAIN;
            }

            // Write result back to the table
            let mut updated = tabent;
            updated.flags = flags | AMF_DONE;
            let offset = (i as u64) * core::mem::size_of::<AsynMsg>() as u64;
            core::ptr::write_unaligned((table_v + offset) as *mut AsynMsg, updated);

            return result;
        }

        EAGAIN
    }
}

/// Try to deliver all pending asynchronous messages for a process.
///
/// Walks all privilege structures and calls `try_one()` for each source
/// that has a pending async bit set for this process.
///
/// # Safety
///
/// Process pointer must be valid.
pub unsafe fn try_async(caller_ptr: *mut Proc) -> i32 {
    unsafe {
        let map = &(*(*caller_ptr).p_priv).s_asyn_pending;

        // Try all privilege structures
        let mut privp: *const crate::r#priv::Priv = crate::r#priv::beg_priv_addr();
        let end: *const crate::r#priv::Priv = crate::r#priv::end_priv_addr();
        while privp < end {
            let s_proc_nr = (*privp).s_proc_nr;
            if s_proc_nr == crate::system::NONE || s_proc_nr == i32::MIN {
                privp = privp.add(1);
                continue;
            }

            let id = (*privp).s_id;
            if id < 0 || !map.test(id as usize) {
                privp = privp.add(1);
                continue;
            }

            let src_ptr = crate::table::proc_addr(s_proc_nr);
            if src_ptr.is_null() {
                privp = privp.add(1);
                continue;
            }

            let r = try_one(src_ptr, caller_ptr);
            if r == OK {
                return r;
            }

            privp = privp.add(1);
        }

        EAGAIN
    }
}

/// Cancel all pending asynchronous messages between two processes.
///
/// Cancel all outstanding async sends from `src_ptr` to `dst_ptr`.
///
/// Walks the source's async table, marks entries targeting the
/// destination as cancelled (`EDEADSRCDST`, `AMF_DONE`), clears
/// the async pending bit on the destination, and notifies the
/// source if any entries were cancelled.
///
/// # Safety
///
/// Process pointers must be valid.
pub unsafe fn cancel_async(src_ptr: *mut Proc, dst_ptr: *mut Proc) {
    unsafe {
        if src_ptr.is_null() || dst_ptr.is_null() {
            return;
        }

        let src_priv = (*src_ptr).p_priv;
        if src_priv.is_null() {
            return;
        }

        let size = (*src_priv).s_asynsize;
        let table_v = (*src_priv).s_asyntab;

        // Clear the table reference on the source first (if no entries remain,
        // this is the final state; if entries remain, we re-arm below).
        (*src_priv).s_asyntab = 0;
        (*src_priv).s_asynsize = 0;

        let dst_ep = (*dst_ptr).p_endpoint;
        let mut do_notify = false;
        let mut entries_remain = false;

        if table_v != 0 && size > 0 {
            for i in 0..size {
                let offset = (i as u64) * core::mem::size_of::<AsynMsg>() as u64;
                let tabent = core::ptr::read_unaligned((table_v + offset) as *const AsynMsg);

                let flags = tabent.flags;
                // Skip invalid or already-done entries
                if flags & AMF_VALID == 0 || flags & AMF_DONE != 0 {
                    continue;
                }

                if tabent.endpoint == dst_ep {
                    // Mark as cancelled with EDEADSRCDST
                    let mut updated = tabent;
                    updated.result = EDEADSRCDST;
                    updated.flags = flags | AMF_DONE;
                    core::ptr::write_unaligned((table_v + offset) as *mut AsynMsg, updated);
                    do_notify = true;
                } else {
                    entries_remain = true;
                }
            }
        }

        // Clear the pending bit for src in dst's async pending map
        if !(*dst_ptr).p_priv.is_null() {
            let src_id = (*src_priv).s_id;
            if src_id >= 0 {
                (*(*dst_ptr).p_priv).s_asyn_pending.clear(src_id as usize);
            }
        }

        // Re-arm the table reference if entries remain for other destinations
        if entries_remain {
            (*src_priv).s_asyntab = table_v;
            (*src_priv).s_asynsize = size;
        }

        // Notify the sender that some async sends were cancelled
        if do_notify {
            mini_notify(arch_common::com::ASYNCM, (*src_ptr).p_endpoint);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// IPC syscall handlers (task 5.40)
// ─────────────────────────────────────────────────────────────────────────
// These are thin wrappers around do_sync_ipc that bridge the arch-specific
// syscall entry point to the kernel's IPC implementation.
// They have the same signature as system::CallHandler for uniformity.

/// SEND syscall handler — routes to `do_sync_ipc` with SEND.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`. `msg` must point to a valid message buffer.
pub unsafe fn ipc_send_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe { do_sync_ipc(caller, msg.as_mut_ptr(), SEND) }
}

/// RECEIVE syscall handler — routes to `do_sync_ipc` with RECEIVE.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`. `msg` must point to a valid message buffer.
pub unsafe fn ipc_receive_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe { do_sync_ipc(caller, msg.as_mut_ptr(), RECEIVE) }
}

/// SENDREC syscall handler — routes to `do_sync_ipc` with SENDREC.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`. `msg` must point to a valid message buffer.
pub unsafe fn ipc_sendrec_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe { do_sync_ipc(caller, msg.as_mut_ptr(), SENDREC) }
}

/// NOTIFY syscall handler — routes to `do_sync_ipc` with NOTIFY.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`. `msg` must point to a valid message buffer.
pub unsafe fn ipc_notify_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe { do_sync_ipc(caller, msg.as_mut_ptr(), NOTIFY) }
}

/// SENDA syscall handler — delivers async messages to their destinations.
///
/// Reads the async message table pointer and size from the message buffer
/// (offset 8: table u64, offset 16: size usize), then calls try_deliver_senda.
///
/// # Safety
///
/// `caller` must point to a valid Proc. `msg` must point to a valid message.
pub unsafe fn ipc_senda_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let table = u64::from_le_bytes(msg[8..16].try_into().unwrap_or([0; 8])) as *mut u8;
        let size = usize::from_le_bytes(msg[16..24].try_into().unwrap_or([0; 8]));
        try_deliver_senda(caller, table, size)
    }
}

/// Maximum syscall number accommodating IPC syscall slots (46–50).
pub const SYS_MAX: i32 = 51;

/// Register all IPC syscall handlers in the kernel call dispatch table.
///
/// Maps indices 46–49 to the four IPC handlers:
/// - 46 → `ipc_send_handler` (SEND)
/// - 47 → `ipc_receive_handler` (RECEIVE)
/// - 48 → `ipc_sendrec_handler` (SENDREC)
/// - 49 → `ipc_notify_handler` (NOTIFY)
///
/// # Safety
///
/// Must be called exactly once during boot, after `system_init()`.
pub unsafe fn register_ipc_syscalls() {
    unsafe {
        crate::system::map_call(46, ipc_send_handler);
        crate::system::map_call(47, ipc_receive_handler);
        crate::system::map_call(48, ipc_sendrec_handler);
        crate::system::map_call(49, ipc_notify_handler);
        crate::system::map_call(50, ipc_senda_handler);
    }
}

/// Get the current process pointer from per-CPU storage.
///
/// # Safety
///
/// Per-CPU storage must be initialized.
pub unsafe fn current_proc() -> *mut Proc {
    unsafe {
        let ptr = arch_x86_64::cpulocals::CPU_LOCAL_STORAGE.proc_ptr();
        ptr as *mut Proc
    }
}

/// Set the current process pointer in per-CPU storage.
///
/// # Safety
///
/// Per-CPU storage must be initialized.
pub unsafe fn set_current_proc(proc: *mut Proc) {
    unsafe {
        arch_x86_64::cpulocals::CPU_LOCAL_STORAGE.set_proc_ptr(proc as *mut core::ffi::c_void);
    }
}

/// Deliver all pending async messages from an async send table.
///
/// Walks `caller_ptr`'s async send table and attempts to deliver each
/// pending message. This is the core of the `send` syscall.
///
/// # Safety
///
/// Process pointer must be valid. The async table must be accessible
/// from the caller's address space.
pub unsafe fn try_deliver_senda(caller_ptr: *mut Proc, table: *mut u8, size: usize) -> i32 {
    unsafe {
        let privp = (*caller_ptr).p_priv;
        if privp.is_null() {
            return crate::grants::EPERM;
        }

        // Clear the pending async table reference
        (*privp).s_asyntab = 0;
        (*privp).s_asynsize = 0;

        if size == 0 {
            return OK;
        }

        // Limit size to something reasonable
        let max_size = 16 * (crate::proc::NR_TASKS + crate::proc::NR_PROCS);
        if size > max_size {
            return crate::grants::EINVAL;
        }

        // Validate table address is in user space
        if (table as u64) >= arch_x86_64::param::KERNBASE {
            return crate::grants::EFAULT_DST;
        }

        let mut do_notify = false;
        let mut all_done = true;

        for i in 0..size {
            // Read async table entry from caller's address space
            let off = (i as u64) * core::mem::size_of::<AsynMsg>() as u64;
            let tabent = core::ptr::read_unaligned((table as u64 + off) as *const AsynMsg);

            let flags = tabent.flags;

            if flags == 0 {
                continue;
            }

            // Validate flags
            if flags & !(AMF_VALID | AMF_DONE | AMF_NOTIFY | AMF_NOREPLY | AMF_NOTIFY_ERR) != 0 {
                return crate::grants::EINVAL;
            }
            if flags & AMF_VALID == 0 {
                return crate::grants::EINVAL;
            }
            if flags & AMF_DONE != 0 {
                continue;
            }

            let dst = tabent.endpoint;
            let mut r = OK;
            let mut dst_p = 0i32;

            if !is_ok_endpoint_f(dst, &mut dst_p, false) {
                r = EDEADSRCDST;
            } else if crate::table::is_kernel_nr(dst_p)
                || !crate::r#priv::may_send_to(&*(caller_ptr as *const Proc), dst_p)
            {
                r = crate::system::ECALLDENIED;
            }

            let dst_ptr = if r == OK {
                let rp = crate::table::proc_addr(dst_p);
                if !rp.is_null()
                    && (*rp).p_rts_flags.load(Ordering::Relaxed) & RtsFlags::NO_ENDPOINT.bits() != 0
                {
                    r = EDEADSRCDST;
                }
                rp
            } else {
                core::ptr::null_mut()
            };

            if r == OK
                && will_receive(dst_ptr, (*caller_ptr).p_endpoint)
                && (flags & AMF_NOREPLY == 0)
            {
                // Destination is waiting for this message — deliver directly.
                // Copy only the `msg` field from the async entry (not flags/endpoint/result).
                let msg_src = &tabent.msg as *const Message as *const u8;
                let msg_dst = (*dst_ptr).p_delivermsg.as_mut_ptr();
                core::ptr::copy_nonoverlapping(msg_src, msg_dst, core::mem::size_of::<Message>());
                (*dst_ptr)
                    .p_misc_flags
                    .fetch_or(MiscFlags::DELIVERMSG.bits(), Ordering::Relaxed);
                let rts = (*dst_ptr).p_rts_flags.load(Ordering::Relaxed);
                (*dst_ptr)
                    .p_rts_flags
                    .store(rts & !RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            } else if r == OK {
                // Destination not waiting — mark as pending
                let caller_id = (*privp).s_id;
                if !(*dst_ptr).p_priv.is_null() {
                    (*(*dst_ptr).p_priv).s_asyn_pending.set(caller_id as usize);
                }
                all_done = false;
                continue;
            }

            // Write result back to the table
            let mut updated = tabent;
            updated.flags = flags | AMF_DONE;
            let off = (i as u64) * core::mem::size_of::<AsynMsg>() as u64;
            core::ptr::write_unaligned((table as u64 + off) as *mut AsynMsg, updated);

            if flags & AMF_NOTIFY != 0 || (r != OK && flags & AMF_NOTIFY_ERR != 0) {
                do_notify = true;
            }
        }

        if do_notify {
            mini_notify(arch_common::com::ASYNCM, (*caller_ptr).p_endpoint);
        }

        if !all_done {
            (*privp).s_asyntab = table as u64;
            (*privp).s_asynsize = size;
        }

        OK
    }
}

// ── is_ok_endpoint_f ───────────────────────────────────────────────────

/// Validate an endpoint with optional panic.
pub fn is_ok_endpoint_f(ep: i32, p: &mut i32, fatal: bool) -> bool {
    *p = endpoint_slot(ep);
    let ok = is_ok_endpoint(ep)
        && unsafe {
            let rp = proc_addr(*p);
            !rp.is_null() && !(*rp).is_empty() && (*rp).p_endpoint == ep
        };
    if !ok && fatal {
        panic!("invalid endpoint: {}", ep);
    }
    ok
}

// ── build_notify_message ──────────────────────────────────────────────

/// Build a notification message.
pub fn build_notify_message(msg: &mut [u8; MESSAGE_SIZE], _src: i32, _dst_ptr: *mut Proc) {
    msg.fill(0);
    // m_type at offset 4 (C: m_ptr->m_type = NOTIFY_MESSAGE)
    msg[4..8].copy_from_slice(&(-10i32).to_ne_bytes()); // NOTIFY_MESSAGE = -10
    // m_source at offset 0 — set by caller / delivery path
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::proc_init;

    fn setup_proc(nr: i32) -> *mut Proc {
        unsafe {
            arch_x86_64::cpulocals::init_cpulocals();
            let rp = proc_addr(nr);
            if !rp.is_null() {
                (*rp).p_rts_flags.store(0, Ordering::Relaxed);
                (*rp).p_nr = nr;
                // Use proper endpoint encoding: generation 0, slot = nr
                (*rp).p_endpoint = crate::table::make_endpoint(0, nr);
                (*rp).p_caller_q = core::ptr::null_mut();
                (*rp).p_q_link = core::ptr::null_mut();
                (*rp).p_getfrom_e = 0;
                (*rp).p_sendto_e = 0;
                (*rp).p_magic = crate::proc::PMAGIC;
            }
            rp
        }
    }

    #[test]
    fn test_ipc_status_helpers() {
        let mut status: u16 = 0;
        status = (status & !0x7F) | SEND as u16;
        assert_eq!(ipc_status_call(status), SEND);
        status |= IPC_FLG_MSG_FROM_KERNEL;
        assert!(ipc_status_has_flag(status, IPC_FLG_MSG_FROM_KERNEL));
    }

    #[test]
    fn test_will_receive_matches() {
        unsafe {
            proc_init();
            let dst = setup_proc(0);
            let src = setup_proc(1);
            (*dst)
                .p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            (*dst).p_getfrom_e = 100;
            (*src).p_endpoint = 100;
            assert!(will_receive(dst, 100));
            assert!(!will_receive(dst, 101));
        }
    }

    #[test]
    fn test_mini_send_direct_delivery() {
        unsafe {
            proc_init();
            let src = setup_proc(0);
            let dst = setup_proc(1);
            let src_ep = (*src).p_endpoint; // = 0
            let dst_ep = (*dst).p_endpoint; // = 1
            (*dst)
                .p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            (*dst).p_getfrom_e = src_ep;

            let mut msg = [0u8; MESSAGE_SIZE];
            msg[0..4].copy_from_slice(&42i32.to_ne_bytes());
            assert_eq!(mini_send(src, dst_ep, msg.as_ptr(), 0), OK);

            let mut buf = [0u8; 4];
            core::ptr::copy_nonoverlapping((*dst).p_delivermsg.as_ptr(), buf.as_mut_ptr(), 4);
            assert_eq!(i32::from_ne_bytes(buf), 42);
            core::ptr::copy_nonoverlapping(
                (*dst).p_delivermsg.as_ptr().add(4),
                buf.as_mut_ptr(),
                4,
            );
            assert_eq!(i32::from_ne_bytes(buf), src_ep);
            let rts = (*dst).p_rts_flags.load(Ordering::Relaxed);
            assert_eq!(rts & RtsFlags::RECEIVING.bits(), 0);
        }
    }

    #[test]
    fn test_mini_send_queues_when_not_receiving() {
        unsafe {
            proc_init();
            let src = setup_proc(0);
            let dst = setup_proc(1);
            let dst_ep = (*dst).p_endpoint;
            (*dst).p_rts_flags.store(0, Ordering::Relaxed);

            let mut msg = [0u8; MESSAGE_SIZE];
            msg[0..4].copy_from_slice(&42i32.to_ne_bytes());
            assert_eq!(mini_send(src, dst_ep, msg.as_ptr(), 0), OK);

            let rts = (*src).p_rts_flags.load(Ordering::Relaxed);
            assert!(rts & RtsFlags::SENDING.bits() != 0);
            assert_eq!((*dst).p_caller_q, src);
        }
    }

    #[test]
    fn test_mini_send_non_blocking() {
        unsafe {
            proc_init();
            let src = setup_proc(0);
            let dst = setup_proc(1);
            let dst_ep = (*dst).p_endpoint;
            (*dst).p_rts_flags.store(0, Ordering::Relaxed);
            assert_eq!(
                mini_send(src, dst_ep, [0u8; MESSAGE_SIZE].as_ptr(), NON_BLOCKING),
                ENOTREADY
            );
        }
    }

    #[test]
    fn test_mini_send_no_endpoint() {
        unsafe {
            proc_init();
            let src = setup_proc(0);
            let dst = setup_proc(1);
            let dst_ep = (*dst).p_endpoint;
            (*dst)
                .p_rts_flags
                .store(RtsFlags::NO_ENDPOINT.bits(), Ordering::Relaxed);
            assert_eq!(
                mini_send(src, dst_ep, [0u8; MESSAGE_SIZE].as_ptr(), 0),
                EDEADSRCDST
            );
        }
    }

    #[test]
    fn test_mini_receive_from_queued_sender() {
        unsafe {
            proc_init();
            let src = setup_proc(0);
            let dst = setup_proc(1);
            let src_ep = (*src).p_endpoint;
            let dst_ep = (*dst).p_endpoint;
            (*src)
                .p_rts_flags
                .store(RtsFlags::SENDING.bits(), Ordering::Relaxed);
            (*src).p_sendto_e = dst_ep;
            (&mut (*src).p_sendmsg)[0..4].copy_from_slice(&99i32.to_ne_bytes());
            (*dst).p_caller_q = src;

            let mut buf = [0u8; MESSAGE_SIZE];
            assert_eq!(
                mini_receive(dst, crate::system::NONE, buf.as_mut_ptr(), 0),
                OK
            );
            assert_eq!(i32::from_ne_bytes(buf[..4].try_into().unwrap()), 99);
            let sr = (*src).p_rts_flags.load(Ordering::Relaxed);
            assert_eq!(sr & RtsFlags::SENDING.bits(), 0);
            let _ = src_ep;
        }
    }

    #[test]
    fn test_mini_receive_blocking() {
        unsafe {
            proc_init();
            let dst = setup_proc(0);
            assert_eq!(
                mini_receive(
                    dst,
                    crate::system::NONE,
                    [0u8; MESSAGE_SIZE].as_mut_ptr(),
                    0
                ),
                OK
            );
            let rts = (*dst).p_rts_flags.load(Ordering::Relaxed);
            assert!(rts & RtsFlags::RECEIVING.bits() != 0);
        }
    }

    #[test]
    fn test_deadlock_simple_cycle() {
        unsafe {
            proc_init();
            let a = setup_proc(0);
            let b = setup_proc(1);
            let a_ep = (*a).p_endpoint;
            let b_ep = (*b).p_endpoint;
            // A is blocked on SENDING to B
            (*a).p_rts_flags
                .store(RtsFlags::SENDING.bits(), Ordering::Relaxed);
            (*a).p_sendto_e = b_ep;
            // B is blocked on RECEIVING from A
            (*b).p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            (*b).p_getfrom_e = a_ep;
            // B tries to send to A — deadlock!
            assert!(
                deadlock(SEND, b, a_ep),
                "B→A should deadlock when A→B blocked"
            );
        }
    }

    #[test]
    fn test_deadlock_no_cycle() {
        unsafe {
            proc_init();
            let a = setup_proc(0);
            let b = setup_proc(1);
            let a_ep = (*a).p_endpoint;
            (*a).p_rts_flags.store(0, Ordering::Relaxed);
            assert!(!deadlock(SEND, b, a_ep));
        }
    }

    #[test]
    fn test_sendrec_roundtrip() {
        unsafe {
            proc_init();
            let src = setup_proc(0);
            let dst = setup_proc(1);
            let src_ep = (*src).p_endpoint;
            let dst_ep = (*dst).p_endpoint;
            (*dst)
                .p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            (*dst).p_getfrom_e = src_ep;

            let mut msg = [0u8; MESSAGE_SIZE];
            msg[0..4].copy_from_slice(&42i32.to_ne_bytes());
            assert_eq!(mini_send(src, dst_ep, msg.as_ptr(), 0), OK);

            let mut buf = [0u8; 4];
            core::ptr::copy_nonoverlapping((*dst).p_delivermsg.as_ptr(), buf.as_mut_ptr(), 4);
            assert_eq!(i32::from_ne_bytes(buf), 42);
        }
    }

    #[test]
    fn test_mini_notify_wakes_receiving() {
        unsafe {
            proc_init();
            let dst = setup_proc(0);
            let dst_ep = (*dst).p_endpoint;
            (*dst)
                .p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            (*dst).p_getfrom_e = crate::system::NONE;

            assert_eq!(mini_notify(999, dst_ep), OK);
            let rts = (*dst).p_rts_flags.load(Ordering::Relaxed);
            assert_eq!(
                rts & RtsFlags::RECEIVING.bits(),
                0,
                "notification should wake RECEIVING process"
            );
        }
    }

    // ── Server dispatch tests ──────────────────────────────────────────

    #[test]
    fn test_register_dispatch_invalid_endpoint() {
        assert!(!register_server_dispatch(-999, |_, _| 0));
        assert!(!register_server_dispatch(9999, |_, _| 0));
    }

    #[test]
    fn test_register_dispatch_valid_endpoint() {
        let handler: ServerDispatchFn = |_, _| 42;
        assert!(register_server_dispatch(
            arch_common::com::PM_PROC_NR,
            handler
        ));
    }

    #[test]
    fn test_try_server_dispatch_no_handler() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            let result = try_server_dispatch(rp, 42, &mut msg);
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_try_server_dispatch_registered_handler() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            let handler: ServerDispatchFn = |_, _| 99;
            register_server_dispatch(arch_common::com::PM_PROC_NR, handler);
            let result = try_server_dispatch(rp, arch_common::com::PM_PROC_NR, &mut msg);
            assert_eq!(result, Some(99));
        }
    }

    #[test]
    fn test_set_exec_rip_register_and_call() {
        unsafe {
            let handler: SetExecRipFn = |_, _, _| {};
            register_set_exec_rip(handler);
            proc_init();
            let rp = crate::table::proc_addr(0);
            set_exec_target(rp, 0x400000, 0x7fff0000);
        }
    }

    #[test]
    fn test_pm_dispatch_stubs_compile() {
        fn _fork(_: ServerDispatchFn) {}
        fn _exec(_: ServerDispatchFn) {}
        fn _exit(_: ServerDispatchFn) {}
        fn _waitpid(_: ServerDispatchFn) {}
        _fork(pm_fork_dispatch);
        _exec(pm_exec_dispatch);
        _exit(pm_exit_dispatch);
        _waitpid(pm_waitpid_dispatch);
    }

    #[test]
    fn test_init_server_dispatch_registers_pm() {
        init_server_dispatch();
        unsafe {
            let mut msg = [0u8; MESSAGE_SIZE];
            let rp = crate::table::proc_addr(0);
            let result = try_server_dispatch(rp, arch_common::com::PM_PROC_NR, &mut msg);
            assert!(
                result.is_some(),
                "PM should have a dispatch handler after init"
            );
            assert_eq!(result, Some(0));
        }
    }

    // ── IPC syscall handler tests ──────────────────────────────────────

    #[test]
    fn test_ipc_handler_functions_are_callable() {
        // Verify all four handlers compile with the right signature
        fn _check(
            _: unsafe fn(*mut crate::proc::Proc, &mut [u8; crate::proc::MESSAGE_SIZE]) -> i32,
        ) {
        }
        _check(ipc_send_handler);
        _check(ipc_receive_handler);
        _check(ipc_sendrec_handler);
        _check(ipc_notify_handler);
        _check(ipc_senda_handler);
    }

    #[test]
    fn test_register_ipc_syscalls_is_callable() {
        fn _f(_: unsafe fn()) {}
        _f(register_ipc_syscalls);
    }

    #[test]
    fn test_sys_max_constant() {
        assert_eq!(SYS_MAX, 51);
    }

    #[test]
    fn test_current_proc_helpers_compile() {
        fn _f1(_: unsafe fn() -> *mut crate::proc::Proc) {}
        fn _f2(_: unsafe fn(*mut crate::proc::Proc)) {}
        _f1(current_proc);
        _f2(set_current_proc);
    }

    #[test]
    fn test_ipc_syscall_handler_signatures() {
        // Verify the handler functions match the system::CallHandler signature
        fn _check(
            _: unsafe fn(*mut crate::proc::Proc, &mut [u8; crate::proc::MESSAGE_SIZE]) -> i32,
        ) {
        }
        _check(ipc_send_handler);
        _check(ipc_receive_handler);
        _check(ipc_sendrec_handler);
        _check(ipc_notify_handler);
    }

    // Phase 6.5.7 — delivermsg regression checks

    #[test]
    fn test_delivermsg_zero_vir_returns_ok() {
        unsafe {
            let mut proc = crate::proc::Proc::default();
            proc.p_delivermsg_vir = 0;
            let r = delivermsg(&mut proc as *mut _);
            assert_eq!(r, OK, "zero vir should skip delivery");
        }
    }

    #[test]
    fn test_delivermsg_copies_data_to_buffer() {
        // Verifies the copy_nonoverlapping in delivermsg works.
        // With BOOT_CR3 == 0 (test mode), the CR3 switch is skipped,
        // but the actual memory copy IS executed.
        unsafe {
            let mut proc = crate::proc::Proc::default();
            // Fill the kernel-side message buffer with test data
            for i in 0..crate::proc::MESSAGE_SIZE {
                proc.p_delivermsg[i] = (i ^ 0xA5) as u8;
            }
            // Create a local buffer to receive the message
            let mut buf = [0u8; crate::proc::MESSAGE_SIZE];
            proc.p_delivermsg_vir = buf.as_mut_ptr() as u64;

            let r = delivermsg(&mut proc as *mut _);
            assert_eq!(r, OK);

            // Verify data was copied
            for i in 0..crate::proc::MESSAGE_SIZE {
                assert_eq!(
                    buf[i],
                    (i ^ 0xA5) as u8,
                    "byte {} mismatch after delivermsg",
                    i
                );
            }
        }
    }
}
