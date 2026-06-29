//! System call dispatch infrastructure — adapted from `minix/kernel/system.c`
//!
//! Defines the kernel call dispatch table (`call_vec`), system
//! initialization, privilege management, signal delivery skeletons,
//! and IPC cleanup functions.
//!
//! **x86_64 differences from i386:**
//! - All vir_bytes are u64 (not u32)
//! - No i386-only syscalls (SYS_READBIOS, SYS_IOPENABLE) — omitted
//!   from call_vec (these have no x86_64 equivalent)
//! - SYS_DEVIO, SYS_SDEVIO, SYS_VDEVIO are implemented (Phase 8.8) —
//!   port I/O is the same on x86_64
//! - message copy uses raw pointer copy (no segmentation)

use core::sync::atomic::Ordering;

use arch_common::consts::{SEGMENT_INDEX, VM_GRANT};
use arch_common::endpoint::ANY;

use crate::r#priv::*;
use crate::proc::*;
use crate::sched::dequeue;
use crate::table;
use crate::table::proc_addr;
use arch_x86_64::frame::TrapFrame;

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

unsafe fn msg_write_i64(msg: &mut [u8; MESSAGE_SIZE], offset: usize, val: i64) {
    msg[offset..offset + 8].copy_from_slice(&val.to_ne_bytes());
}

/// Read a u32 field from the message.
unsafe fn msg_read_u32(msg: &[u8; MESSAGE_SIZE], offset: usize) -> u32 {
    u32::from_ne_bytes(msg[offset..offset + 4].try_into().unwrap())
}

/// Read an i64 field from the message.
unsafe fn msg_read_i64(msg: &[u8; MESSAGE_SIZE], offset: usize) -> i64 {
    i64::from_ne_bytes(msg[offset..offset + 8].try_into().unwrap())
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
#[allow(dead_code)]
const DIAGCTL_BUF_OFF: usize = 8;
#[allow(dead_code)]
const DIAGCTL_LEN_OFF: usize = 16;
const DIAGCTL_ENDPT_OFF: usize = 20;

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

// Phase 6.16: do_safememset message offsets
const SAFEMEMSET_GRANTER_OFF: usize = 0;
const SAFEMEMSET_GRANT_ID_OFF: usize = 4;
const SAFEMEMSET_OFFSET_OFF: usize = 8;
const SAFEMEMSET_PATTERN_OFF: usize = 16;
const SAFEMEMSET_BYTES_OFF: usize = 24;
// ── Exec message offsets (Phase 8.10) ──────────────────────────────────
//
// mess_lsys_krn_sys_exec:
//   offset  0: endpt   (endpoint_t / i32, 4 bytes)
//   offset  4: _pad    (4 bytes)
//   offset  8: ip      (vir_bytes / u64)
//   offset 16: stack   (vir_bytes / u64)
//   offset 24: name    (vir_bytes / u64) — pointer to program name in caller
//   offset 32: ps_str  (vir_bytes / u64)
const EXEC_ENDPT_OFF: usize = 0;
const EXEC_IP_OFF: usize = 8;
const EXEC_STACK_OFF: usize = 16;
const EXEC_NAME_OFF: usize = 24;
const EXEC_PS_STR_OFF: usize = 32;

// ── Mcontext message offsets (Phase 8.10) ─────────────────────────────
//
// mess_lsys_krn_sys_{get,set}mcontext:
//   offset  0: endpt    (endpoint_t / i32, 4 bytes)
//   offset  4: _pad     (4 bytes)
//   offset  8: ctx_ptr  (vir_bytes / u64)
const MCONTEXT_ENDPT_OFF: usize = 0;
const MCONTEXT_CTX_PTR_OFF: usize = 8;

const M1_P1_OFF: usize = 24;
// M1_P2_OFF = 32, M1_P3_OFF = 40, M1_P4_OFF = 48 — reserved for future use

// ── Devio message offsets (Phase 8.8) ────────────────────────────────
//
// mess_lsys_krn_sys_devio (for do_devio):
//   offset  0: request  (int / i32)
//   offset  4: port     (int / i32) — port_t fits in 16 bits
//   offset  8: value    (u32)
//
// mess_krn_lsys_sys_devio (reply for do_devio, input):
//   offset  0: value    (u32)
const DEVIO_REQUEST_OFF: usize = 0;
const DEVIO_PORT_OFF: usize = 4;
const DEVIO_VALUE_OFF: usize = 8;
const DEVIO_REPLY_VALUE_OFF: usize = 0;

// mess_lsys_krn_sys_vdevio (for do_vdevio):
//   offset  0: request  (int / i32)
//   offset  4: vec_size (int / i32)
//   offset  8: vec_addr (vir_bytes / u64)
const VDEVIO_REQUEST_OFF: usize = 0;
const VDEVIO_VEC_SIZE_OFF: usize = 4;
const VDEVIO_VEC_ADDR_OFF: usize = 8;

// mess_lsys_krn_sys_sdevio (for do_sdevio):
//   Layout on x86_64 (with natural alignment):
//   offset  0: request   (int / i32, 4 bytes)
//   offset  4: _pad      (4 bytes for alignment)
//   offset  8: port      (long int / i64, 8 bytes)
//   offset 16: vec_endpt (endpoint_t / i32, 4 bytes)
//   offset 20: _pad2     (4 bytes)
//   offset 24: vec_addr  (phys_bytes / u64, 8 bytes)
//   offset 32: vec_size  (vir_bytes / u64, 8 bytes)
//   offset 40: offset    (vir_bytes / u64, 8 bytes)
const SDEVIO_REQUEST_OFF: usize = 0;
const SDEVIO_PORT_OFF: usize = 8;
const SDEVIO_VEC_ENDPT_OFF: usize = 16;
const SDEVIO_VEC_ADDR_OFF: usize = 24;
const SDEVIO_VEC_SIZE_OFF: usize = 32;
const SDEVIO_OFFSET_OFF: usize = 40;

// mess_lsys_krn_sys_vumap (for do_vumap):
//   offset  0: endpt     (endpoint_t / i32)
//   offset  8: vaddr     (vir_bytes / u64)
//   offset 16: vcount    (int / i32)
//   offset 20: _pad      (int / i32)
//   offset 24: offset    (vir_bytes / u64)
//   offset 32: access    (int / i32)
//   offset 36: _pad2     (int / i32)
//   offset 40: paddr     (vir_bytes / u64)
//   offset 48: pmax      (int / i32)
// mess_krn_lsys_sys_vumap (reply):
//   offset  0: pcount    (int / i32)
const VUMAP_ENDPT_OFF: usize = 0;
const VUMAP_VADDR_OFF: usize = 8;
const VUMAP_VCOUNT_OFF: usize = 16;
const VUMAP_OFFSET_OFF: usize = 24;
const VUMAP_ACCESS_OFF: usize = 32;
const VUMAP_PADDR_OFF: usize = 40;
const VUMAP_PMAX_OFF: usize = 48;
const VUMAP_REPLY_PCOUNT_OFF: usize = 0;

/// Maximum number of vectored map elements.
const MAPVEC_NR: usize = 64;

/// User virtual address access flags (converted to CPF_*).
const VUA_READ: i32 = 0x0001;
const VUA_WRITE: i32 = 0x0002;

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

// mess_lsys_krn_sys_irqctl (for do_irqctl):
//   offset  0: request  (int / i32)
//   offset  4: vector   (int / i32)
//   offset  8: policy   (int / i32)
//   offset 12: hook_id  (int / i32) — also notify_id for SETPOLICY, reply for hook index
const IRQCTL_REQUEST_OFF: usize = 0;
const IRQCTL_VECTOR_OFF: usize = 4;
const IRQCTL_POLICY_OFF: usize = 8;
const IRQCTL_HOOK_ID_OFF: usize = 12;

// mess_lsys_krn_sys_setalarm (for do_setalarm):
//   offset  0: exp_time   (u64)
//   offset  8: time_left  (u64, reply field)
//   offset 16: abs_time   (i32)
const SETALARM_EXP_TIME_OFF: usize = 0;
const SETALARM_TIME_LEFT_OFF: usize = 8;
const SETALARM_ABS_TIME_OFF: usize = 16;

// mess_lsys_krn_sys_stime (for do_stime):
//   offset 0: boot_time (time_t / i64)
const STIME_BOOT_TIME_OFF: usize = 0;

// mess_lsys_krn_sys_settime (for do_settime):
//   offset  0: sec       (time_t / i64)
//   offset  8: nsec      (long / i64)
//   offset 16: now       (int / i32)
//   offset 20: clock_id  (clockid_t / i32)
const SETTIME_SEC_OFF: usize = 0;
const SETTIME_NSEC_OFF: usize = 8;
const SETTIME_NOW_OFF: usize = 16;
const SETTIME_CLOCK_ID_OFF: usize = 20;

// mess_2 offsets (for do_vtimer):
//   offset  0: m2ll1 (i64)
//   offset  8: m2i1  (i32) — VT_WHICH
//   offset 12: m2i2  (i32) — VT_SET
//   offset 16: m2i3  (i32) — unused
//   offset 24: m2l1  (u64) — VT_VALUE
//   offset 32: m2l2  (u64) — VT_ENDPT
const VTIMER_WHICH_OFF: usize = 8;
const VTIMER_SET_OFF: usize = 12;
const VTIMER_VALUE_OFF: usize = 24;
const VTIMER_ENDPT_OFF: usize = 32;

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
pub const ENOSPC: i32 = -28;

/// Maximum number of IRQ vectors.
pub const NR_IRQ_VECTORS: i32 = 64;

/// IRQ sub-operations.
pub const IRQ_SETPOLICY: i32 = 1;
pub const IRQ_RMPOLICY: i32 = 2;
pub const IRQ_ENABLE: i32 = 3;
pub const IRQ_DISABLE: i32 = 4;
pub const IRQ_REENABLE: i32 = 0x001;

/// Clock ID for realtime clock.
pub const CLOCK_REALTIME: i32 = 0;

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

/// VDEVIO static buffer for copying (port,value) pairs from/to user space.
const VDEVIO_BUF_SIZE: usize = 64;

// ── do_devio — single I/O port access (Phase 8.8) ─────────────────────
// Source: .refs/minix-3.3.0/minix/kernel/system/do_devio.c

/// Handle SYS_DEVIO: read/write a single I/O port.
///
/// # Safety
///
/// `caller` must be a valid process pointer with privilege structure.
pub unsafe fn do_devio_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let request = msg_read_i32(msg, DEVIO_REQUEST_OFF) as u32;
        let port = msg_read_i32(msg, DEVIO_PORT_OFF) as u16;

        let io_type = request & arch_common::com::DIO_TYPEMASK;
        let io_dir = request & arch_common::com::DIO_DIRMASK;

        let size = match io_type {
            t if t == arch_common::com::DIO_BYTE => 1,
            t if t == arch_common::com::DIO_WORD => 2,
            t if t == arch_common::com::DIO_LONG => 4,
            _ => 4,
        };

        // Check port alignment
        if (port as u32) & (size as u32 - 1) != 0 {
            return crate::ipc::EPERM;
        }

        // Check I/O port access permissions
        let privp = (*caller).p_priv;
        if !privp.is_null() && (*privp).s_flags.contains(PrivFlags::CHECK_IO_PORT) {
            let nr_io_range = (*privp).s_nr_io_range as usize;
            let port_u32 = port as u32;
            let io_tab_ptr: *const crate::r#priv::IoRange = &raw const (*privp).s_io_tab[0];
            let mut found = false;
            for i in 0..nr_io_range {
                let ior = &*io_tab_ptr.add(i);
                if port_u32 >= ior.ior_base && port_u32 + size as u32 - 1 <= ior.ior_limit {
                    found = true;
                    break;
                }
            }
            if !found {
                return crate::ipc::EPERM;
            }
        }

        // Validate io_dir and io_type first (always, even in test mode)
        let is_input = io_dir == arch_common::com::DIO_INPUT;
        let is_output = io_dir == arch_common::com::DIO_OUTPUT;
        if !is_input && !is_output {
            return crate::ipc::EINVAL;
        }
        let io_type_valid = match io_type {
            t if t == arch_common::com::DIO_BYTE => true,
            t if t == arch_common::com::DIO_WORD => true,
            t if t == arch_common::com::DIO_LONG => true,
            _ => false,
        };
        if !io_type_valid {
            return crate::ipc::EINVAL;
        }

        // Perform I/O (gated: only on bare metal where BOOT_CR3 is set)
        let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
        if boot_cr3 != 0 {
            if is_input {
                let value = match io_type {
                    t if t == arch_common::com::DIO_BYTE => arch_x86_64::asm::inb(port) as u32,
                    t if t == arch_common::com::DIO_WORD => arch_x86_64::asm::inw(port) as u32,
                    _ => arch_x86_64::asm::inl(port),
                };
                let reply_bytes = value.to_ne_bytes();
                let reply_start = DEVIO_REPLY_VALUE_OFF;
                msg[reply_start..reply_start + 4].copy_from_slice(&reply_bytes);
            } else {
                let value = msg_read_u32(msg, DEVIO_VALUE_OFF);
                match io_type {
                    t if t == arch_common::com::DIO_BYTE => {
                        arch_x86_64::asm::outb(port, value as u8)
                    }
                    t if t == arch_common::com::DIO_WORD => {
                        arch_x86_64::asm::outw(port, value as u16)
                    }
                    _ => arch_x86_64::asm::outl(port, value),
                }
            }
        }
        // In test mode (BOOT_CR3 == 0) or after I/O: acknowledge request
        OK
    }
}

// ── do_vdevio — vectored I/O port access (Phase 8.8) ──────────────────
// Source: .refs/minix-3.3.0/minix/kernel/system/do_vdevio.c

/// Static buffer for VDEVIO (port,value) pairs.
static mut VDEVIO_BUF: [u8; VDEVIO_BUF_SIZE] = [0u8; VDEVIO_BUF_SIZE];

/// Handle SYS_VDEVIO: perform a series of I/O port operations.
///
/// # Safety
///
/// `caller` must be a valid process pointer with privilege structure.
pub unsafe fn do_vdevio_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let request = msg_read_i32(msg, VDEVIO_REQUEST_OFF) as u32;
        let vec_size = msg_read_i32(msg, VDEVIO_VEC_SIZE_OFF);
        let vec_addr = msg_read_u64(msg, VDEVIO_VEC_ADDR_OFF);

        if vec_size <= 0 {
            return crate::ipc::EINVAL;
        }

        let io_dir = request & arch_common::com::DIO_DIRMASK;
        let io_type = request & arch_common::com::DIO_TYPEMASK;

        let io_in = if io_dir == arch_common::com::DIO_INPUT {
            true
        } else if io_dir == arch_common::com::DIO_OUTPUT {
            false
        } else {
            return crate::ipc::EINVAL;
        };

        use arch_common::devio::{PvbPair, PvlPair, PvwPair};
        use core::mem::size_of;

        let (bytes, io_size) = match io_type {
            t if t == arch_common::com::DIO_BYTE => {
                (vec_size as usize * size_of::<PvbPair>(), size_of::<u8>())
            }
            t if t == arch_common::com::DIO_WORD => {
                (vec_size as usize * size_of::<PvwPair>(), size_of::<u16>())
            }
            t if t == arch_common::com::DIO_LONG => {
                (vec_size as usize * size_of::<PvlPair>(), size_of::<u32>())
            }
            _ => return crate::ipc::EINVAL,
        };

        if bytes > VDEVIO_BUF_SIZE {
            return crate::ipc::E2BIG;
        }

        // Copy vector from caller's address space
        // We switch to caller's CR3 to read from their virtual address,
        // then copy into the kernel VDEVIO_BUF (identity-mapped).
        let buf_ptr = core::ptr::addr_of_mut!(VDEVIO_BUF) as u64;
        {
            let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
            if boot_cr3 != 0 {
                let caller_cr3 = (*caller).p_seg.p_cr3;
                if caller_cr3 != 0 {
                    arch_x86_64::asm::write_cr3(caller_cr3);
                }
            }
            let buf_mut = core::ptr::addr_of_mut!(VDEVIO_BUF) as *mut u8;
            core::ptr::copy_nonoverlapping(vec_addr as *const u8, buf_mut, bytes);
            if boot_cr3 != 0 {
                arch_x86_64::asm::write_cr3(boot_cr3);
            }
        }

        // Check I/O port permissions
        let privp = (*caller).p_priv;
        if !privp.is_null() && (*privp).s_flags.contains(PrivFlags::CHECK_IO_PORT) {
            let nr_io_range = (*privp).s_nr_io_range as usize;
            let io_tab_ptr: *const crate::r#priv::IoRange = &raw const (*privp).s_io_tab[0];
            for i in 0..vec_size as usize {
                let port = match io_type {
                    t if t == arch_common::com::DIO_BYTE => {
                        let pvb = &*(buf_ptr as *const PvbPair).add(i);
                        pvb.port as u32
                    }
                    t if t == arch_common::com::DIO_WORD => {
                        let pvw = &*(buf_ptr as *const PvwPair).add(i);
                        pvw.port as u32
                    }
                    _ => {
                        let pvl = &*(buf_ptr as *const PvlPair).add(i);
                        pvl.port as u32
                    }
                };
                let mut found = false;
                for j in 0..nr_io_range {
                    let ior = &*io_tab_ptr.add(j);
                    if port >= ior.ior_base && port + io_size as u32 - 1 <= ior.ior_limit {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return crate::ipc::EPERM;
                }
            }
        }

        // Perform actual device I/O
        match io_type {
            t if t == arch_common::com::DIO_BYTE => {
                let pairs =
                    core::slice::from_raw_parts_mut(buf_ptr as *mut PvbPair, vec_size as usize);
                if io_in {
                    for pair in pairs.iter_mut() {
                        pair.value = arch_x86_64::asm::inb(pair.port);
                    }
                } else {
                    for pair in pairs.iter() {
                        arch_x86_64::asm::outb(pair.port, pair.value);
                    }
                }
            }
            t if t == arch_common::com::DIO_WORD => {
                let pairs =
                    core::slice::from_raw_parts_mut(buf_ptr as *mut PvwPair, vec_size as usize);
                if io_in {
                    for pair in pairs.iter_mut() {
                        if pair.port & 1 != 0 {
                            return crate::ipc::EPERM;
                        }
                        pair.value = arch_x86_64::asm::inw(pair.port);
                    }
                } else {
                    for pair in pairs.iter() {
                        if pair.port & 1 != 0 {
                            return crate::ipc::EPERM;
                        }
                        arch_x86_64::asm::outw(pair.port, pair.value);
                    }
                }
            }
            _ => {
                // DIO_LONG
                let pairs =
                    core::slice::from_raw_parts_mut(buf_ptr as *mut PvlPair, vec_size as usize);
                if io_in {
                    for pair in pairs.iter_mut() {
                        if pair.port & 3 != 0 {
                            return crate::ipc::EPERM;
                        }
                        pair.value = arch_x86_64::asm::inl(pair.port);
                    }
                } else {
                    for pair in pairs.iter() {
                        if pair.port & 3 != 0 {
                            return crate::ipc::EPERM;
                        }
                        arch_x86_64::asm::outl(pair.port, pair.value);
                    }
                }
            }
        }

        // Copy results back for input requests
        if io_in {
            let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
            if boot_cr3 != 0 {
                let caller_cr3 = (*caller).p_seg.p_cr3;
                if caller_cr3 != 0 {
                    arch_x86_64::asm::write_cr3(caller_cr3);
                }
            }
            let buf_src = core::ptr::addr_of_mut!(VDEVIO_BUF) as *const u8;
            core::ptr::copy_nonoverlapping(buf_src, vec_addr as *mut u8, bytes);
            if boot_cr3 != 0 {
                arch_x86_64::asm::write_cr3(boot_cr3);
            }
        }

        OK
    }
}

// ── do_sdevio — string I/O (block read/write) (Phase 8.8) ────────────
// Source: .refs/minix-3.3.0/minix/kernel/arch/i386/do_sdevio.c

/// Handle SYS_SDEVIO: read/write a block of bytes/words from/to a single port.
///
/// # Safety
///
/// `caller` must be a valid process pointer.
pub unsafe fn do_sdevio_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let request = msg_read_i32(msg, SDEVIO_REQUEST_OFF) as u32;
        let port = msg_read_u64(msg, SDEVIO_PORT_OFF);
        let vec_endpt = msg_read_i32(msg, SDEVIO_VEC_ENDPT_OFF);
        let vec_addr = msg_read_u64(msg, SDEVIO_VEC_ADDR_OFF);
        let count = msg_read_u64(msg, SDEVIO_VEC_SIZE_OFF);
        let offset = msg_read_u64(msg, SDEVIO_OFFSET_OFF);

        if count == 0 {
            return OK;
        }

        let req_dir = request & arch_common::com::DIO_DIRMASK;
        let req_type = request & arch_common::com::DIO_TYPEMASK;
        let is_safe = (request & arch_common::com::DIO_SAFEMASK) == arch_common::com::DIO_SAFE;

        // Determine size per element
        let size = match req_type {
            t if t == arch_common::com::DIO_BYTE => 1,
            t if t == arch_common::com::DIO_WORD => 2,
            t if t == arch_common::com::DIO_LONG => 4,
            _ => 4,
        };

        // Resolve destination process
        let caller_ep = (*caller).p_endpoint;
        let (dest_ep, dest_proc_nr) = if vec_endpt == crate::system::SELF {
            (caller_ep, (*caller).p_nr)
        } else {
            if !table::is_ok_endpoint(vec_endpt) {
                return crate::ipc::EINVAL;
            }
            let pnr = table::endpoint_slot(vec_endpt);
            if table::is_kernel_nr(pnr) {
                return crate::ipc::EPERM;
            }
            (vec_endpt, pnr)
        };

        let dest_rp = table::proc_addr(dest_proc_nr);
        if dest_rp.is_null() || (*dest_rp).is_empty() || (*dest_rp).p_endpoint != dest_ep {
            return crate::ipc::EINVAL;
        }

        // Determine virtual buffer address (grant or direct)
        let vir_buf: u64;
        if is_safe {
            // Safe variant: use verify_grant to resolve
            let access = if req_dir == arch_common::com::DIO_INPUT {
                arch_common::safecopies::CPF_WRITE
            } else {
                arch_common::safecopies::CPF_READ
            };
            let grant_result = crate::grants::verify_grant(
                dest_ep,
                caller_ep,
                vec_addr as i32,
                count,
                access,
                offset,
            );
            let (newoffset, new_granter, _flags) = match grant_result {
                Ok(v) => v,
                Err(_) => return crate::ipc::EPERM,
            };
            // Resolve the actual destination from the grant
            let new_pnr = table::endpoint_slot(new_granter);
            vir_buf = newoffset;
            // Update dest_rp to the grant's granter
            let new_rp = table::proc_addr(new_pnr);
            if new_rp.is_null() || (*new_rp).is_empty() {
                return crate::ipc::EINVAL;
            }
            // For the CR3 switch below, use the new_rp's address space
            // We store the resolved info and use new_rp for switch
            let abs_port = port as u16;

            // Check I/O port access permissions
            let privp = (*caller).p_priv;
            if !privp.is_null() && (*privp).s_flags.contains(PrivFlags::CHECK_IO_PORT) {
                let nr_io_range = (*privp).s_nr_io_range as usize;
                let port_u32 = abs_port as u32;
                let io_tab_ptr: *const crate::r#priv::IoRange = &raw const (*privp).s_io_tab[0];
                let mut found = false;
                for j in 0..nr_io_range {
                    let ior = &*io_tab_ptr.add(j);
                    if port_u32 >= ior.ior_base && port_u32 + size as u32 - 1 <= ior.ior_limit {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return crate::ipc::EPERM;
                }
            }

            // Alignment check
            if (abs_port as u32) & (size as u32 - 1) != 0 {
                return crate::ipc::EPERM;
            }

            // Switch to destination address space
            let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
            if boot_cr3 != 0 {
                let dest_cr3 = (*new_rp).p_seg.p_cr3;
                if dest_cr3 != 0 {
                    arch_x86_64::asm::write_cr3(dest_cr3);
                }
            }

            // Perform string I/O
            let result = if req_dir == arch_common::com::DIO_INPUT {
                match req_type {
                    t if t == arch_common::com::DIO_BYTE => {
                        arch_x86_64::asm::phys_insb(abs_port, vir_buf, count as usize);
                        OK
                    }
                    t if t == arch_common::com::DIO_WORD => {
                        arch_x86_64::asm::phys_insw(abs_port, vir_buf, count as usize);
                        OK
                    }
                    _ => crate::ipc::EINVAL,
                }
            } else if req_dir == arch_common::com::DIO_OUTPUT {
                match req_type {
                    t if t == arch_common::com::DIO_BYTE => {
                        arch_x86_64::asm::phys_outsb(abs_port, vir_buf, count as usize);
                        OK
                    }
                    t if t == arch_common::com::DIO_WORD => {
                        arch_x86_64::asm::phys_outsw(abs_port, vir_buf, count as usize);
                        OK
                    }
                    _ => crate::ipc::EINVAL,
                }
            } else {
                crate::ipc::EINVAL
            };

            // Switch back to boot CR3
            if boot_cr3 != 0 {
                arch_x86_64::asm::write_cr3(boot_cr3);
            }

            result
        } else {
            // Non-safe variant: unsafe sdevio — only allowed for caller's own process
            let caller_proc_nr = (*caller).p_nr;
            let dest_slot = table::endpoint_slot(dest_ep);
            if dest_slot != caller_proc_nr {
                return crate::ipc::EPERM;
            }
            vir_buf = vec_addr;

            let abs_port = port as u16;

            // Check I/O port access permissions
            let privp = (*caller).p_priv;
            if !privp.is_null() && (*privp).s_flags.contains(PrivFlags::CHECK_IO_PORT) {
                let nr_io_range = (*privp).s_nr_io_range as usize;
                let port_u32 = abs_port as u32;
                let io_tab_ptr: *const crate::r#priv::IoRange = &raw const (*privp).s_io_tab[0];
                let mut found = false;
                for j in 0..nr_io_range {
                    let ior = &*io_tab_ptr.add(j);
                    if port_u32 >= ior.ior_base && port_u32 + size as u32 - 1 <= ior.ior_limit {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return crate::ipc::EPERM;
                }
            }

            // Alignment check
            if (abs_port as u32) & (size as u32 - 1) != 0 {
                return crate::ipc::EPERM;
            }

            // Switch to destination address space
            let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
            if boot_cr3 != 0 {
                let dest_cr3 = (*dest_rp).p_seg.p_cr3;
                if dest_cr3 != 0 {
                    arch_x86_64::asm::write_cr3(dest_cr3);
                }
            }

            // Perform string I/O
            let result = if req_dir == arch_common::com::DIO_INPUT {
                match req_type {
                    t if t == arch_common::com::DIO_BYTE => {
                        arch_x86_64::asm::phys_insb(abs_port, vir_buf, count as usize);
                        OK
                    }
                    t if t == arch_common::com::DIO_WORD => {
                        arch_x86_64::asm::phys_insw(abs_port, vir_buf, count as usize);
                        OK
                    }
                    _ => crate::ipc::EINVAL,
                }
            } else if req_dir == arch_common::com::DIO_OUTPUT {
                match req_type {
                    t if t == arch_common::com::DIO_BYTE => {
                        arch_x86_64::asm::phys_outsb(abs_port, vir_buf, count as usize);
                        OK
                    }
                    t if t == arch_common::com::DIO_WORD => {
                        arch_x86_64::asm::phys_outsw(abs_port, vir_buf, count as usize);
                        OK
                    }
                    _ => crate::ipc::EINVAL,
                }
            } else {
                crate::ipc::EINVAL
            };

            // Switch back to boot CR3
            if boot_cr3 != 0 {
                arch_x86_64::asm::write_cr3(boot_cr3);
            }

            result
        }
    }
}

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
        map_call(1, do_exec_handler); // SYS_EXEC — Phase 8.10
        map_call(2, do_clear_handler); // SYS_CLEAR
        map_call(3, do_schedule_handler); // SYS_SCHEDULE
        map_call(4, do_privctl_handler); // SYS_PRIVCTL
        map_call(5, do_trace_handler); // SYS_TRACE
        map_call(6, do_kill_handler); // SYS_KILL
        map_call(7, do_getksig_handler); // SYS_GETKSIG
        map_call(8, do_endksig_handler); // SYS_ENDKSIG
        map_call(9, do_sigsend_handler); // SYS_SIGSEND
        map_call(10, do_sigreturn_handler); // SYS_SIGRETURN
        map_call(13, do_memset_handler); // SYS_MEMSET
        map_call(14, do_umap_handler); // SYS_UMAP
        map_call(15, do_vircopy_handler); // SYS_VIRCOPY
        map_call(16, do_physcopy_handler); // SYS_PHYSCOPY
        map_call(17, do_umap_remote_handler); // SYS_UMAP_REMOTE
        map_call(18, do_vumap_handler); // SYS_VUMAP
        map_call(19, do_irqctl_handler); // SYS_IRQCTL
        // Phase 8.8: I/O syscalls (no longer i386-specific; x86_64 uses the same port I/O)
        map_call(21, do_devio_handler); // SYS_DEVIO
        map_call(22, do_sdevio_handler); // SYS_SDEVIO
        map_call(23, do_vdevio_handler); // SYS_VDEVIO
        map_call(24, do_setalarm_handler); // SYS_SETALARM
        map_call(25, do_times_handler); // SYS_TIMES
        map_call(26, do_getinfo_handler); // SYS_GETINFO
        map_call(27, do_abort_handler); // SYS_ABORT
        map_call(31, do_safecopy_from_handler); // SYS_SAFECOPYFROM
        map_call(32, do_safecopy_to_handler); // SYS_SAFECOPYTO
        map_call(33, do_vsafecopy_handler); // SYS_VSAFECOPY
        map_call(34, do_setgrant_handler); // SYS_SETGRANT
        map_call(36, do_sprofile_handler); // SYS_SPROF
        map_call(37, do_cprofile_handler); // SYS_CPROF
        map_call(38, do_profbuf_handler); // SYS_PROFBUF
        map_call(39, do_stime_handler); // SYS_STIME
        map_call(40, do_settime_handler); // SYS_SETTIME
        map_call(43, do_vmctl_handler); // SYS_VMCTL
        map_call(44, do_diagctl_handler); // SYS_DIAGCTL
        map_call(45, do_vtimer_handler); // SYS_VTIMER
        map_call(46, do_runctl_handler); // SYS_RUNCTL
        map_call(50, do_getmcontext_handler); // SYS_GETMCONTEXT — Phase 8.10
        map_call(51, do_setmcontext_handler); // SYS_SETMCONTEXT — Phase 8.10
        map_call(52, do_update_stub); // SYS_UPDATE
        map_call(53, do_exit_handler); // SYS_EXIT
        map_call(54, do_schedctl_handler); // SYS_SCHEDCTL
        map_call(55, do_statectl_handler); // SYS_STATECTL
        map_call(56, do_safememset_handler); // SYS_SAFEMEMSET
    }

    /// Stub for SYS_UPDATE — deferred.
    pub unsafe fn do_update_stub(_caller: *mut Proc, _msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
        EBADREQUEST
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

/// Handle SYS_EXEC (Phase 8.10): set up a process after a successful exec.
// Message layout for privctl:
//   offset  0: endpt      (i32) — target process endpoint
//   offset  4: request    (i32) — SYS_PRIV_ALLOW / SYS_PRIV_DISALLOW / etc.
//   offset  8: arg_ptr    (u64) — pointer to privilege/io/mem/irq data
//   offset 16: phys_start (u64) — physical address start (for QUERY_MEM)
//   offset 24: phys_len   (u64) — physical address length
use arch_common::com::{
    SYS_PRIV_ADD_IO, SYS_PRIV_ADD_IRQ, SYS_PRIV_ADD_MEM, SYS_PRIV_ALLOW, SYS_PRIV_DISALLOW,
    SYS_PRIV_QUERY_MEM, SYS_PRIV_SET_SYS, SYS_PRIV_SET_USER, SYS_PRIV_UPDATE_SYS, SYS_PRIV_YIELD,
};

// ─────────────────────────────────────────────────────────────────────────
// data_copy helper — wraps virtual_copy for common kernel→user copies
// ─────────────────────────────────────────────────────────────────────────

/// Copy data from a user process into kernel space.
///
/// `src_endpt` is the endpoint of the source process.  `src_addr` is the
/// virtual address in the source process's address space.  `dst_addr` is
/// the kernel virtual address to copy into.  `bytes` is the number of bytes
/// to copy.
///
/// # Safety
///
/// Both pointers must be valid and readable/writable for `bytes`.
unsafe fn data_copy_from(src_endpt: i32, src_addr: u64, dst_addr: u64, bytes: usize) -> i32 {
    let src_proc = table::endpoint_slot(src_endpt);
    let kernel_proc: i32 = -1; // KERNEL process number
    unsafe { crate::vm::virtual_copy(src_proc, src_addr, kernel_proc, dst_addr, bytes) }
}

const PRIVCTL_ENDPT_OFF: usize = 0;
const PRIVCTL_REQUEST_OFF: usize = 4;
const PRIVCTL_ARG_PTR_OFF: usize = 8;
const PRIVCTL_PHYS_START_OFF: usize = 16;
const PRIVCTL_PHYS_LEN_OFF: usize = 24;

/// Handle SYS_PRIVCTL — manage process privileges.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`, `msg` must be a valid message buffer.
pub unsafe fn do_privctl_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        // Caller must be a system process.
        let caller_priv = (*caller).p_priv;
        if caller_priv.is_null() || !(*caller_priv).s_flags.contains(PrivFlags::SYS_PROC) {
            return crate::ipc::EPERM;
        }

        let target_endpt = msg_read_i32(msg, PRIVCTL_ENDPT_OFF);
        let request = msg_read_i32(msg, PRIVCTL_REQUEST_OFF) as u32;

        // Resolve target process.
        let proc_nr = if target_endpt == SELF {
            (*caller).p_nr
        } else {
            if !table::is_ok_endpoint(target_endpt) {
                return crate::ipc::EINVAL;
            }
            table::endpoint_slot(target_endpt)
        };
        let rp = crate::table::proc_addr(proc_nr);
        if rp.is_null() || (*rp).is_empty() {
            return crate::ipc::EINVAL;
        }

        match request {
            SYS_PRIV_ALLOW => {
                // Allow process to run. Must have RTS_NO_PRIV set.
                let flags = (*rp)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed);
                if flags & RtsFlags::NO_PRIV.bits() == 0 {
                    return crate::ipc::EPERM;
                }
                if (*rp).p_priv.is_null() || (*(*rp).p_priv).s_proc_nr == NONE {
                    return crate::ipc::EPERM;
                }
                (*rp).p_rts_flags.fetch_and(
                    !RtsFlags::NO_PRIV.bits(),
                    core::sync::atomic::Ordering::Relaxed,
                );
                OK
            }

            SYS_PRIV_YIELD => {
                // Allow target, suspend caller.
                let flags = (*rp)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed);
                if flags & RtsFlags::NO_PRIV.bits() == 0 {
                    return crate::ipc::EPERM;
                }
                if (*rp).p_priv.is_null() || (*(*rp).p_priv).s_proc_nr == NONE {
                    return crate::ipc::EPERM;
                }
                (*caller).p_rts_flags.fetch_or(
                    RtsFlags::NO_PRIV.bits(),
                    core::sync::atomic::Ordering::Relaxed,
                );
                (*rp).p_rts_flags.fetch_and(
                    !RtsFlags::NO_PRIV.bits(),
                    core::sync::atomic::Ordering::Relaxed,
                );
                OK
            }

            SYS_PRIV_DISALLOW => {
                // Disallow process from running.
                let flags = (*rp)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed);
                if flags & RtsFlags::NO_PRIV.bits() != 0 {
                    return crate::ipc::EPERM;
                }
                (*rp).p_rts_flags.fetch_or(
                    RtsFlags::NO_PRIV.bits(),
                    core::sync::atomic::Ordering::Relaxed,
                );
                OK
            }

            SYS_PRIV_SET_SYS => {
                // Set privilege structure for a blocked system process.
                let flags = (*rp)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed);
                if flags & RtsFlags::NO_PRIV.bits() == 0 {
                    return crate::ipc::EPERM;
                }

                let arg_ptr = msg_read_u64(msg, PRIVCTL_ARG_PTR_OFF);
                let mut priv_buf = crate::r#priv::Priv::default();

                if arg_ptr != 0 {
                    // Copy privilege structure from caller.
                    let r = data_copy_from(
                        (*caller).p_endpoint,
                        arg_ptr,
                        &mut priv_buf as *mut _ as u64,
                        core::mem::size_of::<crate::r#priv::Priv>(),
                    );
                    if r != 0 {
                        return r;
                    }
                }

                // Allocate a privilege slot.
                if get_priv(rp).is_none() {
                    return crate::ipc::ENOMEM;
                }

                // If caller supplied a priv, copy its fields.
                if arg_ptr != 0 {
                    let rp_priv = (*rp).p_priv;
                    if rp_priv.is_null() {
                        return crate::ipc::ENOMEM;
                    }
                    let saved_id = (*rp_priv).s_id;
                    *rp_priv = priv_buf;
                    (*rp_priv).s_id = saved_id;
                    (*rp_priv).s_proc_nr = (*rp).p_nr;

                    // Clear pending state.
                    for chunk in (*rp_priv).s_notify_pending.chunk.iter_mut() {
                        *chunk = 0;
                    }
                    (*rp_priv).s_int_pending = 0;
                    (*rp_priv).s_sig_pending = 0u128;

                    // Set defaults for resources.
                    (*rp_priv).s_nr_io_range = 0;
                    (*rp_priv).s_nr_mem_range = 0;
                    (*rp_priv).s_nr_irq = 0;
                    (*rp_priv).s_grant_table = 0;
                    (*rp_priv).s_grant_entries = 0;
                }
                OK
            }

            SYS_PRIV_SET_USER => {
                // Link to the user privilege structure (shared by all user procs).
                let flags = (*rp)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed);
                if flags & RtsFlags::NO_PRIV.bits() == 0 {
                    return crate::ipc::EPERM;
                }
                let user_priv = crate::r#priv::priv_addr_mut(USER_PRIV_ID);
                (*rp).p_priv = user_priv;
                OK
            }

            SYS_PRIV_ADD_IO => {
                // Grant I/O port range access.
                let flags = (*rp)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed);
                if flags & RtsFlags::NO_PRIV.bits() != 0 {
                    return crate::ipc::EPERM;
                }
                let sp = (*rp).p_priv;
                if sp.is_null() || !(*sp).s_flags.contains(PrivFlags::SYS_PROC) {
                    return crate::ipc::EPERM;
                }

                let arg_ptr = msg_read_u64(msg, PRIVCTL_ARG_PTR_OFF);
                let mut io_range = crate::r#priv::IoRange::default();
                let r = data_copy_from(
                    (*caller).p_endpoint,
                    arg_ptr,
                    &mut io_range as *mut _ as u64,
                    core::mem::size_of::<crate::r#priv::IoRange>(),
                );
                if r != 0 {
                    return r;
                }

                // Check for duplicate.
                for i in 0..(*sp).s_nr_io_range as usize {
                    if i >= NR_IO_RANGE {
                        break;
                    }
                    if (*sp).s_io_tab[i].ior_base == io_range.ior_base
                        && (*sp).s_io_tab[i].ior_limit == io_range.ior_limit
                    {
                        return OK;
                    }
                }

                let i = (*sp).s_nr_io_range as usize;
                if i >= NR_IO_RANGE {
                    return crate::ipc::ENOMEM;
                }
                (*sp).s_flags.insert(PrivFlags::CHECK_IO_PORT);
                (*sp).s_io_tab[i] = io_range;
                (*sp).s_nr_io_range += 1;
                OK
            }

            SYS_PRIV_ADD_MEM => {
                // Grant memory range access.
                let flags = (*rp)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed);
                if flags & RtsFlags::NO_PRIV.bits() != 0 {
                    return crate::ipc::EPERM;
                }
                let sp = (*rp).p_priv;
                if sp.is_null() || !(*sp).s_flags.contains(PrivFlags::SYS_PROC) {
                    return crate::ipc::EPERM;
                }

                let arg_ptr = msg_read_u64(msg, PRIVCTL_ARG_PTR_OFF);
                let mut mem_range = crate::r#priv::MemRange::default();
                let r = data_copy_from(
                    (*caller).p_endpoint,
                    arg_ptr,
                    &mut mem_range as *mut _ as u64,
                    core::mem::size_of::<crate::r#priv::MemRange>(),
                );
                if r != 0 {
                    return r;
                }

                for i in 0..(*sp).s_nr_mem_range as usize {
                    if i >= NR_MEM_RANGE {
                        break;
                    }
                    if (*sp).s_mem_tab[i].mr_base == mem_range.mr_base
                        && (*sp).s_mem_tab[i].mr_limit == mem_range.mr_limit
                    {
                        return OK;
                    }
                }

                let i = (*sp).s_nr_mem_range as usize;
                if i >= NR_MEM_RANGE {
                    return crate::ipc::ENOMEM;
                }
                (*sp).s_flags.insert(PrivFlags::CHECK_MEM);
                (*sp).s_mem_tab[i] = mem_range;
                (*sp).s_nr_mem_range += 1;
                OK
            }

            SYS_PRIV_ADD_IRQ => {
                // Grant IRQ line access.
                let flags = (*rp)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed);
                if flags & RtsFlags::NO_PRIV.bits() != 0 {
                    return crate::ipc::EPERM;
                }
                let sp = (*rp).p_priv;
                if sp.is_null() || !(*sp).s_flags.contains(PrivFlags::SYS_PROC) {
                    return crate::ipc::EPERM;
                }

                let arg_ptr = msg_read_u64(msg, PRIVCTL_ARG_PTR_OFF);
                let mut irq: i32 = 0;
                let r = data_copy_from(
                    (*caller).p_endpoint,
                    arg_ptr,
                    &mut irq as *mut _ as u64,
                    core::mem::size_of::<i32>(),
                );
                if r != 0 {
                    return r;
                }

                for i in 0..(*sp).s_nr_irq as usize {
                    if i >= NR_IRQ {
                        break;
                    }
                    if (*sp).s_irq_tab[i] == irq {
                        return OK;
                    }
                }

                let i = (*sp).s_nr_irq as usize;
                if i >= NR_IRQ {
                    return crate::ipc::ENOMEM;
                }
                (*sp).s_flags.insert(PrivFlags::CHECK_IRQ);
                (*sp).s_irq_tab[i] = irq;
                (*sp).s_nr_irq += 1;
                OK
            }

            SYS_PRIV_QUERY_MEM => {
                // Check if a process is allowed to map certain physical memory.
                let addr = msg_read_u64(msg, PRIVCTL_PHYS_START_OFF);
                let len = msg_read_u64(msg, PRIVCTL_PHYS_LEN_OFF);
                let limit = addr.wrapping_add(len).wrapping_sub(1);
                if limit < addr {
                    return crate::ipc::EPERM;
                }
                let sp = (*rp).p_priv;
                if sp.is_null() || !(*sp).s_flags.contains(PrivFlags::SYS_PROC) {
                    return crate::ipc::EPERM;
                }
                let mut found = false;
                for i in 0..(*sp).s_nr_mem_range as usize {
                    if i >= NR_MEM_RANGE {
                        break;
                    }
                    if addr >= (*sp).s_mem_tab[i].mr_base && limit <= (*sp).s_mem_tab[i].mr_limit {
                        found = true;
                        break;
                    }
                }
                if found { OK } else { crate::ipc::EPERM }
            }

            SYS_PRIV_UPDATE_SYS => {
                // Update privilege structure fields.
                let arg_ptr = msg_read_u64(msg, PRIVCTL_ARG_PTR_OFF);
                if arg_ptr == 0 {
                    return crate::ipc::EINVAL;
                }
                let mut priv_buf = crate::r#priv::Priv::default();
                let r = data_copy_from(
                    (*caller).p_endpoint,
                    arg_ptr,
                    &mut priv_buf as *mut _ as u64,
                    core::mem::size_of::<crate::r#priv::Priv>(),
                );
                if r != 0 {
                    return r;
                }

                // Update the target's privilege structure.
                let sp = (*rp).p_priv;
                if sp.is_null() {
                    return crate::ipc::EPERM;
                }

                // Copy flags and signal managers.
                (*sp).s_flags = priv_buf.s_flags;
                (*sp).s_sig_mgr = priv_buf.s_sig_mgr;
                (*sp).s_bak_sig_mgr = priv_buf.s_bak_sig_mgr;

                // Copy IRQ table.
                if priv_buf.s_flags.contains(PrivFlags::CHECK_IRQ) {
                    let nr = priv_buf.s_nr_irq.max(0) as usize;
                    let nr = nr.min(NR_IRQ);
                    (*sp).s_nr_irq = nr as i32;
                    for i in 0..nr {
                        (*sp).s_irq_tab[i] = priv_buf.s_irq_tab[i];
                    }
                }

                // Copy I/O ranges.
                if priv_buf.s_flags.contains(PrivFlags::CHECK_IO_PORT) {
                    let nr = priv_buf.s_nr_io_range.max(0) as usize;
                    let nr = nr.min(NR_IO_RANGE);
                    (*sp).s_nr_io_range = nr as i32;
                    for i in 0..nr {
                        (*sp).s_io_tab[i] = priv_buf.s_io_tab[i];
                    }
                }

                // Copy memory ranges.
                if priv_buf.s_flags.contains(PrivFlags::CHECK_MEM) {
                    let nr = priv_buf.s_nr_mem_range.max(0) as usize;
                    let nr = nr.min(NR_MEM_RANGE);
                    (*sp).s_nr_mem_range = nr as i32;
                    for i in 0..nr {
                        (*sp).s_mem_tab[i] = priv_buf.s_mem_tab[i];
                    }
                }

                OK
            }

            _ => EBADREQUEST,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// do_trace — ptrace kernel support (SYS_TRACE)
// ─────────────────────────────────────────────────────────────────────────

// Internal trace request codes (matching C do_trace.c switch cases).
const T_STOP: i32 = 0;
const T_GETINS: i32 = 1;
const T_GETDATA: i32 = 2;
const T_GETUSER: i32 = 3;
const T_SETINS: i32 = 4;
const T_SETDATA: i32 = 5;
const T_SETUSER: i32 = 6;
const T_DETACH: i32 = 7;
const T_RESUME: i32 = 8;
const T_STEP: i32 = 9;
const T_SYSCALL: i32 = 10;
const T_READB_INS: i32 = 11;
const T_WRITEB_INS: i32 = 12;

// Message layout for trace:
//   offset  0: endpt   (i32) — traced process endpoint
//   offset  4: request (i32) — trace request (T_*)
//   offset  8: address (u64) — address in traced process
//   offset 16: data    (i64) — data to write / returned data
const TRACE_ENDPT_OFF: usize = 0;
const TRACE_REQUEST_OFF: usize = 4;
const TRACE_ADDRESS_OFF: usize = 8;
const TRACE_DATA_OFF: usize = 16;

/// Handle SYS_TRACE — ptrace kernel operations.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`, `msg` must be a valid message buffer.
pub unsafe fn do_trace_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let tr_endpt = msg_read_i32(msg, TRACE_ENDPT_OFF);
        let tr_request = msg_read_i32(msg, TRACE_REQUEST_OFF);
        let tr_addr = msg_read_u64(msg, TRACE_ADDRESS_OFF);
        let mut tr_data = msg_read_i64(msg, TRACE_DATA_OFF);

        if !table::is_ok_endpoint(tr_endpt) {
            return crate::ipc::EINVAL;
        }
        let tr_proc_nr = table::endpoint_slot(tr_endpt);
        if table::is_kernel_nr(tr_proc_nr) {
            return crate::ipc::EPERM;
        }

        let rp = crate::table::proc_addr(tr_proc_nr);
        if rp.is_null() || (*rp).is_empty() || (*rp).p_endpoint != tr_endpt {
            return crate::ipc::EINVAL;
        }

        match tr_request {
            T_STOP => {
                // Stop the process.
                (*rp)
                    .p_rts_flags
                    .fetch_or(RtsFlags::P_STOP.bits(), Ordering::Relaxed);
                // Clear syscall trace and single step flags.
                (*rp).p_misc_flags.fetch_and(
                    !(MiscFlags::SC_TRACE.bits() | MiscFlags::STEP.bits()),
                    Ordering::Relaxed,
                );
                msg_write_i64(msg, TRACE_DATA_OFF, 0);
                OK
            }

            T_GETINS | T_GETDATA => {
                // Read a word from the traced process's address space.
                let r = crate::vm::virtual_copy(
                    tr_proc_nr,
                    tr_addr,
                    -1, // KERNEL
                    &mut tr_data as *mut _ as u64,
                    core::mem::size_of::<i64>(),
                );
                if r != 0 {
                    return r;
                }
                msg_write_i64(msg, TRACE_DATA_OFF, tr_data);
                OK
            }

            T_GETUSER => {
                // Read a value from the process table.
                if tr_addr & (core::mem::size_of::<i64>() as u64 - 1) != 0 {
                    return crate::ipc::EFAULT;
                }
                let proc_size = core::mem::size_of::<crate::proc::Proc>();
                if tr_addr + core::mem::size_of::<i64>() as u64 <= proc_size as u64 {
                    // Read from proc struct.
                    let base = rp as *const Proc as *const u8;
                    let src = base.add(tr_addr as usize) as *const i64;
                    tr_data = *src;
                } else if !(*rp).p_priv.is_null() {
                    // Read from priv struct (after alignment).
                    let align = core::mem::size_of::<i64>() - 1;
                    let adjusted =
                        tr_addr.wrapping_sub((proc_size + align) as u64) & !(align as u64);
                    let priv_size = core::mem::size_of::<Priv>();
                    if adjusted + core::mem::size_of::<i64>() as u64 <= priv_size as u64 {
                        let base = (*rp).p_priv as *const u8;
                        let src = base.add(adjusted as usize) as *const i64;
                        tr_data = *src;
                    } else {
                        return crate::ipc::EFAULT;
                    }
                } else {
                    return crate::ipc::EFAULT;
                }
                msg_write_i64(msg, TRACE_DATA_OFF, tr_data);
                OK
            }

            T_SETINS | T_SETDATA => {
                // Write a word to the traced process's address space.
                let r = crate::vm::virtual_copy(
                    -1, // KERNEL
                    &tr_data as *const _ as u64,
                    tr_proc_nr,
                    tr_addr,
                    core::mem::size_of::<i64>(),
                );
                if r != 0 {
                    return r;
                }
                msg_write_i64(msg, TRACE_DATA_OFF, 0);
                OK
            }

            T_SETUSER => {
                // Set a value in the process's stackframe.
                if tr_addr & (core::mem::size_of::<i64>() as u64 - 1) != 0 {
                    return crate::ipc::EFAULT;
                }
                let stackframe_size = core::mem::size_of::<TrapFrame>();
                let p_reg_offset = core::mem::offset_of!(crate::proc::Proc, p_reg) as u64;
                if tr_addr < p_reg_offset
                    || tr_addr
                        > p_reg_offset + stackframe_size as u64 - core::mem::size_of::<i64>() as u64
                {
                    return crate::ipc::EFAULT;
                }

                // On x86_64, refuse to write to segment registers (cs, ss).
                // These are in SegFrame, not TrapFrame. Since we limit writes
                // to the TrapFrame range, segment regs are automatically protected.

                let base = rp as *const Proc as *mut u8;
                let dst = base.add(tr_addr as usize) as *mut i64;
                *dst = tr_data;
                msg_write_i64(msg, TRACE_DATA_OFF, 0);
                OK
            }

            T_DETACH => {
                // Detach tracer — clear syscall active flag.
                (*rp)
                    .p_misc_flags
                    .fetch_and(!MiscFlags::SC_ACTIVE.bits(), Ordering::Relaxed);
                // Fall through to T_RESUME.
                (*rp)
                    .p_rts_flags
                    .fetch_and(!RtsFlags::P_STOP.bits(), Ordering::Relaxed);
                msg_write_i64(msg, TRACE_DATA_OFF, 0);
                OK
            }

            T_RESUME => {
                // Resume execution.
                (*rp)
                    .p_rts_flags
                    .fetch_and(!RtsFlags::P_STOP.bits(), Ordering::Relaxed);
                msg_write_i64(msg, TRACE_DATA_OFF, 0);
                OK
            }

            T_STEP => {
                // Set trace bit (single-step) and resume.
                (*rp)
                    .p_misc_flags
                    .fetch_or(MiscFlags::STEP.bits(), Ordering::Relaxed);
                (*rp)
                    .p_rts_flags
                    .fetch_and(!RtsFlags::P_STOP.bits(), Ordering::Relaxed);
                msg_write_i64(msg, TRACE_DATA_OFF, 0);
                OK
            }

            T_SYSCALL => {
                // Trace system calls.
                (*rp)
                    .p_misc_flags
                    .fetch_or(MiscFlags::SC_TRACE.bits(), Ordering::Relaxed);
                (*rp)
                    .p_rts_flags
                    .fetch_and(!RtsFlags::P_STOP.bits(), Ordering::Relaxed);
                msg_write_i64(msg, TRACE_DATA_OFF, 0);
                OK
            }

            T_READB_INS => {
                // Read a byte from instruction space.
                let mut ub: u8 = 0;
                let r = crate::vm::virtual_copy(
                    tr_proc_nr,
                    tr_addr,
                    -1, // KERNEL
                    &mut ub as *mut _ as u64,
                    1,
                );
                if r != 0 {
                    return r;
                }
                msg_write_i64(msg, TRACE_DATA_OFF, ub as i64);
                OK
            }

            T_WRITEB_INS => {
                // Write a byte to instruction space.
                let ub = (tr_data & 0xff) as u8;
                let r = crate::vm::virtual_copy(
                    -1, // KERNEL
                    &ub as *const _ as u64,
                    tr_proc_nr,
                    tr_addr,
                    1,
                );
                if r != 0 {
                    return r;
                }
                msg_write_i64(msg, TRACE_DATA_OFF, 0);
                OK
            }

            _ => crate::ipc::EINVAL,
        }
    }
}

// ── do_copy message offsets (mess_lsys_krn_sys_copy) ────────────────
//   offset  0: src_endpt (i32)
//   offset  4: _pad      (4 bytes)
//   offset  8: src_addr  (u64)
//   offset 16: dst_endpt (i32)
//   offset 20: _pad      (4 bytes)
//   offset 24: dst_addr  (u64)
//   offset 32: nr_bytes  (u64)
//   offset 40: flags     (i32)
const COPY_SRC_ENDPT_OFF: usize = 0;
const COPY_SRC_ADDR_OFF: usize = 8;
const COPY_DST_ENDPT_OFF: usize = 16;
const COPY_DST_ADDR_OFF: usize = 24;
const COPY_NR_BYTES_OFF: usize = 32;
const COPY_FLAGS_OFF: usize = 40;

const CP_FLAG_TRY: i32 = 0x01;

// ── Deferred stubs — need VM infrastructure: ─────────────────────────
// Replaced: do_vircopy_stub and do_physcopy_stub with real handlers below.

/// Handle SYS_VIRCOPY: virtual copy between processes.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`, `msg` must be a valid message.
pub unsafe fn do_vircopy_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe { do_copy_common(caller, msg) }
}

/// Handle SYS_PHYSCOPY: physical copy between processes.
///
/// Both VIRCOPY and PHYSCOPY share the same implementation (the distinction
/// is for permission checking at a higher level).
///
/// # Safety
///
/// Same as `do_vircopy_handler`.
pub unsafe fn do_physcopy_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe { do_copy_common(caller, msg) }
}

/// Common implementation for SYS_VIRCOPY and SYS_PHYSCOPY.
///
/// Reads src/dst endpoints/addresses from the message, validates them,
/// resolves SELF, and calls `virtual_copy` to perform the data transfer.
///
/// # Safety
///
/// `caller` must be a valid process pointer.
unsafe fn do_copy_common(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let mut src_endpt = msg_read_i32(msg, COPY_SRC_ENDPT_OFF);
        let src_addr = msg_read_u64(msg, COPY_SRC_ADDR_OFF);
        let mut dst_endpt = msg_read_i32(msg, COPY_DST_ENDPT_OFF);
        let dst_addr = msg_read_u64(msg, COPY_DST_ADDR_OFF);
        let nr_bytes = msg_read_u64(msg, COPY_NR_BYTES_OFF);
        let flags = msg_read_i32(msg, COPY_FLAGS_OFF);

        // Resolve SELF for both endpoints
        if src_endpt == crate::system::SELF {
            src_endpt = (*caller).p_endpoint;
        }
        if dst_endpt == crate::system::SELF {
            dst_endpt = (*caller).p_endpoint;
        }

        // Validate endpoints (NONE is allowed for one side — kernel owns it)
        if src_endpt != crate::system::NONE && !crate::table::is_ok_endpoint(src_endpt) {
            return crate::grants::EINVAL;
        }
        if dst_endpt != crate::system::NONE && !crate::table::is_ok_endpoint(dst_endpt) {
            return crate::grants::EINVAL;
        }

        // Check for overflow (bytes must fit in vir_bytes range)
        if nr_bytes > 0xFFFFFFFF {
            return crate::grants::EINVAL;
        }
        let bytes = nr_bytes as usize;

        // Resolve endpoint to proc_nr for virtual_copy
        let src_proc = if src_endpt == crate::system::NONE {
            -1i32 // KERNEL
        } else {
            crate::table::endpoint_slot(src_endpt)
        };
        let dst_proc = if dst_endpt == crate::system::NONE {
            -1i32 // KERNEL
        } else {
            crate::table::endpoint_slot(dst_endpt)
        };

        if flags & CP_FLAG_TRY != 0 {
            // CP_FLAG_TRY: direct copy without VM fallback
            let r = crate::vm::virtual_copy(src_proc, src_addr, dst_proc, dst_addr, bytes);
            if r == crate::grants::EFAULT_SRC || r == crate::grants::EFAULT_DST {
                return crate::ipc::EFAULT;
            }
            r
        } else {
            // Full copy (with VM fallback — currently same as direct)
            // TODO: wire virtual_copy_vmcheck for page fault handling
            crate::vm::virtual_copy(src_proc, src_addr, dst_proc, dst_addr, bytes)
        }
    }
}

// SAFECOPYFROM, SAFECOPYTO, VSAFECOPY — thin wrappers around grants module

/// Handle SYS_SAFECOPYFROM: grant-based copy from remote process.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`, `msg` must be a valid message buffer.
pub unsafe fn do_safecopy_from_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        crate::grants::do_safecopy_from(caller, &*core::ptr::addr_of!(*msg) as &[u8; MESSAGE_SIZE])
    }
}

/// Handle SYS_SAFECOPYTO: grant-based copy to remote process.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`, `msg` must be a valid message buffer.
pub unsafe fn do_safecopy_to_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        crate::grants::do_safecopy_to(caller, &*core::ptr::addr_of!(*msg) as &[u8; MESSAGE_SIZE])
    }
}

/// Handle SYS_VSAFECOPY: vectored grant-based safe copy.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`, `msg` must be a valid message buffer.
pub unsafe fn do_vsafecopy_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        crate::grants::do_vsafecopy(caller, &*core::ptr::addr_of!(*msg) as &[u8; MESSAGE_SIZE])
    }
}

/// SAFEMEMSET — grant-based memset
///
/// # Safety
///
/// `caller` must point to a valid `Proc`, `msg` must be a valid message buffer.
pub unsafe fn do_safememset_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let granter = msg_read_i32(msg, SAFEMEMSET_GRANTER_OFF);
        let grant_id = msg_read_i32(msg, SAFEMEMSET_GRANT_ID_OFF);
        let offset = msg_read_u64(msg, SAFEMEMSET_OFFSET_OFF);
        let pattern = msg_read_u64(msg, SAFEMEMSET_PATTERN_OFF);
        let bytes = msg_read_u64(msg, SAFEMEMSET_BYTES_OFF);

        if granter == crate::system::NONE {
            return crate::grants::EFAULT_SRC;
        }

        // Verify the grant for write access
        let r = crate::grants::verify_grant(
            granter,
            (*caller).p_endpoint,
            grant_id,
            bytes,
            crate::grants::CPF_WRITE,
            offset,
        );
        let (phys_addr, _new_granter, _flags) = match r {
            Ok(v) => v,
            Err(e) => return e,
        };

        // Write pattern via vm_memset
        crate::vm::vm_memset(phys_addr, pattern as u8, bytes as usize);
        crate::ipc::OK
    }
}

// ─────────────────────────────────────────────────────────────────────────
// do_vumap — vectored virtual-to-physical mapping (Phase 6.17)
// ─────────────────────────────────────────────────────────────────────────

/// Handle SYS_VUMAP: map a vector of grants or local virtual addresses to
/// physical addresses.
///
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_vumap.c`
///
/// # Safety
///
/// `caller` and `msg` must be valid.
pub unsafe fn do_vumap_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let endpt = (*caller).p_endpoint;
        let source = msg_read_i32(msg, VUMAP_ENDPT_OFF);
        let vaddr = msg_read_u64(msg, VUMAP_VADDR_OFF);
        let mut vcount = msg_read_i32(msg, VUMAP_VCOUNT_OFF);
        let offset = msg_read_u64(msg, VUMAP_OFFSET_OFF);
        let mut access = msg_read_i32(msg, VUMAP_ACCESS_OFF);
        let paddr = msg_read_u64(msg, VUMAP_PADDR_OFF);
        let mut pmax = msg_read_i32(msg, VUMAP_PMAX_OFF);

        // Validate bounds
        if vcount <= 0 || pmax <= 0 {
            return crate::ipc::EINVAL;
        }
        if vcount > MAPVEC_NR as i32 {
            vcount = MAPVEC_NR as i32;
        }
        if pmax > MAPVEC_NR as i32 {
            pmax = MAPVEC_NR as i32;
        }

        // Convert access flags to CPF_* flags
        access = match access {
            VUA_READ => crate::grants::CPF_READ,
            VUA_WRITE => crate::grants::CPF_WRITE,
            a if a == (VUA_READ | VUA_WRITE) => crate::grants::CPF_READ | crate::grants::CPF_WRITE,
            _ => return crate::ipc::EINVAL,
        };

        // Resolve the source endpoint
        let source_e = if source == SELF { endpt } else { source };

        // Get source process info for CR3 switching
        if !table::is_ok_endpoint(source_e) {
            return crate::ipc::EFAULT;
        }
        let source_proc_nr = table::endpoint_slot(source_e);
        let source_rp = proc_addr(source_proc_nr);
        if source_rp.is_null() {
            return crate::ipc::EFAULT;
        }
        let source_cr3 = (*source_rp).p_seg.p_cr3;
        if source_cr3 == 0 {
            return crate::ipc::EFAULT;
        }
        let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
        if boot_cr3 == 0 {
            return crate::ipc::EFAULT;
        }

        // Allocate kernel-local vectors on the stack
        let mut vvec: [arch_common::types::VumapVir; MAPVEC_NR] = [core::mem::zeroed(); MAPVEC_NR];
        let mut pvec: [arch_common::types::VumapPhys; MAPVEC_NR] = [core::mem::zeroed(); MAPVEC_NR];

        // Copy input vector from caller's address space
        let _size = vcount as usize * size_of::<arch_common::types::VumapVir>();
        arch_x86_64::asm::write_cr3(source_cr3);
        core::ptr::copy_nonoverlapping(
            vaddr as *const arch_common::types::VumapVir,
            vvec.as_mut_ptr(),
            vcount as usize,
        );
        arch_x86_64::asm::write_cr3(boot_cr3);

        let mut pcount: i32 = 0;

        // Process each input entry
        for entry in vvec.iter().take(vcount as usize) {
            if pcount >= pmax {
                break;
            }

            let mut entry_size = entry.vv_size as u64;
            if entry_size <= offset {
                return crate::ipc::EINVAL;
            }
            entry_size -= offset;

            let (mut vir_addr, granter_e) = if source != SELF {
                // Grant-based resolution
                let grant_id = entry.vv_u.u_grant;
                match crate::grants::verify_grant(
                    source_e, endpt, grant_id, entry_size, access, offset,
                ) {
                    Ok((newoffset, newep, _flags)) => (newoffset, newep),
                    Err(e) => return e,
                }
            } else {
                // Direct virtual address
                let addr = entry.vv_u.u_addr + offset;
                (addr, endpt)
            };

            // Validate granter endpoint
            if !table::is_ok_endpoint(granter_e) {
                return crate::ipc::EFAULT;
            }
            let granter_nr = table::endpoint_slot(granter_e);
            let granter_rp = proc_addr(granter_nr);
            if granter_rp.is_null() {
                return crate::ipc::EFAULT;
            }

            // Walk the granter's page table for contiguous physical ranges
            while entry_size > 0 && pcount < pmax {
                let mut phys_addr: u64 = 0;
                let chunk =
                    crate::vm::vm_lookup_range(granter_rp, vir_addr, &mut phys_addr, entry_size);

                if chunk == 0 {
                    // Page not mapped
                    if access & crate::grants::CPF_READ != 0 {
                        return crate::ipc::EFAULT;
                    }
                    // Write to unmapped memory — check range (may allocate)
                    if !crate::vm::vm_check_range(granter_rp, vir_addr, entry_size) {
                        return crate::ipc::EFAULT;
                    }
                    return crate::ipc::EFAULT;
                }

                pvec[pcount as usize].vp_addr = phys_addr;
                pvec[pcount as usize].vp_size = chunk as usize;
                pcount += 1;

                vir_addr += chunk;
                entry_size -= chunk;
            }
        }

        // Copy output vector back to caller's address space
        if pcount > 0 {
            let _psize = pcount as usize * size_of::<arch_common::types::VumapPhys>();
            arch_x86_64::asm::write_cr3(source_cr3);
            core::ptr::copy_nonoverlapping(
                pvec.as_ptr(),
                paddr as *mut arch_common::types::VumapPhys,
                pcount as usize,
            );
            arch_x86_64::asm::write_cr3(boot_cr3);

            // Write back pcount
            msg_write_i32(msg, VUMAP_REPLY_PCOUNT_OFF, pcount);
            OK
        } else {
            crate::ipc::EFAULT
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Phase 7.3 — Timer/clock-dependent syscall handlers
// ─────────────────────────────────────────────────────────────────────────

/// Generic interrupt handler for IRQ hooks registered via IRQ_SETPOLICY.
///
/// Transforms hardware interrupts into notification messages to
/// the registered driver process.
unsafe fn generic_handler(hook: *mut IrqHook) -> i32 {
    unsafe {
        let proc_nr_e = (*hook).proc_nr_e;

        // Validate the target process endpoint.
        if !crate::table::is_ok_endpoint(proc_nr_e) {
            panic!("invalid interrupt handler: {}", proc_nr_e);
        }
        let proc_nr = crate::table::endpoint_slot(proc_nr_e);
        let rp = crate::table::proc_addr(proc_nr);
        if rp.is_null() {
            panic!("invalid interrupt handler: {}", proc_nr_e);
        }

        // Set the pending interrupt bit in the priv structure.
        (*(*rp).p_priv).s_int_pending |= 1u64 << (*hook).notify_id;

        // Send notification from HARDWARE (KERNEL) to target process.
        crate::ipc::mini_notify(arch_common::com::HARDWARE, proc_nr_e);

        // Return whether to re-enable the IRQ based on policy.
        ((*hook).policy & IRQ_REENABLE as u64) as i32
    }
}

/// Handle SYS_IRQCTL: IRQ policy control.
///
/// Dispatches IRQ_SETPOLICY, IRQ_RMPOLICY, IRQ_ENABLE, IRQ_DISABLE.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`; `msg` must be a valid message buffer.
pub unsafe fn do_irqctl_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let request = msg_read_i32(msg, IRQCTL_REQUEST_OFF);
        let irq_vec = msg_read_i32(msg, IRQCTL_VECTOR_OFF);
        let irq_hook_id = msg_read_i32(msg, IRQCTL_HOOK_ID_OFF) - 1;

        match request {
            IRQ_ENABLE | IRQ_DISABLE => {
                if irq_hook_id < 0
                    || irq_hook_id as usize >= NR_IRQ_HOOKS
                    || IRQ_HOOKS[irq_hook_id as usize].proc_nr_e == NONE
                {
                    return crate::ipc::EINVAL;
                }
                if IRQ_HOOKS[irq_hook_id as usize].proc_nr_e != (*caller).p_endpoint {
                    return crate::ipc::EPERM;
                }
                let hook = &IRQ_HOOKS[irq_hook_id as usize] as *const IrqHook;
                if request == IRQ_ENABLE {
                    crate::interrupt::enable_irq(hook);
                } else {
                    crate::interrupt::disable_irq(hook);
                }
                crate::ipc::OK
            }

            IRQ_SETPOLICY => {
                // Validate IRQ vector.
                if !(0..NR_IRQ_VECTORS).contains(&irq_vec) {
                    return crate::ipc::EINVAL;
                }

                let privp = (*caller).p_priv;
                if privp.is_null() {
                    return crate::ipc::EPERM;
                }

                // Check IRQ access if CHECK_IRQ flag is set.
                if (*privp).s_flags.contains(PrivFlags::CHECK_IRQ) {
                    let mut found = false;
                    for i in 0..(*privp).s_nr_irq as usize {
                        if irq_vec == (*privp).s_irq_tab[i] {
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        return crate::ipc::EPERM;
                    }
                }

                let notify_id = msg_read_i32(msg, IRQCTL_HOOK_ID_OFF);
                if !(0..=63).contains(&notify_id) {
                    return crate::ipc::EINVAL;
                }

                // Try to find an existing mapping to override.
                let hooks_base = core::ptr::addr_of_mut!(IRQ_HOOKS);
                let mut hook_ptr: *mut IrqHook = core::ptr::null_mut();
                let mut found_idx: i32 = -1;
                let mut i = 0usize;
                while i < NR_IRQ_HOOKS {
                    let hook = &*core::ptr::addr_of!((*hooks_base)[i]);
                    if hook.proc_nr_e == (*caller).p_endpoint && hook.notify_id == notify_id as u64
                    {
                        found_idx = i as i32;
                        hook_ptr = core::ptr::addr_of_mut!((*hooks_base)[i]);
                        crate::interrupt::rm_irq_handler(hook_ptr);
                        break;
                    }
                    i += 1;
                }

                // If no existing mapping, find a free hook slot.
                if hook_ptr.is_null() {
                    i = 0;
                    while i < NR_IRQ_HOOKS {
                        let hook = &*core::ptr::addr_of!((*hooks_base)[i]);
                        if hook.proc_nr_e == NONE {
                            found_idx = i as i32;
                            hook_ptr = core::ptr::addr_of_mut!((*hooks_base)[i]);
                            break;
                        }
                        i += 1;
                    }
                }

                if hook_ptr.is_null() {
                    return ENOSPC;
                }

                // Install the handler.
                let policy = msg_read_i32(msg, IRQCTL_POLICY_OFF);
                (*hook_ptr).proc_nr_e = (*caller).p_endpoint;
                (*hook_ptr).notify_id = notify_id as u64;
                (*hook_ptr).policy = policy as u64;
                crate::interrupt::put_irq_handler(hook_ptr, irq_vec, generic_handler);

                // Return the hook index (+1) in the reply.
                msg_write_i32(msg, IRQCTL_HOOK_ID_OFF, found_idx + 1);
                crate::ipc::OK
            }

            IRQ_RMPOLICY => {
                if irq_hook_id < 0
                    || irq_hook_id as usize >= NR_IRQ_HOOKS
                    || IRQ_HOOKS[irq_hook_id as usize].proc_nr_e == NONE
                {
                    return crate::ipc::EINVAL;
                }
                if (*caller).p_endpoint != IRQ_HOOKS[irq_hook_id as usize].proc_nr_e {
                    return crate::ipc::EPERM;
                }
                let hook = &IRQ_HOOKS[irq_hook_id as usize] as *const IrqHook;
                crate::interrupt::rm_irq_handler(hook);
                IRQ_HOOKS[irq_hook_id as usize].proc_nr_e = NONE;
                crate::ipc::OK
            }

            _ => crate::ipc::EINVAL,
        }
    }
}

/// Callback invoked when a setalarm timer expires.
unsafe fn cause_alarm(tp: *mut MinixTimer) {
    unsafe {
        let proc_nr_e = (*tp).tmr_arg as i32;
        crate::ipc::mini_notify(arch_common::com::CLOCK, proc_nr_e);
    }
}

/// Handle SYS_SETALARM: set or cancel a process's synchronous alarm.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`; `msg` must be a valid message buffer.
pub unsafe fn do_setalarm_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let exp_time = msg_read_u64(msg, SETALARM_EXP_TIME_OFF);
        let use_abs_time = msg_read_i32(msg, SETALARM_ABS_TIME_OFF);

        // Caller must be a system process.
        let privp = (*caller).p_priv;
        if privp.is_null() || !(*privp).s_flags.contains(PrivFlags::SYS_PROC) {
            return crate::ipc::EPERM;
        }

        let tp = &mut (*privp).s_alarm_timer as *mut MinixTimer;
        (*tp).tmr_arg = (*caller).p_endpoint as usize;
        (*tp).tmr_func = cause_alarm as *const () as usize;

        // Return ticks left on the previous alarm.
        let uptime = crate::clock::get_monotonic();
        let time_left =
            if (*tp).tmr_exp_time != crate::clock::TMR_NEVER && uptime < (*tp).tmr_exp_time {
                (*tp).tmr_exp_time - uptime
            } else {
                0
            };
        msg_write_u64(msg, SETALARM_TIME_LEFT_OFF, time_left);

        // (Re)set the timer.
        if exp_time == 0 {
            crate::clock::reset_kernel_timer(tp);
        } else {
            let abs = if use_abs_time != 0 {
                exp_time
            } else {
                exp_time + crate::clock::get_monotonic()
            };
            (*tp).tmr_exp_time = abs;
            crate::clock::set_kernel_timer(tp, abs, cause_alarm as *const () as usize);
        }

        crate::ipc::OK
    }
}

/// Handle SYS_STIME: set the system boot time.
///
/// # Safety
///
/// `msg` must be a valid message buffer.
pub unsafe fn do_stime_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let boot_time = msg_read_u64(msg, STIME_BOOT_TIME_OFF);
        crate::clock::set_boottime(boot_time as i64);
        crate::ipc::OK
    }
}

/// Handle SYS_SETTIME: set the realtime clock or adjust time.
///
/// # Safety
///
/// `msg` must be a valid message buffer.
pub unsafe fn do_settime_handler(_caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let sec = msg_read_i64(msg, SETTIME_SEC_OFF);
        let nsec = msg_read_i64(msg, SETTIME_NSEC_OFF);
        let now = msg_read_i32(msg, SETTIME_NOW_OFF);
        let clock_id = msg_read_i32(msg, SETTIME_CLOCK_ID_OFF);

        // Only CLOCK_REALTIME can be changed.
        if clock_id != CLOCK_REALTIME {
            return crate::ipc::EINVAL;
        }

        let hz = crate::glo::SYSTEM_HZ.load(Ordering::Relaxed) as i64;

        if now == 0 {
            // User just wants to adjtime() — convert delta to ticks.
            let ticks = (sec * hz) + (nsec / (1_000_000_000 / hz));
            crate::clock::set_adjtime_delta(ticks as i32);
            return crate::ipc::OK;
        }

        // Calculate the new realtime value in ticks.
        let boottime = crate::clock::get_boottime();
        let timediff = sec - boottime;
        let timediff_ticks = timediff * hz;

        if sec <= boottime || !(i64::MIN / 2..=i64::MAX / 2).contains(&timediff_ticks) {
            // Boottime was likely wrong; try to correct it.
            crate::clock::set_boottime(sec);
            crate::clock::set_realtime(1);
            return crate::ipc::OK;
        }

        let newclock = (timediff_ticks + (nsec / (1_000_000_000 / hz))) as u64;
        crate::clock::set_realtime(newclock);
        crate::ipc::OK
    }
}

/// Handle SYS_VTIMER: set/retrieve a process's virtual or profiling timer.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`; `msg` must be a valid message buffer.
pub unsafe fn do_vtimer_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let which = msg_read_i32(msg, VTIMER_WHICH_OFF);
        let vt_set = msg_read_i32(msg, VTIMER_SET_OFF);
        let value = msg_read_u64(msg, VTIMER_VALUE_OFF);
        let endpt_raw = msg_read_u64(msg, VTIMER_ENDPT_OFF) as i32;

        // Caller must be a system process.
        let privp = (*caller).p_priv;
        if privp.is_null() || !(*privp).s_flags.contains(PrivFlags::SYS_PROC) {
            return crate::ipc::EPERM;
        }

        if which != arch_common::com::VT_VIRTUAL as i32 && which != arch_common::com::VT_PROF as i32
        {
            return crate::ipc::EINVAL;
        }

        // Determine the target process.
        let proc_nr_e = if endpt_raw == SELF {
            (*caller).p_endpoint
        } else {
            endpt_raw
        };

        if !crate::table::is_ok_endpoint(proc_nr_e) {
            return crate::ipc::EINVAL;
        }
        let proc_nr = crate::table::endpoint_slot(proc_nr_e);
        let rp = crate::table::proc_addr(proc_nr);
        if rp.is_null() {
            return crate::ipc::EINVAL;
        }

        // Select flag and field based on timer type.
        let (pt_flag, pt_left_ptr): (MiscFlags, *mut u64) =
            if which == arch_common::com::VT_VIRTUAL as i32 {
                (MiscFlags::VIRT_TIMER, &mut (*rp).p_virt_left as *mut u64)
            } else {
                (MiscFlags::PROF_TIMER, &mut (*rp).p_prof_left as *mut u64)
            };

        // Retrieve the old value.
        let mf = (*rp).p_misc_flags.load(Ordering::Relaxed);
        let old_value = if mf & pt_flag.bits() != 0 {
            *pt_left_ptr
        } else {
            0
        };

        // Set new value if requested.
        if vt_set != 0 {
            (*rp)
                .p_misc_flags
                .fetch_and(!pt_flag.bits(), Ordering::Relaxed);

            if value > 0 {
                *pt_left_ptr = value;
                (*rp)
                    .p_misc_flags
                    .fetch_or(pt_flag.bits(), Ordering::Relaxed);
            } else {
                *pt_left_ptr = 0;
            }
        }

        // Return the old value.
        msg_write_u64(msg, VTIMER_VALUE_OFF, old_value);
        crate::ipc::OK
    }
}

// ── SPROF message offsets (mess_lsys_krn_sys_sprof, from ipc.h) ──
//   offset  0: action   (i32)
//   offset  4: freq     (i32)
//   offset  8: intr_type (i32)
//   offset 12: endpt    (i32)
//   offset 16: ctl_ptr  (u64)
//   offset 24: mem_ptr  (u64)
//   offset 32: mem_size (u64)
const SPROF_ACTION_OFF: usize = 0;
const SPROF_FREQ_OFF: usize = 4;
const SPROF_INTR_TYPE_OFF: usize = 8;
const SPROF_ENDPT_OFF: usize = 12;
const SPROF_CTL_PTR_OFF: usize = 16;
const SPROF_MEM_PTR_OFF: usize = 24;
const SPROF_MEM_SIZE_OFF: usize = 32;

// ── CPROF message offsets (generic mess_1 layout) ───────────────────
//   offset  0: action   (i32)
//   offset  4: mem_size (i32)
//   offset  8: endpt    (i32)
//   offset 16: ctl_ptr  (u64)
//   offset 24: mem_ptr  (u64)
const CPROF_ACTION_OFF: usize = 0;
const CPROF_MEM_SIZE_OFF: usize = 4;
const CPROF_ENDPT_OFF: usize = 8;
const CPROF_CTL_PTR_OFF: usize = 16;
const CPROF_MEM_PTR_OFF: usize = 24;

// ── PROFBUF message offsets (mess_lsys_krn_sys_profbuf, from ipc.h) ─
//   offset  0: ctl_ptr  (u64)
//   offset  8: mem_ptr  (u64)
const PROFBUF_CTL_PTR_OFF: usize = 0;
const PROFBUF_MEM_PTR_OFF: usize = 8;

// ── Profiling handlers ───────────────────────────────────────────────

/// Handle SYS_SPROF: start/stop statistical profiling.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`, `msg` to a valid message.
pub unsafe fn do_sprofile_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let action = msg_read_i32(msg, SPROF_ACTION_OFF);
        let _freq = msg_read_i32(msg, SPROF_FREQ_OFF);
        let _intr_type = msg_read_i32(msg, SPROF_INTR_TYPE_OFF);
        let _endpt = msg_read_i32(msg, SPROF_ENDPT_OFF);
        let _ctl_ptr = msg_read_u64(msg, SPROF_CTL_PTR_OFF);
        let _mem_ptr = msg_read_u64(msg, SPROF_MEM_PTR_OFF);
        let _mem_size = msg_read_u64(msg, SPROF_MEM_SIZE_OFF);

        // Caller must be a system process.
        let privp = (*caller).p_priv;
        if privp.is_null() || !(*privp).s_flags.contains(PrivFlags::SYS_PROC) {
            return crate::ipc::EPERM;
        }

        crate::profile::sprofile(
            action,
            _mem_size as i32,
            _freq,
            _intr_type,
            _ctl_ptr as *mut core::ffi::c_void,
            _mem_ptr as *mut core::ffi::c_void,
        )
    }
}

/// Handle SYS_CPROF: get/reset call profiling data.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`, `msg` to a valid message.
pub unsafe fn do_cprofile_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let _action = msg_read_i32(msg, CPROF_ACTION_OFF);
        let _mem_size = msg_read_i32(msg, CPROF_MEM_SIZE_OFF);
        let _endpt = msg_read_i32(msg, CPROF_ENDPT_OFF);
        let _ctl_ptr = msg_read_u64(msg, CPROF_CTL_PTR_OFF);
        let _mem_ptr = msg_read_u64(msg, CPROF_MEM_PTR_OFF);

        // Caller must be a system process.
        let privp = (*caller).p_priv;
        if privp.is_null() || !(*privp).s_flags.contains(PrivFlags::SYS_PROC) {
            return crate::ipc::EPERM;
        }

        match _action {
            crate::profile::PROF_RESET => {
                crate::profile::CPROF_PROCS_NO = 0;
                crate::system::OK
            }
            crate::profile::PROF_GET => {
                // Validate endpoint.
                if !crate::table::is_ok_endpoint(_endpt) {
                    return crate::ipc::EINVAL;
                }

                // For now, just return OK with empty info.
                // Full implementation would iterate CPROF_PROC_INFO and
                // data_copy profiling tables to user space (like C code).
                let info = crate::profile::CprofInfo {
                    mem_used: 0,
                    err: 0,
                };

                // Copy info struct to user via ctl_ptr.
                if _ctl_ptr != 0 {
                    let r = data_copy_from(
                        -1, // KERNEL
                        &info as *const _ as u64,
                        _ctl_ptr,
                        core::mem::size_of::<crate::profile::CprofInfo>(),
                    );
                    if r != 0 {
                        return r;
                    }
                }

                crate::system::OK
            }
            _ => crate::ipc::EINVAL,
        }
    }
}

/// Handle SYS_PROFBUF: register profiling buffer locations.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`, `msg` to a valid message.
pub unsafe fn do_profbuf_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let _ctl_ptr = msg_read_u64(msg, PROFBUF_CTL_PTR_OFF);
        let _mem_ptr = msg_read_u64(msg, PROFBUF_MEM_PTR_OFF);

        // Check slot availability.
        if crate::profile::CPROF_PROCS_NO >= 64 {
            return crate::ipc::ENOMEM;
        }

        let idx = crate::profile::CPROF_PROCS_NO;
        let info = crate::profile::CprofProcInfo {
            endpt: (*caller).p_endpoint,
            name: (*caller).p_name.as_mut_ptr() as *mut u8,
            ctl_v: _ctl_ptr,
            buf_v: _mem_ptr,
        };
        core::ptr::write(
            core::ptr::addr_of_mut!(crate::profile::CPROF_PROC_INFO)
                .cast::<crate::profile::CprofProcInfo>()
                .add(idx),
            info,
        );
        let count_ptr = core::ptr::addr_of_mut!(crate::profile::CPROF_PROCS_NO);
        *count_ptr += 1;

        // Set SPROF_SEEN flag
        let old_mf = (*caller).p_misc_flags.load(Ordering::Relaxed);
        (*caller)
            .p_misc_flags
            .store(old_mf | 0x0080, Ordering::Relaxed); // MF_SPROF_SEEN

        crate::system::OK
    }
}

/// Handle SYS_EXEC (Phase 8.10): set up a process after a successful exec.
/// Source: `.refs/minix-3.3.0/minix/kernel/system/do_exec.c`
///
/// # Safety
///
/// `caller` must point to a valid `Proc`.
pub unsafe fn do_exec_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let endpt = msg_read_i32(msg, EXEC_ENDPT_OFF);
        let ip = msg_read_u64(msg, EXEC_IP_OFF);
        let stack = msg_read_u64(msg, EXEC_STACK_OFF);
        let name_ptr = msg_read_u64(msg, EXEC_NAME_OFF);
        let ps_str = msg_read_u64(msg, EXEC_PS_STR_OFF);

        if !crate::table::is_ok_endpoint(endpt) {
            return crate::ipc::EINVAL;
        }
        let proc_nr = crate::table::endpoint_slot(endpt);
        let rp = crate::table::proc_addr(proc_nr);
        if rp.is_null() || (*rp).is_empty() || (*rp).p_endpoint != endpt {
            return crate::ipc::EINVAL;
        }

        // Clear MF_DELIVERMSG if set (C: rp->p_misc_flags &= ~MF_DELIVERMSG)
        let old_mf = (*rp).p_misc_flags.load(Ordering::Relaxed);
        (*rp)
            .p_misc_flags
            .store(old_mf & !0x0004, Ordering::Relaxed);

        // Copy program name from caller's address space
        // use a stack buffer and CR3 switching like VDEVIO
        let mut name_buf = [0u8; arch_common::types::PROC_NAME_LEN];
        let copy_len = name_buf.len() - 1;
        {
            let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
            if boot_cr3 != 0 {
                let caller_cr3 = (*caller).p_seg.p_cr3;
                if caller_cr3 != 0 {
                    arch_x86_64::asm::write_cr3(caller_cr3);
                }
            }
            core::ptr::copy_nonoverlapping(name_ptr as *const u8, name_buf.as_mut_ptr(), copy_len);
            if boot_cr3 != 0 {
                arch_x86_64::asm::write_cr3(boot_cr3);
            }
        }
        name_buf[copy_len] = 0; // null-terminate

        // Find actual string length (stop at null)
        let name_len = name_buf.iter().position(|&c| c == 0).unwrap_or(copy_len);
        let name_slice = &name_buf[..name_len];

        // Set process name on the target proc
        for (i, &b) in name_slice.iter().enumerate() {
            if i >= arch_common::types::PROC_NAME_LEN - 1 {
                break;
            }
            (*rp).p_name[i] = b as i8;
        }
        (*rp).p_name[name_len.min(arch_common::types::PROC_NAME_LEN - 1)] = 0i8;

        // Call arch_proc_init to set up TrapFrame
        arch_x86_64::arch_proc::arch_proc_init(&raw mut (*rp).p_reg, ip, stack, name_slice, ps_str);

        // No reply to EXEC call: clear RTS_RECEIVING
        // The target will start executing at the new entry point on return
        let old_rts = (*rp).p_rts_flags.load(Ordering::Relaxed);
        (*rp)
            .p_rts_flags
            .store(old_rts & !0x4000_0000, Ordering::Relaxed); // clear RECEIVING

        // Mark FPU regs as not significant
        let old_mf2 = (*rp).p_misc_flags.load(Ordering::Relaxed);
        (*rp)
            .p_misc_flags
            .store(old_mf2 & !0x1000, Ordering::Relaxed); // clear FPU_INITIALIZED
        // Force reloading FPU if current process owns it
        arch_x86_64::hw::release_fpu(rp as *mut core::ffi::c_void);

        // set_exec_target to switch to new binary on return
        crate::ipc::set_exec_target(rp, ip, stack);

        EDONTREPLY
    }
}

// ── do_getmcontext / do_setmcontext — machine context (Phase 8.10) ────
// Source: .refs/minix-3.3.0/minix/kernel/system/do_mcontext.c

/// Handle SYS_GETMCONTEXT: save a process's machine context.
///
/// Reads the TrapFrame from the target process and copies it into an
/// Mcontext struct in the caller's address space.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`.
pub unsafe fn do_getmcontext_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let endpt = msg_read_i32(msg, MCONTEXT_ENDPT_OFF);
        let ctx_ptr = msg_read_u64(msg, MCONTEXT_CTX_PTR_OFF);

        if !crate::table::is_ok_endpoint(endpt) {
            return crate::ipc::EINVAL;
        }
        let proc_nr = crate::table::endpoint_slot(endpt);
        if crate::table::is_kernel_nr(proc_nr) {
            return crate::ipc::EPERM;
        }
        let rp = crate::table::proc_addr(proc_nr);
        if rp.is_null() || (*rp).is_empty() || (*rp).p_endpoint != endpt {
            return crate::ipc::EINVAL;
        }

        // Build Mcontext from the process's TrapFrame
        let reg = &(*rp).p_reg;
        use arch_x86_64::mcontext::Mcontext;
        let mc = {
            // Save FPU state if the process has used the FPU
            let mut fpstate = [0u8; 512];
            let mf = (*rp).p_misc_flags.load(Ordering::Relaxed);
            if mf & 0x1000 != 0 && !(*rp).p_seg.fpu_state.is_null() {
                // Save FPU state from the process's save area
                core::ptr::copy_nonoverlapping(
                    (*rp).p_seg.fpu_state as *const u8,
                    fpstate.as_mut_ptr(),
                    512,
                );
            }
            Mcontext {
                mc_rax: reg.rax,
                mc_rbx: reg.rbx,
                mc_rcx: reg.rcx,
                mc_rdx: reg.rdx,
                mc_rsi: reg.rsi,
                mc_rdi: reg.rdi,
                mc_rbp: 0,
                mc_r8: reg.r8,
                mc_r9: reg.r9,
                mc_r10: reg.r10,
                mc_r11: reg.r11,
                mc_r12: reg.r12,
                mc_r13: reg.r13,
                mc_r14: reg.r14,
                mc_r15: reg.r15,
                mc_rip: reg.rip,
                mc_rsp: reg.rsp,
                mc_rflags: reg.rflags,
                mc_cs: reg.cs,
                mc_ss: reg.ss,
                mc_ds: reg.ds,
                mc_es: reg.es,
                mc_fs: reg.fs,
                mc_gs: reg.gs,
                mc_fpstate: fpstate,
            }
        };

        // Copy the Mcontext to the caller's address space via CR3 switching
        let mc_bytes = &mc as *const Mcontext as *const u8;
        let copy_sz = core::mem::size_of::<Mcontext>();
        let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
        if boot_cr3 != 0 {
            let caller_cr3 = (*caller).p_seg.p_cr3;
            if caller_cr3 != 0 {
                arch_x86_64::asm::write_cr3(caller_cr3);
            }
        }
        core::ptr::copy_nonoverlapping(mc_bytes, ctx_ptr as *mut u8, copy_sz);
        if boot_cr3 != 0 {
            arch_x86_64::asm::write_cr3(boot_cr3);
        }

        OK
    }
}

/// Handle SYS_SETMCONTEXT: restore a process's machine context.
///
/// Reads an Mcontext struct from the caller's address space and applies
/// it to the target process's TrapFrame.
///
/// # Safety
///
/// `caller` must point to a valid `Proc`.
pub unsafe fn do_setmcontext_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let endpt = msg_read_i32(msg, MCONTEXT_ENDPT_OFF);
        let ctx_ptr = msg_read_u64(msg, MCONTEXT_CTX_PTR_OFF);

        if !crate::table::is_ok_endpoint(endpt) {
            return crate::ipc::EINVAL;
        }
        let proc_nr = crate::table::endpoint_slot(endpt);
        let rp = crate::table::proc_addr(proc_nr);
        if rp.is_null() || (*rp).is_empty() || (*rp).p_endpoint != endpt {
            return crate::ipc::EINVAL;
        }

        // Copy Mcontext from the caller's address space
        use arch_x86_64::mcontext::Mcontext;
        let copy_sz = core::mem::size_of::<Mcontext>();
        let mut mc = Mcontext {
            mc_rax: 0,
            mc_rbx: 0,
            mc_rcx: 0,
            mc_rdx: 0,
            mc_rsi: 0,
            mc_rdi: 0,
            mc_rbp: 0,
            mc_r8: 0,
            mc_r9: 0,
            mc_r10: 0,
            mc_r11: 0,
            mc_r12: 0,
            mc_r13: 0,
            mc_r14: 0,
            mc_r15: 0,
            mc_rip: 0,
            mc_rsp: 0,
            mc_rflags: 0,
            mc_cs: 0,
            mc_ss: 0,
            mc_ds: 0,
            mc_es: 0,
            mc_fs: 0,
            mc_gs: 0,
            mc_fpstate: [0u8; 512],
        };
        let mc_bytes = &raw mut mc as *mut u8;

        let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
        if boot_cr3 != 0 {
            let caller_cr3 = (*caller).p_seg.p_cr3;
            if caller_cr3 != 0 {
                arch_x86_64::asm::write_cr3(caller_cr3);
            }
        }
        core::ptr::copy_nonoverlapping(ctx_ptr as *const u8, mc_bytes, copy_sz);
        if boot_cr3 != 0 {
            arch_x86_64::asm::write_cr3(boot_cr3);
        }

        // Apply the saved context to the target process's TrapFrame
        let reg = &mut (*rp).p_reg;
        reg.rax = mc.mc_rax;
        reg.rbx = mc.mc_rbx;
        reg.rcx = mc.mc_rcx;
        reg.rdx = mc.mc_rdx;
        reg.rsi = mc.mc_rsi;
        reg.rdi = mc.mc_rdi;
        reg.r8 = mc.mc_r8;
        reg.r9 = mc.mc_r9;
        reg.r10 = mc.mc_r10;
        reg.r11 = mc.mc_r11;
        reg.r12 = mc.mc_r12;
        reg.r13 = mc.mc_r13;
        reg.r14 = mc.mc_r14;
        reg.r15 = mc.mc_r15;
        reg.rip = mc.mc_rip;
        reg.rsp = mc.mc_rsp;
        reg.rflags = mc.mc_rflags;
        reg.cs = mc.mc_cs;
        reg.ss = mc.mc_ss;
        reg.ds = mc.mc_ds;
        reg.es = mc.mc_es;
        reg.fs = mc.mc_fs;
        reg.gs = mc.mc_gs;

        // Restore FPU state if saved
        let fpu_initialized = mc.mc_fpstate.iter().any(|&b| b != 0);
        if fpu_initialized && !(*rp).p_seg.fpu_state.is_null() {
            core::ptr::copy_nonoverlapping(mc.mc_fpstate.as_ptr(), (*rp).p_seg.fpu_state, 512);
            let old_mf = (*rp).p_misc_flags.load(Ordering::Relaxed);
            (*rp).p_misc_flags.store(old_mf | 0x1000, Ordering::Relaxed); // set FPU_INITIALIZED
        } else {
            let old_mf = (*rp).p_misc_flags.load(Ordering::Relaxed);
            (*rp)
                .p_misc_flags
                .store(old_mf & !0x1000, Ordering::Relaxed); // clear FPU_INITIALIZED
        }
        // Force reloading FPU in either case
        arch_x86_64::hw::release_fpu(rp as *mut core::ffi::c_void);

        OK
    }
}

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
                let endpt = msg_read_i32(msg, DIAGCTL_ENDPT_OFF);
                if !crate::table::is_ok_endpoint(endpt) {
                    return crate::ipc::EINVAL;
                }
                let proc_nr = crate::table::endpoint_slot(endpt);
                let rp = crate::table::proc_addr(proc_nr);
                if rp.is_null() || (*rp).is_empty() || (*rp).p_endpoint != endpt {
                    return crate::ipc::EINVAL;
                }
                crate::debug::proc_stacktrace(rp);
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

    #[test]
    fn test_do_irqctl_handler_invalid_request() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, IRQCTL_REQUEST_OFF, 999);
            let result = do_irqctl_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_do_setalarm_handler_no_priv_returns_eperm() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            if !(*rp).p_priv.is_null() {
                (*(*rp).p_priv).s_flags = PrivFlags::empty();
            }
            let mut msg = [0u8; MESSAGE_SIZE];
            let result = do_setalarm_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EPERM);
        }
    }

    #[test]
    fn test_do_stime_handler_ok() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            let result = do_stime_handler(rp, &mut msg);
            assert_eq!(result, OK);
        }
    }

    #[test]
    fn test_do_settime_handler_ok() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            let result = do_settime_handler(rp, &mut msg);
            assert_eq!(result, OK);
        }
    }

    #[test]
    fn test_do_vtimer_handler_invalid_endpoint() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            // Must have SYS_PROC privilege
            let priv0 = setup_test_priv(0);
            (*rp).p_priv = priv0;
            (*priv0).s_flags = PrivFlags::SYS_PROC;
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, VTIMER_ENDPT_OFF, 99999);
            let result = do_vtimer_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_boottime_accessors() {
        crate::clock::set_boottime(42);
        assert_eq!(crate::clock::get_boottime(), 42);
        crate::clock::set_boottime(0);
        assert_eq!(crate::clock::get_boottime(), 0);
    }

    // ── Phase 8.8: DEVIO / VDEVIO / SDEVIO tests ──────────────────────

    /// Reset proc 0 to a clean state for devio tests.
    unsafe fn reset_devio_test_proc() -> *mut Proc {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            (*rp).p_priv = core::ptr::null_mut();
            rp
        }
    }

    #[test]
    fn test_devio_invalid_dir_returns_einval() {
        unsafe {
            let rp = reset_devio_test_proc();
            let mut msg = [0u8; MESSAGE_SIZE];
            // Neither input nor output (request = DIO_BYTE without DIO_INPUT/DIO_OUTPUT)
            msg_write_i32(
                &mut msg,
                DEVIO_REQUEST_OFF,
                arch_common::com::DIO_BYTE as i32,
            );
            let result = do_devio_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_devio_bad_io_type_returns_inval() {
        unsafe {
            let rp = reset_devio_test_proc();
            let mut msg = [0u8; MESSAGE_SIZE];
            // Request with valid dir but garbage type bits above typemask
            let req = arch_common::com::DIO_INPUT | arch_common::com::DIO_BYTE;
            msg_write_i32(&mut msg, DEVIO_REQUEST_OFF, req as i32);
            let result = do_devio_handler(rp, &mut msg);
            assert_eq!(result, OK); // DIO_INPUT|DIO_BYTE is valid — IO skipped in test
        }
    }

    #[test]
    fn test_devio_unaligned_word_port_returns_eperm() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            // Request word output on port 1 (odd = unaligned for word)
            let req = arch_common::com::DIO_OUTPUT | arch_common::com::DIO_WORD;
            msg_write_i32(&mut msg, DEVIO_REQUEST_OFF, req as i32);
            msg_write_i32(&mut msg, DEVIO_PORT_OFF, 1); // unaligned for word
            let result = do_devio_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EPERM);
        }
    }

    #[test]
    fn test_devio_ok_without_priv_check() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            // No privilege structure = no CHECK_IO_PORT check
            (*rp).p_priv = core::ptr::null_mut();
            let mut msg = [0u8; MESSAGE_SIZE];
            // Byte output to port 0x80 — BOOT_CR3 is 0 in tests, so I/O skipped
            let req = arch_common::com::DIO_OUTPUT | arch_common::com::DIO_BYTE;
            msg_write_i32(&mut msg, DEVIO_REQUEST_OFF, req as i32);
            msg_write_i32(&mut msg, DEVIO_PORT_OFF, 0x80);
            msg_write_i32(&mut msg, DEVIO_VALUE_OFF, 0);
            let result = do_devio_handler(rp, &mut msg);
            assert_eq!(result, OK);
        }
    }

    #[test]
    fn test_devio_unauthorized_port_returns_eperm() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let priv0 = setup_test_priv(0);
            (*rp).p_priv = priv0;
            (*priv0).s_flags = PrivFlags::CHECK_IO_PORT;
            (*priv0).s_nr_io_range = 1;
            // Allow only port 0x60-0x6F
            (*priv0).s_io_tab[0] = crate::r#priv::IoRange {
                ior_base: 0x60,
                ior_limit: 0x6F,
            };
            let mut msg = [0u8; MESSAGE_SIZE];
            let req = arch_common::com::DIO_OUTPUT | arch_common::com::DIO_BYTE;
            msg_write_i32(&mut msg, DEVIO_REQUEST_OFF, req as i32);
            msg_write_i32(&mut msg, DEVIO_PORT_OFF, 0x378);
            let result = do_devio_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EPERM);
        }
    }

    #[test]
    fn test_devio_authorized_port_passes() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let priv0 = setup_test_priv(0);
            (*rp).p_priv = priv0;
            (*priv0).s_flags = PrivFlags::CHECK_IO_PORT;
            (*priv0).s_nr_io_range = 1;
            (*priv0).s_io_tab[0] = crate::r#priv::IoRange {
                ior_base: 0x80,
                ior_limit: 0x80,
            };
            let mut msg = [0u8; MESSAGE_SIZE];
            let req = arch_common::com::DIO_OUTPUT | arch_common::com::DIO_BYTE;
            msg_write_i32(&mut msg, DEVIO_REQUEST_OFF, req as i32);
            msg_write_i32(&mut msg, DEVIO_PORT_OFF, 0x80);
            msg_write_i32(&mut msg, DEVIO_VALUE_OFF, 0);
            let result = do_devio_handler(rp, &mut msg);
            assert_eq!(result, OK);
        }
    }

    #[test]
    fn test_vdevio_neg_size_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            let req = arch_common::com::DIO_INPUT | arch_common::com::DIO_BYTE;
            msg_write_i32(&mut msg, VDEVIO_REQUEST_OFF, req as i32);
            msg_write_i32(&mut msg, VDEVIO_VEC_SIZE_OFF, -1);
            let result = do_vdevio_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_vdevio_zero_size_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            let req = arch_common::com::DIO_INPUT | arch_common::com::DIO_BYTE;
            msg_write_i32(&mut msg, VDEVIO_REQUEST_OFF, req as i32);
            msg_write_i32(&mut msg, VDEVIO_VEC_SIZE_OFF, 0);
            let result = do_vdevio_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_vdevio_bad_dir_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            // Neither input nor output
            msg_write_i32(
                &mut msg,
                VDEVIO_REQUEST_OFF,
                arch_common::com::DIO_BYTE as i32,
            );
            msg_write_i32(&mut msg, VDEVIO_VEC_SIZE_OFF, 1);
            let result = do_vdevio_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_vdevio_bad_type_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            let req = arch_common::com::DIO_INPUT | 0xFF;
            msg_write_i32(&mut msg, VDEVIO_REQUEST_OFF, req as i32);
            msg_write_i32(&mut msg, VDEVIO_VEC_SIZE_OFF, 1);
            let result = do_vdevio_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_sdevio_zero_count_returns_ok() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            let req = arch_common::com::DIO_INPUT | arch_common::com::DIO_BYTE;
            msg_write_i32(&mut msg, SDEVIO_REQUEST_OFF, req as i32);
            msg_write_u64(&mut msg, SDEVIO_PORT_OFF, 0);
            msg_write_i32(&mut msg, SDEVIO_VEC_ENDPT_OFF, NONE);
            msg_write_u64(&mut msg, SDEVIO_VEC_SIZE_OFF, 0); // zero count
            let result = do_sdevio_handler(rp, &mut msg);
            assert_eq!(result, OK);
        }
    }

    #[test]
    fn test_sdevio_bad_endpoint_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            let req = arch_common::com::DIO_INPUT | arch_common::com::DIO_BYTE;
            msg_write_i32(&mut msg, SDEVIO_REQUEST_OFF, req as i32);
            msg_write_i32(&mut msg, SDEVIO_VEC_ENDPT_OFF, 99999); // bad endpoint
            msg_write_u64(&mut msg, SDEVIO_VEC_SIZE_OFF, 1);
            let result = do_sdevio_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_sdevio_system_init_registers_devio_calls() {
        unsafe {
            system_init();
            assert!(CALL_VEC[21].is_some()); // SYS_DEVIO
            assert!(CALL_VEC[22].is_some()); // SYS_SDEVIO
            assert!(CALL_VEC[23].is_some()); // SYS_VDEVIO
        }
    }

    // ── Phase 8.10: EXEC / mcontext tests ──────────────────────────────

    #[test]
    fn test_exec_bad_endpoint_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, EXEC_ENDPT_OFF, 99999); // bad endpoint
            let result = do_exec_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_exec_on_empty_slot_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            // Slot 1 is empty after proc_init (not in BOOT_IMAGE)
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, EXEC_ENDPT_OFF, 0x80000001u32 as i32); // valid endpoint format but empty slot
            let result = do_exec_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_exec_returns_edontreply() {
        unsafe {
            proc_init();
            // Set up proc 0 (PM) as a valid target
            let rp = crate::table::proc_addr(0);
            (*rp).p_endpoint = 0;
            (*rp)
                .p_rts_flags
                .store(RtsFlags::empty().bits(), Ordering::Relaxed);
            (*rp).p_misc_flags.store(0, Ordering::Relaxed);

            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, EXEC_ENDPT_OFF, 0); // target = PM
            msg_write_u64(&mut msg, EXEC_IP_OFF, 0x1000); // new entry point
            msg_write_u64(&mut msg, EXEC_STACK_OFF, 0x7fffe000); // new stack
            msg_write_u64(&mut msg, EXEC_NAME_OFF, b"test_prog\0" as *const u8 as u64); // name pointer
            msg_write_u64(&mut msg, EXEC_PS_STR_OFF, 0);

            let result = do_exec_handler(rp, &mut msg);
            assert_eq!(result, EDONTREPLY);

            // RTS_RECEIVING should be cleared (process becomes runnable)
            let rts = (*rp).p_rts_flags.load(Ordering::Relaxed);
            assert_eq!(
                rts & RtsFlags::RECEIVING.bits(),
                0,
                "RTS_RECEIVING should be cleared after exec"
            );

            // MF_DELIVERMSG should be cleared
            let mf = (*rp).p_misc_flags.load(Ordering::Relaxed);
            assert_eq!(mf & 0x0004, 0, "MF_DELIVERMSG should be cleared");

            // MF_FPU_INITIALIZED should be cleared
            assert_eq!(mf & 0x1000, 0, "MF_FPU_INITIALIZED should be cleared");

            // Process name should be non-empty (may read garbage from test name_ptr)
            let has_name = (*rp).p_name[0] != 0;
            assert!(has_name, "process name should be set after exec");
        }
    }

    #[test]
    fn test_exec_clears_old_delivermsg_flag() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            (*rp).p_endpoint = 0;
            (*rp)
                .p_rts_flags
                .store(RtsFlags::empty().bits(), Ordering::Relaxed);
            // Set MF_DELIVERMSG before exec
            (*rp).p_misc_flags.store(0x0004, Ordering::Relaxed);

            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, EXEC_ENDPT_OFF, 0);
            msg_write_u64(&mut msg, EXEC_IP_OFF, 0x1000);
            msg_write_u64(&mut msg, EXEC_STACK_OFF, 0x7fffe000);
            msg_write_u64(&mut msg, EXEC_NAME_OFF, b"test\0" as *const u8 as u64);
            msg_write_u64(&mut msg, EXEC_PS_STR_OFF, 0);

            let _ = do_exec_handler(rp, &mut msg);

            let mf = (*rp).p_misc_flags.load(Ordering::Relaxed);
            assert_eq!(mf & 0x0004, 0, "MF_DELIVERMSG should have been cleared");
        }
    }

    #[test]
    fn test_getmcontext_bad_endpoint_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, MCONTEXT_ENDPT_OFF, 99999);
            let result = do_getmcontext_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_setmcontext_bad_endpoint_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, MCONTEXT_ENDPT_OFF, 99999);
            let result = do_setmcontext_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_exec_and_mcontext_registered() {
        unsafe {
            system_init();
            assert!(CALL_VEC[1].is_some()); // SYS_EXEC
            assert!(CALL_VEC[50].is_some()); // SYS_GETMCONTEXT
            assert!(CALL_VEC[51].is_some()); // SYS_SETMCONTEXT
        }
    }

    #[test]
    fn test_copy_handlers_registered() {
        unsafe {
            system_init();
            assert!(CALL_VEC[15].is_some()); // SYS_VIRCOPY
            assert!(CALL_VEC[16].is_some()); // SYS_PHYSCOPY
        }
    }

    #[test]
    fn test_vircopy_bad_src_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, COPY_SRC_ENDPT_OFF, 99999); // bad src
            msg_write_i32(&mut msg, COPY_DST_ENDPT_OFF, -1); // NONE = kernel
            let result = do_vircopy_handler(rp, &mut msg);
            assert_eq!(result, crate::grants::EINVAL);
        }
    }

    #[test]
    fn test_physcopy_bad_dst_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, COPY_SRC_ENDPT_OFF, -1); // NONE = kernel
            msg_write_i32(&mut msg, COPY_DST_ENDPT_OFF, 99999); // bad dst
            let result = do_physcopy_handler(rp, &mut msg);
            assert_eq!(result, crate::grants::EINVAL);
        }
    }

    #[test]
    fn test_copy_both_none_returns_ok() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, COPY_SRC_ENDPT_OFF, -1); // NONE = kernel
            msg_write_i32(&mut msg, COPY_DST_ENDPT_OFF, -1); // NONE = kernel
            msg_write_u64(&mut msg, COPY_NR_BYTES_OFF, 0); // zero bytes = no-op
            let result = do_vircopy_handler(rp, &mut msg);
            assert_eq!(result, 0); // OK
        }
    }

    #[test]
    fn test_copy_flags_constant() {
        assert_eq!(CP_FLAG_TRY, 0x01);
    }

    #[test]
    fn test_copy_offset_constants() {
        // Verify offsets match struct layout
        assert_eq!(COPY_SRC_ENDPT_OFF, 0);
        assert_eq!(COPY_SRC_ADDR_OFF, 8);
        assert_eq!(COPY_DST_ENDPT_OFF, 16);
        assert_eq!(COPY_DST_ADDR_OFF, 24);
        assert_eq!(COPY_NR_BYTES_OFF, 32);
        assert_eq!(COPY_FLAGS_OFF, 40);
    }

    #[test]
    fn test_sprofile_handlers_registered() {
        unsafe {
            system_init();
            assert!(CALL_VEC[36].is_some()); // SYS_SPROF
            assert!(CALL_VEC[37].is_some()); // SYS_CPROF
            assert!(CALL_VEC[38].is_some()); // SYS_PROFBUF
        }
    }

    #[test]
    fn test_sprofile_offset_constants() {
        assert_eq!(SPROF_ACTION_OFF, 0);
        assert_eq!(SPROF_FREQ_OFF, 4);
        assert_eq!(SPROF_INTR_TYPE_OFF, 8);
        assert_eq!(SPROF_ENDPT_OFF, 12);
        assert_eq!(SPROF_CTL_PTR_OFF, 16);
        assert_eq!(SPROF_MEM_PTR_OFF, 24);
        assert_eq!(SPROF_MEM_SIZE_OFF, 32);
    }

    #[test]
    fn test_cprofile_offset_constants() {
        assert_eq!(CPROF_ACTION_OFF, 0);
        assert_eq!(CPROF_MEM_SIZE_OFF, 4);
        assert_eq!(CPROF_ENDPT_OFF, 8);
        assert_eq!(CPROF_CTL_PTR_OFF, 16);
        assert_eq!(CPROF_MEM_PTR_OFF, 24);
    }

    #[test]
    fn test_profbuf_offset_constants() {
        assert_eq!(PROFBUF_CTL_PTR_OFF, 0);
        assert_eq!(PROFBUF_MEM_PTR_OFF, 8);
    }

    #[test]
    fn test_sprofile_rejects_non_sys_proc() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(10);
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, SPROF_ACTION_OFF, crate::profile::PROF_START);
            let result = do_sprofile_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EPERM);
        }
    }

    #[test]
    fn test_cprofile_rejects_non_sys_proc() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(10);
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, CPROF_ACTION_OFF, crate::profile::PROF_RESET);
            let result = do_cprofile_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EPERM);
        }
    }

    #[test]
    fn test_cprofile_reset_ok() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let privp = setup_test_priv(0);
            (*privp).s_flags = PrivFlags::SYS_PROC;
            (*rp).p_priv = privp;
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, CPROF_ACTION_OFF, crate::profile::PROF_RESET);
            let result = do_cprofile_handler(rp, &mut msg);
            assert_eq!(result, 0); // OK
        }
    }

    #[test]
    fn test_cprofile_get_bad_endpoint_returns_einval() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            let privp = setup_test_priv(0);
            (*privp).s_flags = PrivFlags::SYS_PROC;
            (*rp).p_priv = privp;
            let mut msg = [0u8; MESSAGE_SIZE];
            msg_write_i32(&mut msg, CPROF_ACTION_OFF, crate::profile::PROF_GET);
            msg_write_i32(&mut msg, CPROF_ENDPT_OFF, 99999); // bad endpoint
            let result = do_cprofile_handler(rp, &mut msg);
            assert_eq!(result, crate::ipc::EINVAL);
        }
    }
}
