//! RS server — Reincarnation Server: service lifecycle management.
//!
//! Ported from `.refs/minix-3.3.0/minix/servers/rs/`
//!
//! The RS server manages system services: startup, shutdown, restart,
//! crash recovery, live update, and clone/replica management.  It is
//! the central authority for the system's process lifecycle.
//!
//! # Service lifecycle
//!
//! ```text
//!   do_up → alloc_slot → init_slot → start_service → run_service
//!              ↓                            ↓
//!         lookup_slot_by_label       sched_init_proc
//!              ↓                            ↓
//!         publish_service            sys_exec / fork
//! ```
//!
//! The IPC message loop is deferred (Phase 12 — SEF/server framework).
//! All service table management and lookup functions are fully implemented.

#![allow(dead_code, clippy::missing_safety_doc)]

use arch_common::ipc::Message;

// Constants

/// Number of system process slots.
pub const NR_SYS_PROCS: usize = 32;

/// Number of boot process entries.
pub const NR_BOOT_PROCS: usize = 16;

/// Maximum label length.
pub const RS_MAX_LABEL_LEN: usize = 64;

/// Maximum command line length.
pub const MAX_COMMAND_LEN: usize = 512;

/// Maximum number of arguments.
pub const MAX_NR_ARGS: usize = 10;

/// Maximum IPC list size.
pub const MAX_IPC_LIST: usize = 256;

/// Maximum control entries.
pub const RS_NR_CONTROL: usize = 8;

/// Default heartbeat period in ticks.
pub const RS_INIT_T: u32 = 100; // system_hz * 10
pub const RS_DELTA_T: u32 = 10; // system_hz

pub const RS_IN_USE: u32 = 0x001;
pub const RS_EXITING: u32 = 0x002;
pub const RS_REFRESHING: u32 = 0x004;
pub const RS_NOPINGREPLY: u32 = 0x008;
pub const RS_TERMINATED: u32 = 0x010;
pub const RS_LATEREPLY: u32 = 0x020;
pub const RS_INITIALIZING: u32 = 0x040;
pub const RS_UPDATING: u32 = 0x080;
pub const RS_ACTIVE: u32 = 0x100;
pub const RS_REINCARNATE: u32 = 0x200;

pub const SF_CORE_SRV: u32 = 0x001;
pub const SF_SYNCH_BOOT: u32 = 0x002;
pub const SF_NEED_COPY: u32 = 0x004;
pub const SF_USE_COPY: u32 = 0x008;
pub const SF_NEED_REPL: u32 = 0x010;
pub const SF_USE_REPL: u32 = 0x020;
pub const SF_NO_BIN_EXP: u32 = 0x040;

/// Immutable sys flags.
pub const IMM_SF: u32 = SF_NO_BIN_EXP | SF_CORE_SRV | SF_SYNCH_BOOT | SF_NEED_COPY | SF_NEED_REPL;

pub const SRV_SF: u32 = SF_CORE_SRV;
pub const SRVR_SF: u32 = SRV_SF | SF_NEED_REPL;
pub const DSRV_SF: u32 = 0;
pub const VM_SF: u32 = SRVR_SF;

const OK: i32 = 0;
const EPERM: i32 = -1;
const ENOMEM: i32 = -12;
const EBUSY: i32 = -16;
const EINVAL: i32 = -22;
const ENOSYS: i32 = -71;

// RS call numbers
pub const RS_UP: i32 = 0x700;
pub const RS_DOWN: i32 = 0x701;
pub const RS_REFRESH: i32 = 0x702;
pub const RS_RESTART: i32 = 0x703;
pub const RS_SHUTDOWN: i32 = 0x704;
pub const RS_UPDATE: i32 = 0x705;
pub const RS_CLONE: i32 = 0x706;
pub const RS_EDIT: i32 = 0x707;
pub const RS_GETSYSINFO: i32 = 0x708;
pub const RS_LOOKUP: i32 = 0x709;
pub const RS_INIT: i32 = 0x70A;
pub const RS_LU_PREPARE: i32 = 0x70B;

const ESRCH: i32 = -3;
const EEXIST: i32 = -17;
const EDONTREPLY: i32 = -201;

// Types

/// A boot image entry.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct BootImage {
    pub endpoint: i32,
    pub flags: u32,
}

/// A boot image privilege entry.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct BootImagePriv {
    pub endpoint: i32,
    pub label: [u8; RS_MAX_LABEL_LEN],
    pub flags: i32,
}

/// A boot image system entry.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct BootImageSys {
    pub endpoint: i32,
    pub flags: i32,
}

/// A boot image device entry.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct BootImageDev {
    pub endpoint: i32,
    pub dev_nr: u32,
}

/// Public process record — published to DS.
#[derive(Debug, Clone)]
#[repr(C)]
pub struct RprocPub {
    pub in_use: bool,
    pub endpoint: i32,
    pub dev_nr: i32,
    pub label: [u8; RS_MAX_LABEL_LEN],
    pub proc_name: [u8; RS_MAX_LABEL_LEN],
}

impl RprocPub {
    const fn zeroed() -> Self {
        Self {
            in_use: false,
            endpoint: 0,
            dev_nr: -1, // NO_DEV
            label: [0u8; RS_MAX_LABEL_LEN],
            proc_name: [0u8; RS_MAX_LABEL_LEN],
        }
    }
}

impl Default for RprocPub {
    fn default() -> Self {
        Self {
            in_use: false,
            endpoint: 0,
            dev_nr: -1,
            label: [0u8; RS_MAX_LABEL_LEN],
            proc_name: [0u8; RS_MAX_LABEL_LEN],
        }
    }
}

/// Process record — the main RS process table entry.
#[derive(Debug, Clone)]
#[repr(C)]
pub struct Rproc {
    pub pub_idx: usize,
    pub pid: i32,
    pub restarts: i32,
    pub backoff: i64,
    pub flags: u32,
    pub period: i64,
    pub check_tm: u64,
    pub alive_tm: u64,
    pub stop_tm: u64,
    pub scheduler: i32,
    pub priority: i32,
    pub quantum: i32,
    pub cpu: i32,
    pub cmd: [u8; MAX_COMMAND_LEN],
    pub label: [u8; RS_MAX_LABEL_LEN],
}

impl Default for Rproc {
    fn default() -> Self {
        Self {
            pub_idx: 0,
            pid: -1,
            restarts: 0,
            backoff: 0,
            flags: 0,
            period: 0,
            check_tm: 0,
            alive_tm: 0,
            stop_tm: 0,
            scheduler: 0,
            priority: 0,
            quantum: 0,
            cpu: 0,
            cmd: [0u8; MAX_COMMAND_LEN],
            label: [0u8; RS_MAX_LABEL_LEN],
        }
    }
}

impl Rproc {
    const fn zeroed() -> Self {
        Self {
            pub_idx: 0,
            pid: -1,
            restarts: 0,
            backoff: 0,
            flags: 0,
            period: 0,
            check_tm: 0,
            alive_tm: 0,
            stop_tm: 0,
            scheduler: 0,
            priority: 0,
            quantum: 0,
            cpu: 0,
            cmd: [0u8; MAX_COMMAND_LEN],
            label: [0u8; RS_MAX_LABEL_LEN],
        }
    }
}

/// Global update descriptor.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Rupdate {
    pub flags: i32,
    pub prepare_tm: u64,
    pub prepare_maxtime: u64,
    pub rp_idx: i32,
}

// Static tables

use core::cell::UnsafeCell;

struct RprocTableRaw(UnsafeCell<[Rproc; NR_SYS_PROCS]>);
unsafe impl Sync for RprocTableRaw {}
impl RprocTableRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { Rproc::zeroed() }; NR_SYS_PROCS]))
    }
    fn as_ptr(&self) -> *mut Rproc {
        self.0.get() as *mut Rproc
    }
}

struct RprocPubTableRaw(UnsafeCell<[RprocPub; NR_SYS_PROCS]>);
unsafe impl Sync for RprocPubTableRaw {}
impl RprocPubTableRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [const { RprocPub::zeroed() }; NR_SYS_PROCS],
        ))
    }
    fn as_ptr(&self) -> *mut RprocPub {
        self.0.get() as *mut RprocPub
    }
}

static RPROC: RprocTableRaw = RprocTableRaw::new();
static RPROCPUB: RprocPubTableRaw = RprocPubTableRaw::new();

// ---- Slot management ----

/// Allocate a free slot in the system process table.
pub unsafe fn alloc_slot() -> Option<usize> {
    let base = RPROC.as_ptr();
    for i in 0..NR_SYS_PROCS {
        if unsafe { (*base.add(i)).flags & RS_IN_USE == 0 } {
            unsafe {
                (*base.add(i)).flags = RS_IN_USE;
            }
            return Some(i);
        }
    }
    None
}

/// Free a slot in the system process table.
pub unsafe fn free_slot(idx: usize) {
    if idx >= NR_SYS_PROCS {
        return;
    }
    let base = RPROC.as_ptr();
    unsafe {
        (*base.add(idx)).flags = 0;
    }
    let pub_base = RPROCPUB.as_ptr();
    unsafe {
        (*pub_base.add(idx)).in_use = false;
    }
}

/// Look up a slot by label.
pub unsafe fn lookup_slot_by_label(label: &[u8]) -> Option<usize> {
    let base = RPROC.as_ptr();
    for i in 0..NR_SYS_PROCS {
        let rp = unsafe { &*base.add(i) };
        if rp.flags & RS_IN_USE == 0 {
            continue;
        }
        let label_len = label.iter().position(|&c| c == 0).unwrap_or(label.len());
        let rp_label = &rp.label;
        let rp_len = rp_label
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(rp_label.len());
        if label_len == rp_len && rp_label[..rp_len] == label[..label_len] {
            return Some(i);
        }
    }
    None
}

/// Look up a slot by PID.
pub unsafe fn lookup_slot_by_pid(pid: i32) -> Option<usize> {
    let base = RPROC.as_ptr();
    for i in 0..NR_SYS_PROCS {
        let rp = unsafe { &*base.add(i) };
        if rp.flags & RS_IN_USE == 0 {
            continue;
        }
        if rp.pid == pid {
            return Some(i);
        }
    }
    None
}

/// Look up a slot by endpoint.
pub unsafe fn lookup_slot_by_endpoint(endpoint: i32) -> Option<usize> {
    let pub_base = RPROCPUB.as_ptr();
    for i in 0..NR_SYS_PROCS {
        let rpub = unsafe { &*pub_base.add(i) };
        if !rpub.in_use {
            continue;
        }
        if rpub.endpoint == endpoint {
            return Some(i);
        }
    }
    None
}

// Initialization

/// Reset the system process table.
pub unsafe fn rs_init() {
    let base = RPROC.as_ptr();
    for i in 0..NR_SYS_PROCS {
        unsafe {
            *base.add(i) = Rproc::zeroed();
        }
    }
    let pub_base = RPROCPUB.as_ptr();
    for i in 0..NR_SYS_PROCS {
        unsafe {
            (*pub_base.add(i)).in_use = false;
        }
    }
}

/// Initialize a slot with the given label and endpoint.
pub unsafe fn init_slot(idx: usize, endpoint: i32, dev_nr: i32, label: &[u8]) -> Result<(), i32> {
    if idx >= NR_SYS_PROCS {
        return Err(EINVAL);
    }
    let base = RPROC.as_ptr();
    let rp = unsafe { &mut *base.add(idx) };
    rp.flags = RS_IN_USE | RS_INITIALIZING;
    rp.pid = -1;

    let label_len = label.len().min(RS_MAX_LABEL_LEN - 1);
    rp.label[..label_len].copy_from_slice(&label[..label_len]);
    rp.label[label_len] = 0;

    let pub_base = RPROCPUB.as_ptr();
    let rpub = unsafe { &mut *pub_base.add(idx) };
    rpub.in_use = true;
    rpub.endpoint = endpoint;
    rpub.dev_nr = dev_nr;
    rpub.label[..label_len].copy_from_slice(&label[..label_len]);
    rpub.label[label_len] = 0;
    rpub.proc_name[..label_len].copy_from_slice(&label[..label_len]);
    rpub.proc_name[label_len] = 0;

    Ok(())
}

/// Mark a service as initialized (ready).
pub unsafe fn mark_initialized(idx: usize, endpoint: i32) -> Result<(), i32> {
    if idx >= NR_SYS_PROCS {
        return Err(EINVAL);
    }
    let base = RPROC.as_ptr();
    let rp = unsafe { &mut *base.add(idx) };
    if rp.flags & RS_IN_USE == 0 {
        return Err(EINVAL);
    }
    rp.flags &= !RS_INITIALIZING;
    rp.flags |= RS_ACTIVE;
    rp.alive_tm = 0;

    // Update public entry.
    let pub_base = RPROCPUB.as_ptr();
    let rpub = unsafe { &mut *pub_base.add(idx) };
    rpub.endpoint = endpoint;

    Ok(())
}

/// Mark a service as terminated.
pub unsafe fn mark_terminated(idx: usize) {
    if idx >= NR_SYS_PROCS {
        return;
    }
    let base = RPROC.as_ptr();
    let rp = unsafe { &mut *base.add(idx) };
    rp.flags |= RS_TERMINATED;
    rp.flags &= !RS_ACTIVE;
}

/// Check if a process endpoint is valid for RS.
pub unsafe fn rs_isokendpt(endpoint: i32) -> Option<usize> {
    if endpoint < 0 {
        return None;
    }
    let pub_base = RPROCPUB.as_ptr();
    for i in 0..NR_SYS_PROCS {
        let rpub = unsafe { &*pub_base.add(i) };
        if rpub.in_use && rpub.endpoint == endpoint {
            return Some(i);
        }
    }
    None
}

/// Check if the caller is allowed to perform a request on a target service.
pub fn check_call_permission(caller: i32, _target_idx: Option<usize>) -> bool {
    // For now, allow all calls from PM and RS itself.
    // Real implementation checks caller's isolation policy.
    matches!(caller, -3 | -4 | -7) // PM_PROC_NR, RS_PROC_NR, SCHED_PROC_NR
}

/// Return the label for a given slot.
pub unsafe fn slot_label(idx: usize) -> Option<[u8; RS_MAX_LABEL_LEN]> {
    if idx >= NR_SYS_PROCS {
        return None;
    }
    let base = RPROC.as_ptr();
    let rp = unsafe { &*base.add(idx) };
    if rp.flags & RS_IN_USE == 0 {
        return None;
    }
    Some(rp.label)
}

/// Return the endpoint for a given slot.
pub unsafe fn slot_endpoint(idx: usize) -> Option<i32> {
    if idx >= NR_SYS_PROCS {
        return None;
    }
    let pub_base = RPROCPUB.as_ptr();
    let rpub = unsafe { &*pub_base.add(idx) };
    if !rpub.in_use {
        return None;
    }
    Some(rpub.endpoint)
}

// ---- RS request handlers ----

/// Register a new service (RS_UP).
unsafe fn do_up(msg: &mut Message) -> i32 {
    let label_ptr = unsafe { msg.m_payload.m2.m2l1 } as u64;
    let label_len = unsafe { msg.m_payload.m2.m2i1 } as usize;
    let endpoint = unsafe { msg.m_payload.m2.m2i2 };

    let copy_len = label_len.min(RS_MAX_LABEL_LEN);
    let label_buf = [0u8; RS_MAX_LABEL_LEN];

    let r = minix_rt::sys_vircopy(
        msg.m_source,
        label_ptr,
        minix_rt::SELF,
        label_buf.as_ptr() as u64,
        copy_len,
    );
    if r != 0 {
        return r;
    }

    let slot = match unsafe { alloc_slot() } {
        Some(s) => s,
        None => return ENOMEM,
    };

    if let Err(e) = unsafe { init_slot(slot, endpoint, 0, &label_buf[..copy_len]) } {
        unsafe {
            free_slot(slot);
        }
        return e;
    }

    if let Err(e) = unsafe { mark_initialized(slot, endpoint) } {
        unsafe {
            free_slot(slot);
        }
        return e;
    }

    OK
}

/// Stop a service (RS_DOWN).
unsafe fn do_down(msg: &Message) -> i32 {
    let endpoint = unsafe { msg.m_payload.m2.m2i1 };
    match unsafe { lookup_slot_by_endpoint(endpoint) } {
        Some(slot) => {
            unsafe {
                mark_terminated(slot);
                free_slot(slot);
            }
            OK
        }
        None => ESRCH,
    }
}

/// Refresh/restart a service (RS_REFRESH).
unsafe fn do_refresh(msg: &Message) -> i32 {
    let endpoint = unsafe { msg.m_payload.m2.m2i1 };
    match unsafe { lookup_slot_by_endpoint(endpoint) } {
        Some(slot) => {
            let base = RPROC.as_ptr();
            unsafe {
                (*base.add(slot)).flags |= RS_REFRESHING;
                mark_terminated(slot);
                free_slot(slot);
            }
            OK
        }
        None => ESRCH,
    }
}

/// Restart a service (RS_RESTART).
unsafe fn do_restart(msg: &Message) -> i32 {
    let endpoint = unsafe { msg.m_payload.m2.m2i1 };
    match unsafe { lookup_slot_by_endpoint(endpoint) } {
        Some(slot) => {
            unsafe {
                mark_terminated(slot);
                free_slot(slot);
            }
            OK
        }
        None => ESRCH,
    }
}

/// Shutdown (RS_SHUTDOWN).
fn do_shutdown(_msg: &Message) -> i32 {
    OK
}

/// Live update (RS_UPDATE) — not yet implemented.
fn do_update(_msg: &Message) -> i32 {
    ENOSYS
}

/// Clone a service (RS_CLONE) — not yet implemented.
fn do_clone(_msg: &Message) -> i32 {
    ENOSYS
}

/// Edit a service (RS_EDIT) — not yet implemented.
fn do_edit(_msg: &Message) -> i32 {
    ENOSYS
}

/// Look up a service by label (RS_LOOKUP).
unsafe fn do_lookup(msg: &mut Message) -> i32 {
    let label_ptr = unsafe { msg.m_payload.m2.m2l1 } as u64;
    let label_len = unsafe { msg.m_payload.m2.m2i1 } as usize;

    let copy_len = label_len.min(RS_MAX_LABEL_LEN);
    let label_buf = [0u8; RS_MAX_LABEL_LEN];

    let r = minix_rt::sys_vircopy(
        msg.m_source,
        label_ptr,
        minix_rt::SELF,
        label_buf.as_ptr() as u64,
        copy_len,
    );
    if r != 0 {
        return r;
    }

    match unsafe { lookup_slot_by_label(&label_buf[..copy_len]) } {
        Some(slot) => {
            if let Some(ep) = unsafe { slot_endpoint(slot) } {
                msg.m_payload.m2.m2i1 = ep;
                OK
            } else {
                ESRCH
            }
        }
        None => ESRCH,
    }
}

/// Service reports initialization complete (RS_INIT).
unsafe fn do_init_ready(msg: &Message) -> i32 {
    let endpoint = msg.m_source;
    match unsafe { lookup_slot_by_endpoint(endpoint) } {
        Some(slot) => unsafe { mark_initialized(slot, endpoint) }.map_or_else(|e| e, |_| OK),
        None => ESRCH,
    }
}

/// Live update prepare (RS_LU_PREPARE) — not yet implemented.
fn do_upd_ready(_msg: &Message) -> i32 {
    ENOSYS
}

// Server main loop

/// RS server main loop.
///
/// Receives messages from clients and dispatches RS requests.
pub fn rs_server_main() {
    #[cfg(target_os = "minix")]
    {
        // Initialize RS's process table.
        unsafe {
            rs_init();
        }

        // Register boot services with their known endpoints.
        let boot_svcs: &[(i32, &[u8])] = &[
            (arch_common::com::DS_PROC_NR, b"ds"),
            (arch_common::com::RS_PROC_NR, b"rs"),
            (arch_common::com::PM_PROC_NR, b"pm"),
            (arch_common::com::SCHED_PROC_NR, b"sched"),
            (arch_common::com::VFS_PROC_NR, b"vfs"),
            (arch_common::com::VM_PROC_NR, b"vm"),
            (arch_common::com::TTY_PROC_NR, b"tty"),
            (arch_common::com::MFS_PROC_NR, b"mfs"),
            (arch_common::com::FB_PROC_NR, b"fb"),
            (arch_common::com::INPUT_PROC_NR, b"input"),
            (arch_common::com::WS_PROC_NR, b"wserver"),
        ];
        for &(ep, label) in boot_svcs {
            if let Some(slot) = unsafe { alloc_slot() } {
                let _ = unsafe { init_slot(slot, ep, 0, label) };
                unsafe {
                    let _ = mark_initialized(slot, ep);
                }
            }
        }

        // IPC syscall numbers.
        const RECEIVE_CALL: u64 = 47;
        const SENDREC_CALL: u64 = 48;
        const ANY: i32 = 0x0000ffff;

        loop {
            let mut msg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };

            // Receive from any sender.
            let src = unsafe {
                minix_rt::syscall2(RECEIVE_CALL, ANY as u64, &mut msg as *mut Message as u64)
            };
            if src < 0 {
                continue;
            }

            // Notifications are fire-and-forget; the sender does not expect a reply.
            if msg.m_type == arch_common::com::NOTIFY_MESSAGE as i32 {
                continue;
            }

            let call_nr = msg.m_type;

            // Dispatch to handler.
            let result = match call_nr {
                RS_UP => unsafe { do_up(&mut msg) },
                RS_DOWN => unsafe { do_down(&msg) },
                RS_REFRESH => unsafe { do_refresh(&msg) },
                RS_RESTART => unsafe { do_restart(&msg) },
                RS_SHUTDOWN => do_shutdown(&msg),
                RS_UPDATE => do_update(&msg),
                RS_CLONE => do_clone(&msg),
                RS_EDIT => do_edit(&msg),
                RS_LOOKUP => unsafe { do_lookup(&mut msg) },
                RS_INIT => unsafe { do_init_ready(&msg) },
                RS_LU_PREPARE => do_upd_ready(&msg),
                RS_GETSYSINFO => ENOSYS,
                _ => ENOSYS,
            };

            // Reply to sender.
            msg.m_type = result;
            unsafe {
                minix_rt::syscall2(SENDREC_CALL, src as u64, &mut msg as *mut Message as u64);
            }
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // No-op on host builds.
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicBool, Ordering};

    static TEST_LOCK: AtomicBool = AtomicBool::new(false);

    struct TestLockGuard;
    impl TestLockGuard {
        fn acquire() -> Self {
            while TEST_LOCK.swap(true, Ordering::SeqCst) {
                core::hint::spin_loop();
            }
            Self
        }
    }
    impl Drop for TestLockGuard {
        fn drop(&mut self) {
            TEST_LOCK.store(false, Ordering::SeqCst);
        }
    }

    fn setup() -> TestLockGuard {
        let guard = TestLockGuard::acquire();
        unsafe {
            rs_init();
        }
        guard
    }

    #[test]
    fn test_constants() {
        assert_eq!(RS_IN_USE, 0x001);
        assert_eq!(RS_EXITING, 0x002);
        assert_eq!(RS_TERMINATED, 0x010);
        assert_eq!(SF_CORE_SRV, 0x001);
        assert_eq!(SF_NEED_REPL, 0x010);
        assert_eq!(NR_SYS_PROCS, 32);
        assert_eq!(RS_MAX_LABEL_LEN, 64);
    }

    #[test]
    fn test_rs_init_clears_table() {
        let _g = setup();
        unsafe {
            assert!(alloc_slot().is_some());
        }
    }

    #[test]
    fn test_alloc_and_free_slot() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            assert!(idx < NR_SYS_PROCS);
            assert!((&*RPROC.as_ptr().add(idx)).flags & RS_IN_USE != 0);

            free_slot(idx);
            assert_eq!((&*RPROC.as_ptr().add(idx)).flags & RS_IN_USE, 0);
        }
    }

    #[test]
    fn test_alloc_all_slots() {
        let _g = setup();
        unsafe {
            let mut count = 0;
            while alloc_slot().is_some() {
                count += 1;
            }
            assert_eq!(count, NR_SYS_PROCS);

            // Next alloc should fail.
            assert!(alloc_slot().is_none());
        }
    }

    #[test]
    fn test_init_slot() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 100, -1, b"test.service").unwrap();

            let rp = &*RPROC.as_ptr().add(idx);
            assert!(rp.flags & RS_IN_USE != 0);
            assert!(rp.flags & RS_INITIALIZING != 0);
            assert_eq!(rp.pid, -1);

            let label = core::str::from_utf8(&rp.label).unwrap();
            assert_eq!(label.trim_end_matches('\0'), "test.service");
        }
    }

    #[test]
    fn test_lookup_slot_by_label() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 100, -1, b"vm.service").unwrap();

            let found = lookup_slot_by_label(b"vm.service");
            assert_eq!(found, Some(idx));

            let not_found = lookup_slot_by_label(b"nonexistent");
            assert_eq!(not_found, None);
        }
    }

    #[test]
    fn test_lookup_slot_by_endpoint() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 42, -1, b"my.service").unwrap();

            let found = lookup_slot_by_endpoint(42);
            assert_eq!(found, Some(idx));

            let not_found = lookup_slot_by_endpoint(999);
            assert_eq!(not_found, None);
        }
    }

    #[test]
    fn test_mark_initialized_and_terminated() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 101, -1, b"test").unwrap();

            mark_initialized(idx, 101).unwrap();
            let rp = &*RPROC.as_ptr().add(idx);
            assert!(rp.flags & RS_ACTIVE != 0);
            assert!(rp.flags & RS_INITIALIZING == 0);

            mark_terminated(idx);
            assert!(rp.flags & RS_TERMINATED != 0);
            assert!(rp.flags & RS_ACTIVE == 0);
        }
    }

    #[test]
    fn test_rs_isokendpt() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 7, -1, b"proc").unwrap();

            assert_eq!(rs_isokendpt(7), Some(idx));
            assert_eq!(rs_isokendpt(8), None); // not in use
            assert_eq!(rs_isokendpt(-1), None); // negative
        }
    }

    #[test]
    fn test_check_call_permission() {
        assert!(check_call_permission(-3, None)); // PM
        assert!(check_call_permission(-4, None)); // RS
        assert!(check_call_permission(-7, None)); // SCHED
        assert!(!check_call_permission(0, None)); // user
        assert!(!check_call_permission(1, None));
    }

    #[test]
    fn test_slot_label_and_endpoint() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 200, -1, b"label.test").unwrap();

            let label = slot_label(idx).unwrap();
            let label_str = core::str::from_utf8(&label).unwrap();
            assert!(label_str.starts_with("label.test"));

            assert_eq!(slot_endpoint(idx), Some(200));
        }
    }

    #[test]
    fn test_lookup_by_pid() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 300, -1, b"pid.test").unwrap();

            // Set PID.
            let rp = &mut *RPROC.as_ptr().add(idx);
            rp.pid = 1234;

            let found = lookup_slot_by_pid(1234);
            assert_eq!(found, Some(idx));

            assert_eq!(lookup_slot_by_pid(9999), None);
        }
    }

    #[test]
    fn test_rs_server_main_callable() {
        rs_server_main();
    }

    #[test]
    fn test_double_alloc_eventually_fails() {
        let _g = setup();
        unsafe {
            for _ in 0..NR_SYS_PROCS {
                assert!(alloc_slot().is_some());
            }
            assert!(alloc_slot().is_none());
        }
    }

    #[test]
    fn test_free_slot_clears_flags() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 400, -1, b"free.test").unwrap();
            free_slot(idx);

            let rp = &*RPROC.as_ptr().add(idx);
            assert_eq!(rp.flags & RS_IN_USE, 0);

            // Slot should be reusable.
            let idx2 = alloc_slot().unwrap();
            assert_eq!(idx2, idx);
        }
    }
}
