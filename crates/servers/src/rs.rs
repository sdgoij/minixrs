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

// Server main loop (stub — see Phase 12 wiring)

/// RS server main loop.
///
/// Receives messages from clients and dispatches RS requests.
pub fn rs_server_main() {
    #[cfg(target_os = "none")]
    {
        // Initialize RS's process table.
        unsafe {
            rs_init();
        }

        // Register boot services so PM's notifications can reach us.
        // For now, just register PM (endpoint 0) as a known service.
        let boot_services: &[(i32, &[u8])] = &[
            (0, b"pm"),
            (1, b"vfs"),
            (2, b"rs"),
            (8, b"vm"),
            (4, b"sched"),
            (5, b"tty"),
            (6, b"ds"),
            (10, b"init"),
        ];
        for &(ep, label) in boot_services {
            if let Some(slot) = unsafe { alloc_slot() } {
                let _ = unsafe { init_slot(slot, ep, -1, label) };
            }
        }

        // Syscall numbers for IPC.
        const RECEIVE_CALL: u64 = 47;
        const SEND_CALL: u64 = 46;
        const ANY: i32 = 0x0000ffff;

        loop {
            let mut buf = [0u8; 64];

            // Receive from any sender.
            let src =
                unsafe { minix_rt::syscall2(RECEIVE_CALL, ANY as u64, buf.as_mut_ptr() as u64) };
            if src < 0 {
                continue;
            }
            let _sender = src as i32;

            // Notifications (m_type == -10) are fire-and-forget.
            // The sender used NOTIFY and does not expect a reply.
            // Actual request messages are acknowledged with ENOSYS
            // (RS is a stub). Use SEND (not SENDREC) so RS doesn't
            // block waiting for the sender to receive the reply.
            // The reply is read from buf[4..8] which contains m_type.
            if i32::from_le_bytes(buf[4..8].try_into().unwrap_or([0; 4])) != -10 {
                // Write ENOSYS to m_type (bytes 4-7) and SEND it back.
                buf[4..8].copy_from_slice(&(-71i32).to_le_bytes()); // ENOSYS
                unsafe {
                    minix_rt::syscall2(SEND_CALL, src as u64, buf.as_mut_ptr() as u64);
                }
            }
        }
    }
    #[cfg(not(target_os = "none"))]
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
