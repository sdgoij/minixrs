//! IPC subsystem — adapted from `minix/kernel/proc.c`
//!
//! Implements the core Minix IPC primitives.

use core::sync::atomic::Ordering;

use crate::proc::*;
use crate::sched::{dequeue, enqueue};
use crate::table::{endpoint_slot, is_ok_endpoint, proc_addr};

// ── Constants ───────────────────────────────────────────────────────────

pub const NON_BLOCKING: i32 = 0x01;
pub const FROM_KERNEL: i32 = 0x02;

pub const SEND: i32 = 0x01;
pub const RECEIVE: i32 = 0x02;
pub const SENDREC: i32 = 0x03;
pub const SENDNB: i32 = 0x04;
pub const NOTIFY: i32 = 0x05;

pub const IPC_FLG_MSG_FROM_KERNEL: u16 = 0x0001;
pub const IPC_FLG_CALL_MASK: u16 = 0x007F;
pub const IPC_FLG_STATUS_MASK: u16 = 0x00FF;

pub const AMF_VALID: u16 = 0x01;
pub const AMF_DONE: u16 = 0x02;
pub const AMF_NOTIFY: u16 = 0x04;
pub const AMF_NOREPLY: u16 = 0x08;
pub const AMF_NOTIFY_ERR: u16 = 0x10;

pub const OK: i32 = 0;
pub const EFAULT: i32 = -14;
pub const ENOTREADY: i32 = -73;
pub const ELOCKED: i32 = -132;
pub const EDEADSRCDST: i32 = -199;
pub const EPERM: i32 = -1;

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
                (*caller_ptr)
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
pub unsafe fn mini_receive(caller_ptr: *mut Proc, src_e: i32, m_ptr: *mut u8) -> i32 {
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
        let rts = (*dst_ptr).p_rts_flags.load(Ordering::Relaxed);
        if rts & RtsFlags::RECEIVING.bits() != 0
            && ((*dst_ptr).p_getfrom_e == crate::system::NONE || (*dst_ptr).p_getfrom_e == src_e)
        {
            let new = rts & !RtsFlags::RECEIVING.bits();
            (*dst_ptr).p_rts_flags.store(new, Ordering::Relaxed);
            if new == 0 {
                enqueue(dst_ptr);
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

// ── do_sync_ipc ────────────────────────────────────────────────────────

/// Perform a synchronous IPC operation.
///
/// # Safety
///
/// All process pointers must be valid.
pub unsafe fn do_sync_ipc(caller_ptr: *mut Proc, m_ptr: *mut u8, call: i32) -> i32 {
    unsafe {
        let src_dst_e = core::ptr::read_unaligned(m_ptr.cast::<[u8; 8]>());
        let ep = i32::from_ne_bytes([src_dst_e[0], src_dst_e[1], src_dst_e[2], src_dst_e[3]]);
        match call {
            SENDREC => {
                let r = mini_send(caller_ptr, ep, m_ptr, 0);
                if r != OK {
                    return r;
                }
                mini_receive(caller_ptr, ep, m_ptr)
            }
            SEND => mini_send(caller_ptr, ep, m_ptr, 0),
            RECEIVE => mini_receive(caller_ptr, ep, m_ptr),
            SENDNB => mini_send(caller_ptr, ep, m_ptr, NON_BLOCKING),
            NOTIFY => mini_notify((*caller_ptr).p_endpoint, ep),
            _ => crate::system::EBADREQUEST,
        }
    }
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

/// # Safety
///
/// Process pointers must be valid.
pub unsafe fn try_one(_src_ptr: *mut Proc, _dst_ptr: *mut Proc) -> i32 {
    OK
}

/// # Safety
///
/// Process pointer must be valid.
pub unsafe fn try_async(_caller_ptr: *mut Proc) -> i32 {
    OK
}

/// # Safety
///
/// Process pointers must be valid.
pub unsafe fn cancel_async(_src_ptr: *mut Proc, _dst_ptr: *mut Proc) {}

/// # Safety
///
/// Process pointer and table must be valid.
pub unsafe fn try_deliver_senda(_caller_ptr: *mut Proc, _table: *mut u8, _size: usize) -> i32 {
    OK
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
    msg[0..4].copy_from_slice(&(-10i32).to_ne_bytes()); // NOTIFY_MESSAGE
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
            assert_eq!(mini_receive(dst, crate::system::NONE, buf.as_mut_ptr()), OK);
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
                mini_receive(dst, crate::system::NONE, [0u8; MESSAGE_SIZE].as_mut_ptr()),
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
}
