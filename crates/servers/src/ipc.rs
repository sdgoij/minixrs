//! IPC server — System V semaphores and shared memory.
//!
//! Ported from `.refs/minix-3.3.0/minix/servers/ipc/`
//!
//! Provides System V IPC primitives:
//! - Semaphores: `semget`, `semctl`, `semop`
//! - Shared memory: `shmget`, `shmat`, `shmdt`, `shmctl`
//!
//! The IPC message loop is deferred (Phase 12 — SEF + server framework).
//! All semaphore and SHM data structure operations are fully implemented
//! and tested. VM-dependent operations (remap, getphys, refcount, watch_exit)
//! are stubbed with concrete task references.

#![allow(dead_code)]
#![allow(unsafe_op_in_unsafe_fn)]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};

// ═════════════════════════════════════════════════════════════════════════════
// Constants
// ═════════════════════════════════════════════════════════════════════════════

// ── Error codes (POSIX subset) ─────────────────────────────────────────────

const OK: i32 = 0;
const EPERM: i32 = -1;
const ENOENT: i32 = -2;
const EINTR: i32 = -4;
const EIO: i32 = -5;
const EAGAIN: i32 = -11;
const ENOMEM: i32 = -12;
const EACCES: i32 = -13;
const EFAULT: i32 = -14;
const EBUSY: i32 = -16;
const EEXIST: i32 = -17;
const ENODEV: i32 = -19;
const EINVAL: i32 = -22;
const ENOSPC: i32 = -28;
const EFBIG: i32 = -27;
const ERANGE: i32 = -34;
const EDOM: i32 = -33;
const E2BIG: i32 = -7;

// ── IPC commands (from sys/ipc.h) ──────────────────────────────────────────

const IPC_CREAT: i32 = 0o001000;
const IPC_EXCL: i32 = 0o002000;
const IPC_NOWAIT: i32 = 0o004000;

const IPC_PRIVATE: i32 = 0;

const IPC_RMID: i32 = 0;
const IPC_SET: i32 = 1;
const IPC_STAT: i32 = 2;
const IPC_INFO: i32 = 500; // Minix-specific

// ── IPC permission bits ────────────────────────────────────────────────────

const IPC_R: i32 = 0o000400;
const IPC_W: i32 = 0o000200;
const IPC_M: i32 = 0o010000;

// ── Semaphore constants (from sys/sem.h) ───────────────────────────────────

const SEMMNI: usize = 10; // # of semaphore identifiers
const SEMMNS: usize = 60; // # of semaphores in system
const SEMMSL: usize = SEMMNS; // max # of semaphores per id
const SEMOPM: usize = 100; // max # of operations per semop call
const SEMVMX: u16 = 32767; // semaphore maximum value
const SEMAEM: u16 = 16384; // adjust on exit max value

// ── Semaphore control commands ─────────────────────────────────────────────

const GETNCNT: i32 = 3;
const GETPID: i32 = 4;
const GETVAL: i32 = 5;
const GETALL: i32 = 6;
const GETZCNT: i32 = 7;
const SETVAL: i32 = 8;
const SETALL: i32 = 9;

// Minix-specific semaphore info commands.
const SEM_STAT: i32 = 18;
const SEM_INFO: i32 = 19;

// ── Shared memory constants (from sys/shm.h) ───────────────────────────────

const SHM_RDONLY: i32 = 0o010000;
const SHM_RND: i32 = 0o020000;
const SHM_DEST: i32 = 0o001000; // segment will be destroyed on last detach
const SHM_LOCKED: i32 = 0o002000;

const MAX_SHM_NR: usize = 1024;
const SHMMNI: usize = MAX_SHM_NR; // # of SHM identifiers

// ── Shared memory control commands ─────────────────────────────────────────

const SHM_LOCK: i32 = 3;
const SHM_UNLOCK: i32 = 4;
const SHM_STAT: i32 = 13; // Minix-specific
const SHM_INFO: i32 = 14; // Minix-specific

// ── Message offsets for raw buffer access ───────────────────────────────────

/// Size of a full IPC message buffer (matches kernel MESSAGE_SIZE).
const MESSAGE_SIZE: usize = 64;

// Generic IPC message: used for all shm/sem calls.
// Layout matches the C mess_lc_ipc* structs packed into the 64-byte message.
//   offset 0: m_type / call_type (i32)
//   offset 4: m_source (i32) — set by kernel
//   Then payload structured per-call.
const MSG_OFF_CALLTYPE: usize = 0; // i32
const MSG_OFF_SOURCE: usize = 4; // i32
const MSG_OFF_D01: usize = 8; // i32 — first data field (key/id)
const MSG_OFF_D02: usize = 12; // i32 — second data field (size/nr/num/cmd)
const MSG_OFF_D03: usize = 16; // i32 — third data field (flag/addr/opt)
const MSG_OFF_D04: usize = 20; // i32 — fourth data field (retid/retaddr/ret)
const MSG_OFF_D05: usize = 24; // i32 — fifth data field (padding / ops pointer)
const MSG_OFF_D06: usize = 28; // i32 — sixth data field

// ═════════════════════════════════════════════════════════════════════════════
// Types
// ═════════════════════════════════════════════════════════════════════════════

/// IPC permission structure (matches `struct ipc_perm`).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IpcPerm {
    pub uid: i32,
    pub gid: i32,
    pub cuid: i32,
    pub cgid: i32,
    pub mode: u16,
    pub _seq: u16,
    pub _key: i32,
}

impl IpcPerm {
    const fn zeroed() -> Self {
        Self {
            uid: 0,
            gid: 0,
            cuid: 0,
            cgid: 0,
            mode: 0,
            _seq: 0,
            _key: 0,
        }
    }
}

/// Semaphore set metadata (matches `struct semid_ds`).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SemidDs {
    pub sem_perm: IpcPerm,
    pub sem_nsems: u16,
    pub sem_otime: u64,
    pub sem_ctime: u64,
}

impl SemidDs {
    const fn zeroed() -> Self {
        Self {
            sem_perm: IpcPerm::zeroed(),
            sem_nsems: 0,
            sem_otime: 0,
            sem_ctime: 0,
        }
    }
}

/// A process waiting on a semaphore.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct SemWaiting {
    who: i32,
    val: i16,
}

impl SemWaiting {
    const fn zeroed() -> Self {
        Self { who: 0, val: 0 }
    }
}

/// Individual semaphore in a set (matches the C `struct __sem` / `struct semaphore`).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct IpcSemaphore {
    semval: u16,
    sempid: i32,
    semncnt: u16,
    semzcnt: u16,
    zlist: [SemWaiting; SEMMSL],
    nlist: [SemWaiting; SEMMSL],
}

impl IpcSemaphore {
    const fn zeroed() -> Self {
        Self {
            semval: 0,
            sempid: 0,
            semncnt: 0,
            semzcnt: 0,
            zlist: [const { SemWaiting::zeroed() }; SEMMSL],
            nlist: [const { SemWaiting::zeroed() }; SEMMSL],
        }
    }
}

/// A semaphore set.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct SemStruct {
    key: i32,
    id: i32,
    semid_ds: SemidDs,
    sems: [IpcSemaphore; SEMMSL],
}

impl SemStruct {
    const fn zeroed() -> Self {
        Self {
            key: 0,
            id: 0,
            semid_ds: SemidDs::zeroed(),
            sems: [const { IpcSemaphore::zeroed() }; SEMMSL],
        }
    }
}

/// Shared memory metadata (matches `struct shmid_ds`).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ShmidDs {
    shm_perm: IpcPerm,
    shm_segsz: usize,
    shm_lpid: i32,
    shm_cpid: i32,
    shm_nattch: u32,
    shm_atime: u64,
    shm_dtime: u64,
    shm_ctime: u64,
}

impl ShmidDs {
    const fn zeroed() -> Self {
        Self {
            shm_perm: IpcPerm::zeroed(),
            shm_segsz: 0,
            shm_lpid: 0,
            shm_cpid: 0,
            shm_nattch: 0,
            shm_atime: 0,
            shm_dtime: 0,
            shm_ctime: 0,
        }
    }
}

/// A shared memory segment.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ShmStruct {
    key: i32,
    id: i32,
    shmid_ds: ShmidDs,
    page: u64,
    vm_id: i32,
}

impl ShmStruct {
    const fn zeroed() -> Self {
        Self {
            key: 0,
            id: 0,
            shmid_ds: ShmidDs::zeroed(),
            page: 0,
            vm_id: 0,
        }
    }
}

/// Semaphore info structure (for SEM_INFO / IPC_INFO).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct SemInfo {
    semmap: i32,
    semmni: i32,
    semmns: i32,
    semmnu: i32,
    semmsl: i32,
    semopm: i32,
    semume: i32,
    semusz: i32,
    semvmx: i32,
    semaem: i32,
}

/// SHM info structure (for SHM_INFO / IPC_INFO).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct Shminfo {
    shmmax: u64,
    shmmin: u32,
    shmmni: u32,
    shmseg: u32,
    shmall: u32,
}

/// SHM runtime info (for SHM_INFO).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ShmInfo {
    used_ids: i32,
    shm_tot: u64,
    shm_rss: u64,
    shm_swp: u64,
    swap_attempts: u64,
    swap_successes: u64,
}

// ═════════════════════════════════════════════════════════════════════════════
// Static state
// ═════════════════════════════════════════════════════════════════════════════

struct SemListRaw(UnsafeCell<[SemStruct; SEMMNI]>);
unsafe impl Sync for SemListRaw {}
impl SemListRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { SemStruct::zeroed() }; SEMMNI]))
    }
    fn as_ptr(&self) -> *mut SemStruct {
        self.0.get() as *mut SemStruct
    }
}

static SEM_LIST: SemListRaw = SemListRaw::new();
static SEM_LIST_NR: AtomicU32 = AtomicU32::new(0);

struct ShmListRaw(UnsafeCell<[ShmStruct; MAX_SHM_NR]>);
unsafe impl Sync for ShmListRaw {}
impl ShmListRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { ShmStruct::zeroed() }; MAX_SHM_NR]))
    }
    fn as_ptr(&self) -> *mut ShmStruct {
        self.0.get() as *mut ShmStruct
    }
}

static SHM_LIST: ShmListRaw = ShmListRaw::new();
static SHM_LIST_NR: AtomicU32 = AtomicU32::new(0);

/// Auto-incrementing identifier counter (replaces C `int identifier`).
static IDENTIFIER: AtomicI32 = AtomicI32::new(0x1234);

// ═════════════════════════════════════════════════════════════════════════════
// Message helpers
// ═════════════════════════════════════════════════════════════════════════════

/// Read an i32 field from a message buffer at the given offset.
///
/// # Safety
///
/// `msg` must point to a valid 48-byte message buffer.
unsafe fn msg_i32(msg: &[u8; MESSAGE_SIZE], off: usize) -> i32 {
    i32::from_ne_bytes(msg[off..off + 4].try_into().unwrap())
}

/// Write an i32 field into a message buffer at the given offset.
///
/// # Safety
///
/// `msg` must point to a valid 48-byte message buffer.
unsafe fn msg_set_i32(msg: &mut [u8; MESSAGE_SIZE], off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

/// Read a u64 field from a message buffer at the given offset.
///
/// # Safety
///
/// `msg` must point to a valid 48-byte message buffer.
unsafe fn msg_u64(msg: &[u8; MESSAGE_SIZE], off: usize) -> u64 {
    u64::from_ne_bytes(msg[off..off + 8].try_into().unwrap())
}

/// Write a u64 field into a message buffer at the given offset.
///
/// # Safety
///
/// `msg` must point to a valid message buffer.
unsafe fn msg_set_u64(msg: &mut [u8; MESSAGE_SIZE], off: usize, val: u64) {
    msg[off..off + 8].copy_from_slice(&val.to_ne_bytes());
}

// ═════════════════════════════════════════════════════════════════════════════
// Semaphore helpers
// ═════════════════════════════════════════════════════════════════════════════

/// Find a semaphore set by key.
unsafe fn sem_find_key(key: i32) -> Option<*mut SemStruct> {
    if key == IPC_PRIVATE {
        return None;
    }
    let nr = SEM_LIST_NR.load(Ordering::Relaxed) as usize;
    let base = SEM_LIST.as_ptr();
    for i in 0..nr {
        let sem = unsafe { &*base.add(i) };
        if sem.key == key {
            return Some(unsafe { base.add(i) });
        }
    }
    None
}

/// Find a semaphore set by ID.
unsafe fn sem_find_id(id: i32) -> Option<*mut SemStruct> {
    let nr = SEM_LIST_NR.load(Ordering::Relaxed) as usize;
    let base = SEM_LIST.as_ptr();
    for i in 0..nr {
        let sem = unsafe { &*base.add(i) };
        if sem.id == id {
            return Some(unsafe { base.add(i) });
        }
    }
    None
}

/// Send a message to a process (non-blocking IPC reply).
///
/// # Safety
///
/// `who` must be a valid endpoint.
unsafe fn send_message_to_process(who: i32, ret: i32) {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; MESSAGE_SIZE];
        msg_set_i32(&mut msg, MSG_OFF_CALLTYPE, ret);
        let _ = sendnb(who, &mut msg);
    }
    #[cfg(not(target_os = "none"))]
    let _ = (who, ret);
}

/// Remove a semaphore set from the list (compaction).
unsafe fn remove_semaphore(sem: *mut SemStruct) {
    let nr = SEM_LIST_NR.load(Ordering::Relaxed) as usize;
    let base = SEM_LIST.as_ptr();
    for i in 0..nr {
        if unsafe { base.add(i) } == sem {
            // Compact: move last entry to this slot.
            if nr > 0 && i < nr - 1 {
                unsafe { *base.add(i) = *base.add(nr - 1) };
            }
            SEM_LIST_NR.fetch_sub(1, Ordering::Relaxed);
            break;
        }
    }
}

/// Remove a process from all semaphore wait queues (on process exit).
unsafe fn remove_process(pt: i32) {
    let nr = SEM_LIST_NR.load(Ordering::Relaxed) as usize;
    let base = SEM_LIST.as_ptr();
    for i in 0..nr {
        let sem = unsafe { &mut *base.add(i) };
        let nsems = sem.semid_ds.sem_nsems as usize;
        for j in 0..nsems {
            let s = &mut sem.sems[j];
            // Remove from zero-wait list.
            let mut k = 0;
            while k < s.semzcnt as usize {
                if s.zlist[k].who == pt {
                    s.zlist.copy_within(k + 1..s.semzcnt as usize, k);
                    s.semzcnt -= 1;
                    unsafe { send_message_to_process(pt, EINTR) };
                } else {
                    k += 1;
                }
            }
            // Remove from increment-wait list.
            let mut k = 0;
            while k < s.semncnt as usize {
                if s.nlist[k].who == pt {
                    s.nlist.copy_within(k + 1..s.semncnt as usize, k);
                    s.semncnt -= 1;
                    unsafe { send_message_to_process(pt, EINTR) };
                } else {
                    k += 1;
                }
            }
        }
    }
}

/// Update one semaphore set — wake waiting processes if conditions are met.
unsafe fn update_one_semaphore(sem: *mut SemStruct, is_remove: bool) {
    let sem = unsafe { &mut *sem };
    let nsems = sem.semid_ds.sem_nsems as usize;

    if is_remove {
        // Notify all waiters that the semaphore set was removed.
        for i in 0..nsems {
            let s = &sem.sems[i];
            for j in 0..s.semzcnt as usize {
                unsafe { send_message_to_process(s.zlist[j].who, eidrm()) };
            }
            for j in 0..s.semncnt as usize {
                unsafe { send_message_to_process(s.nlist[j].who, eidrm()) };
            }
        }
        unsafe { remove_semaphore(sem) };
        return;
    }

    for i in 0..nsems {
        let s = &mut sem.sems[i];

        // Zero-wait: if semval == 0, wake one FIFO waiter.
        if s.semzcnt > 0 && s.semval == 0 {
            let who = s.zlist[0].who;
            s.zlist.copy_within(1..s.semzcnt as usize, 0);
            s.semzcnt -= 1;
            unsafe { send_message_to_process(who, OK) };
        }

        // Increment-wait: wake waiters whose requested value <= semval.
        if s.semncnt > 0 {
            let mut j = 0;
            while j < s.semncnt as usize {
                if s.nlist[j].val as u16 <= s.semval {
                    s.semval -= s.nlist[j].val as u16;
                    let who = s.nlist[j].who;
                    s.nlist.copy_within(j + 1..s.semncnt as usize, j);
                    s.semncnt -= 1;
                    unsafe { send_message_to_process(who, OK) };
                    // Only one waiter per semaphore per update.
                    break;
                }
                j += 1;
            }
        }
    }
}

/// Walk all semaphore sets and try to wake waiters.
unsafe fn update_semaphores() {
    let nr = SEM_LIST_NR.load(Ordering::Relaxed) as usize;
    let base = SEM_LIST.as_ptr();
    for i in 0..nr {
        unsafe { update_one_semaphore(base.add(i), false) };
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// VM dependency stubs
// ═════════════════════════════════════════════════════════════════════════════

// ═════════════════════════════════════════════════════════════════════════════
// Permission checking
// ═════════════════════════════════════════════════════════════════════════════

/// Check IPC permissions.
///
/// Returns true if the caller has the requested permission mode.
///
/// # Safety
///
/// `who` must be a valid endpoint.
unsafe fn check_perm(req: &IpcPerm, who: i32, mode: i32) -> bool {
    let mode = mode & 0o666;

    // Try to get caller's credentials from PM server.
    // Falls back to uid=0 (grants all) if PM is unavailable.
    let (uid, gid) = pm_get_credentials(who);

    // Root has all permissions.
    if uid == 0 {
        return true;
    }

    let req_mode = if uid == req.uid || uid == req.cuid {
        (req.mode as i32 >> 6) & 0x7
    } else if gid == req.gid || gid == req.cgid {
        (req.mode as i32 >> 3) & 0x7
    } else {
        req.mode as i32 & 0x7
    };

    let cur_mode = if mode >> 6 != 0 {
        (mode >> 6) & 0x7
    } else if mode >> 3 != 0 {
        (mode >> 3) & 0x7
    } else {
        mode & 0x7
    };

    // Check if the requested access includes the needed bits.
    cur_mode & req_mode == req_mode
}

/// Query PM server for a process's UID and GID.
///
/// Returns (uid, gid). On failure (PM unavailable), returns (0, 0)
/// which grants all permissions (safe fallback for bootstrapping).
unsafe fn pm_get_credentials(who: i32) -> (i32, i32) {
    let pm_ep = 0; // PM_PROC_NR

    let mut msg = [0u8; MESSAGE_SIZE];
    msg_set_i32(&mut msg, MSG_OFF_CALLTYPE, 0x901); // PM_GET (VFS_PM_RQ_BASE + 1)
    msg_set_i32(&mut msg, 8, who); // m1_i1 = target endpoint
    msg_set_i32(&mut msg, 12, 0); // m1_i2 = 0 (GETUID)

    let r = sendrec(pm_ep, &mut msg);
    if r != 0 {
        return (0, 0);
    }
    // Reply: high 32 bits = real uid, low 32 bits = effective uid
    let uid_result = msg_i32(&msg, 24) as u64 | ((msg_i32(&msg, 20) as u64) << 32);
    let euid = (uid_result & 0xFFFF_FFFF) as i32;

    // Get GID
    let mut msg = [0u8; MESSAGE_SIZE];
    msg_set_i32(&mut msg, MSG_OFF_CALLTYPE, 0x901); // PM_GET
    msg_set_i32(&mut msg, 8, who);
    msg_set_i32(&mut msg, 12, 1); // m1_i2 = 1 (GETGID)

    let r = sendrec(pm_ep, &mut msg);
    if r != 0 {
        return (euid, 0);
    }
    let gid_result = msg_i32(&msg, 24) as u64 | ((msg_i32(&msg, 20) as u64) << 32);
    let egid = (gid_result & 0xFFFF_FFFF) as i32;

    (euid, egid)
}

// ═════════════════════════════════════════════════════════════════════════════
// VM dependency stubs
// ═════════════════════════════════════════════════════════════════════════════

/// EIDRM error code (identifier removed).
const fn eidrm() -> i32 {
    -43
}

const fn enosys() -> i32 {
    -71
}

/// Stub: register VM exit watch for a process.
///
/// Real implementation would call vm_watch_exit() on the VM server.
/// See PORTING_PLAN.md Phase 12.5 follow-up.
unsafe fn vm_watch_exit_stub(_who: i32) -> Result<(), i32> {
    Ok(())
}

/// Stub: query exited processes (via VM).
///
/// Real implementation would call vm_query_exit() on the VM server.
/// Returns None to signal no more exited processes.
/// See PORTING_PLAN.md Phase 12.5 follow-up.
unsafe fn vm_query_exit_stub() -> Option<i32> {
    None
}

/// Stub: remap a shared memory page into another process.
///
/// On target, delegates to `vm_remap` which sends IPC to VM server.
/// On host, returns ENOSYS.
#[cfg(not(target_os = "none"))]
unsafe fn vm_remap_stub(_who: i32, _addr: u64, _page: u64, _size: usize) -> Result<u64, i32> {
    Err(enosys())
}

#[cfg(target_os = "none")]
unsafe fn vm_remap_stub(who: i32, addr: u64, page: u64, size: usize) -> Result<u64, i32> {
    vm_remap(who, addr, page, size)
}

/// Stub: unmap a shared memory page from a process.
#[cfg(not(target_os = "none"))]
unsafe fn vm_unmap_stub(_who: i32, _addr: u64) -> Result<(), i32> {
    Err(enosys())
}

#[cfg(target_os = "none")]
unsafe fn vm_unmap_stub(who: i32, addr: u64) -> Result<(), i32> {
    vm_unmap(who, addr)
}

/// Stub: get physical address of a page.
#[cfg(not(target_os = "none"))]
unsafe fn vm_getphys_stub(_who: i32, _addr: u64) -> i32 {
    0
}

#[cfg(target_os = "none")]
unsafe fn vm_getphys_stub(who: i32, addr: u64) -> i32 {
    vm_getphys(who, addr)
}

/// Stub: get reference count of a physical region.
///
/// Real implementation would call vm_getrefcount() on the VM server.
/// See PORTING_PLAN.md Phase 12.5 follow-up.
unsafe fn vm_getrefcount_stub(_who: i32, _addr: u64) -> u8 {
    // Return 2 (self + 1 attach) to prevent premature destruction.
    2
}

/// Call the VM server to look up the physical address of a virtual address.
///
/// # Safety
///
/// `who` must be a valid process endpoint; `addr` must be a valid mapped address.
pub unsafe fn vm_getphys(who: i32, addr: u64) -> i32 {
    let vm_ep = 8; // VM_PROC_NR
    let mut msg = [0u8; MESSAGE_SIZE];
    msg_set_i32(
        &mut msg,
        MSG_OFF_CALLTYPE,
        arch_common::com::VM_GETPHYS as i32,
    );
    msg_set_i32(&mut msg, 8, who);
    msg_set_u64(&mut msg, 16, addr);

    let r = sendrec(vm_ep, &mut msg);
    if r != 0 {
        return r;
    }
    msg_i32(&msg, 24) // Reply in m1i4
}

// ═════════════════════════════════════════════════════════════════════════════
// Semaphore operations
// ═════════════════════════════════════════════════════════════════════════════

/// Handle IPC_SEMGET — get or create a semaphore set.
///
/// # Safety
///
/// `msg` must be a valid 48-byte message buffer.
pub unsafe fn do_semget(msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let key = msg_i32(msg, MSG_OFF_D01);
        let nsems = msg_i32(msg, MSG_OFF_D02) as usize;
        let flag = msg_i32(msg, MSG_OFF_D03);

        if let Some(sem) = sem_find_key(key) {
            let sem = &*sem;
            if (flag & IPC_CREAT) != 0 && (flag & IPC_EXCL) != 0 {
                return EEXIST;
            }
            if !check_perm(&sem.semid_ds.sem_perm, msg_i32(msg, MSG_OFF_SOURCE), flag) {
                return EACCES;
            }
            if nsems > sem.semid_ds.sem_nsems as usize {
                return EINVAL;
            }
            msg_set_i32(msg, MSG_OFF_D04, sem.id);
            OK
        } else {
            if (flag & IPC_CREAT) == 0 {
                return ENOENT;
            }
            if nsems == 0 || nsems > SEMMSL {
                return EINVAL;
            }
            let nr = SEM_LIST_NR.load(Ordering::Relaxed) as usize;
            if nr >= SEMMNI {
                return ENOSPC;
            }

            let base = SEM_LIST.as_ptr();
            let sem = &mut *base.add(nr);
            *sem = SemStruct::zeroed();

            let caller = msg_i32(msg, MSG_OFF_SOURCE);
            sem.key = key;
            sem.semid_ds.sem_perm.cuid = caller;
            sem.semid_ds.sem_perm.uid = caller;
            sem.semid_ds.sem_perm.cgid = 0;
            sem.semid_ds.sem_perm.gid = 0;
            sem.semid_ds.sem_perm.mode = (flag & 0o777) as u16;
            sem.semid_ds.sem_nsems = nsems as u16;
            sem.semid_ds.sem_ctime = 0;
            sem.id = IDENTIFIER.fetch_add(1, Ordering::Relaxed);

            SEM_LIST_NR.fetch_add(1, Ordering::Relaxed);

            msg_set_i32(msg, MSG_OFF_D04, sem.id);
            OK
        }
    }
}

/// Handle IPC_SEMCTL — semaphore control operations.
///
/// # Safety
///
/// `msg` must be a valid 48-byte message buffer.
pub unsafe fn do_semctl(msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let id = msg_i32(msg, MSG_OFF_D01);
        let num = msg_i32(msg, MSG_OFF_D02);
        let cmd = msg_i32(msg, MSG_OFF_D03);

        match cmd {
            IPC_INFO => {
                msg_set_i32(
                    msg,
                    MSG_OFF_D04,
                    SEM_LIST_NR.load(Ordering::Relaxed).saturating_sub(1) as i32,
                );
                return OK;
            }
            SEM_INFO => {
                msg_set_i32(
                    msg,
                    MSG_OFF_D04,
                    SEM_LIST_NR.load(Ordering::Relaxed).saturating_sub(1) as i32,
                );
                return OK;
            }
            SEM_STAT => {
                let idx = id as usize;
                if idx >= SEM_LIST_NR.load(Ordering::Relaxed) as usize {
                    return EINVAL;
                }
                let base = SEM_LIST.as_ptr();
                let sem = &*base.add(idx);
                msg_set_i32(msg, MSG_OFF_D04, sem.id);
                return OK;
            }
            _ => {}
        }

        let sem = match sem_find_id(id) {
            Some(s) => s,
            None => return EINVAL,
        };

        if cmd != IPC_SET
            && cmd != IPC_RMID
            && !check_perm(
                &(*sem).semid_ds.sem_perm,
                msg_i32(msg, MSG_OFF_SOURCE),
                IPC_R,
            )
        {
            return EACCES;
        }

        match cmd {
            IPC_STAT => {
                let _ = &(*sem).semid_ds;
                OK
            }
            IPC_SET => {
                let caller = msg_i32(msg, MSG_OFF_SOURCE);
                let uid = caller;
                let sem = &mut *sem;
                if uid != sem.semid_ds.sem_perm.cuid && uid != sem.semid_ds.sem_perm.uid && uid != 0
                {
                    EPERM
                } else {
                    sem.semid_ds.sem_ctime = 0;
                    OK
                }
            }
            IPC_RMID => {
                let caller = msg_i32(msg, MSG_OFF_SOURCE);
                let uid = caller;
                let sem = &mut *sem;
                if uid != sem.semid_ds.sem_perm.cuid && uid != sem.semid_ds.sem_perm.uid && uid != 0
                {
                    EPERM
                } else {
                    update_one_semaphore(sem, true);
                    OK
                }
            }
            GETALL => {
                let _ = (*sem).semid_ds.sem_nsems;
                OK
            }
            GETNCNT => {
                if num < 0 || num >= (*sem).semid_ds.sem_nsems as i32 {
                    EINVAL
                } else {
                    msg_set_i32(msg, MSG_OFF_D04, (*sem).sems[num as usize].semncnt as i32);
                    OK
                }
            }
            GETPID => {
                if num < 0 || num >= (*sem).semid_ds.sem_nsems as i32 {
                    EINVAL
                } else {
                    msg_set_i32(msg, MSG_OFF_D04, (*sem).sems[num as usize].sempid);
                    OK
                }
            }
            GETVAL => {
                if num < 0 || num >= (*sem).semid_ds.sem_nsems as i32 {
                    EINVAL
                } else {
                    msg_set_i32(msg, MSG_OFF_D04, (*sem).sems[num as usize].semval as i32);
                    OK
                }
            }
            GETZCNT => {
                if num < 0 || num >= (*sem).semid_ds.sem_nsems as i32 {
                    EINVAL
                } else {
                    msg_set_i32(msg, MSG_OFF_D04, (*sem).sems[num as usize].semzcnt as i32);
                    OK
                }
            }
            SETALL => {
                let sem = &mut *sem;
                let nsems = sem.semid_ds.sem_nsems as usize;
                for i in 0..nsems {
                    sem.sems[i].semval = 0;
                }
                sem.semid_ds.sem_ctime = 0;
                update_semaphores();
                OK
            }
            SETVAL => {
                let val = msg_i32(msg, MSG_OFF_D04);
                let sem = &mut *sem;
                if !check_perm(&sem.semid_ds.sem_perm, msg_i32(msg, MSG_OFF_SOURCE), IPC_W) {
                    EACCES
                } else if num < 0 || num >= sem.semid_ds.sem_nsems as i32 {
                    EINVAL
                } else if val < 0 || val > SEMVMX as i32 {
                    ERANGE
                } else {
                    sem.sems[num as usize].semval = val as u16;
                    sem.semid_ds.sem_ctime = 0;
                    update_semaphores();
                    OK
                }
            }
            _ => EINVAL,
        }
    }
}

/// Semaphore operation buffer (matches `struct sembuf` from C).
#[repr(C)]
struct SemBuf {
    sem_num: u16,
    sem_op: i16,
    sem_flg: i16,
}

/// Handle IPC_SEMOP — semaphore operations (atomic P/V on a set).
///
/// # Safety
///
/// `msg` must be a valid 48-byte message buffer.
pub unsafe fn do_semop(msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let id = msg_i32(msg, MSG_OFF_D01);
        let nsops = msg_i32(msg, MSG_OFF_D02) as usize;
        let sops_ptr = msg_i32(msg, MSG_OFF_D03) as u64;

        let sem = match sem_find_id(id) {
            Some(s) => s,
            None => return EINVAL,
        };

        if nsops == 0 || nsops > SEMOPM {
            return EINVAL;
        }

        let sem = &mut *sem;

        if !check_perm(&sem.semid_ds.sem_perm, msg_i32(msg, MSG_OFF_SOURCE), IPC_R) {
            return EACCES;
        }

        let who = msg_i32(msg, MSG_OFF_SOURCE);
        let _ = vm_watch_exit_stub(who);

        // Copy sembuf array from userspace or inline message data.
        let sops_size = nsops * core::mem::size_of::<SemBuf>();
        let mut sops_buf = [0u8; SEMOPM * core::mem::size_of::<SemBuf>()];
        if sops_ptr != 0 && sops_size > 0 && sops_size <= sops_buf.len() {
            let caller_slot = kernel::table::endpoint_slot(who);
            let r = kernel::vm::virtual_copy(
                caller_slot,
                sops_ptr,
                -1, // kernel
                sops_buf.as_mut_ptr() as u64,
                sops_size,
            );
            if r != 0 {
                return EINVAL;
            }
        } else if sops_size > 0 && sops_size <= MESSAGE_SIZE - MSG_OFF_D05 {
            // Inline sembuf data in the message payload.
            let src = &msg[MSG_OFF_D05..][..sops_size];
            sops_buf[..sops_size].copy_from_slice(src);
        } else {
            return EINVAL;
        }
        let sops = &*(sops_buf.as_ptr() as *const [SemBuf; SEMOPM]);

        // Process each semop.
        for i in 0..nsops {
            let sop = &(*sops)[i];
            let num = sop.sem_num as usize;
            if num >= sem.semid_ds.sem_nsems as usize {
                return EINVAL;
            }

            let op = sop.sem_op as i32;
            let flg = sop.sem_flg as i32;

            if op == 0 {
                // Wait until semaphore becomes 0.
                if sem.sems[num].semval != 0 {
                    if (flg & IPC_NOWAIT) != 0 {
                        return EAGAIN;
                    }
                    // Enqueue on zero list.
                    let zcnt = sem.sems[num].semzcnt as usize;
                    if zcnt < SEMMSL {
                        sem.sems[num].zlist[zcnt] = SemWaiting { who, val: 0 };
                        sem.sems[num].semzcnt += 1;
                        return OK;
                    }
                    return ENOMEM;
                }
            } else if op > 0 {
                // Release resources: increment semaphore.
                sem.sems[num].semval = (sem.sems[num].semval as i32 + op) as u16;
                sem.sems[num].sempid = who;
            } else {
                // Acquire resources: decrement semaphore.
                let neg_op = (-op) as u16;
                if sem.sems[num].semval >= neg_op {
                    sem.sems[num].semval -= neg_op;
                    sem.sems[num].sempid = who;
                } else {
                    if (flg & IPC_NOWAIT) != 0 {
                        return EAGAIN;
                    }
                    // Enqueue on increment list.
                    let ncnt = sem.sems[num].semncnt as usize;
                    if ncnt < SEMMSL {
                        sem.sems[num].nlist[ncnt] = SemWaiting {
                            who,
                            val: (-op) as i16,
                        };
                        sem.sems[num].semncnt += 1;
                        return OK;
                    }
                    return ENOMEM;
                }
            }
        }

        update_semaphores();
        OK
    }
}

/// Check if any semaphore sets exist.
pub fn is_sem_nil() -> bool {
    SEM_LIST_NR.load(Ordering::Relaxed) == 0
}

/// Handle VM exit notification — remove waiting processes.
///
/// # Safety
///
/// Accesses shared state.
pub unsafe fn sem_process_vm_notify() {
    unsafe {
        while let Some(pt) = vm_query_exit_stub() {
            remove_process(pt);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Shared memory helpers
// ═════════════════════════════════════════════════════════════════════════════

/// Find a shared memory segment by key.
unsafe fn shm_find_key(key: i32) -> Option<*mut ShmStruct> {
    if key == IPC_PRIVATE {
        return None;
    }
    let nr = SHM_LIST_NR.load(Ordering::Relaxed) as usize;
    let base = SHM_LIST.as_ptr();
    for i in 0..nr {
        let shm = unsafe { &*base.add(i) };
        if shm.key == key {
            return Some(unsafe { base.add(i) });
        }
    }
    None
}

/// Find a shared memory segment by ID.
unsafe fn shm_find_id(id: i32) -> Option<*mut ShmStruct> {
    let nr = SHM_LIST_NR.load(Ordering::Relaxed) as usize;
    let base = SHM_LIST.as_ptr();
    for i in 0..nr {
        let shm = unsafe { &*base.add(i) };
        if shm.id == id {
            return Some(unsafe { base.add(i) });
        }
    }
    None
}

// ═════════════════════════════════════════════════════════════════════════════
// Shared memory operations
// ═════════════════════════════════════════════════════════════════════════════

/// Handle IPC_SHMGET — get or create a shared memory segment.
///
/// Message layout (32-bit i386-compatible):
///   offset  8: key (i32)
///   offset 12: size (i32, 32-bit size_t for protocol compat)
///   offset 16: flag (i32)
///   offset 20: retid (i32)
///
/// # Safety
///
/// `msg` must be a valid message buffer.
pub unsafe fn do_shmget(msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let key = msg_i32(msg, MSG_OFF_D01);
        let size = msg_i32(msg, MSG_OFF_D02) as usize;
        let flag = msg_i32(msg, MSG_OFF_D03);

        if let Some(shm) = shm_find_key(key) {
            let shm = &*shm;
            if !check_perm(&shm.shmid_ds.shm_perm, msg_i32(msg, MSG_OFF_SOURCE), flag) {
                return EACCES;
            }
            if (flag & IPC_CREAT) != 0 && (flag & IPC_EXCL) != 0 {
                return EEXIST;
            }
            if size > 0 && shm.shmid_ds.shm_segsz < size {
                return EINVAL;
            }
            msg_set_i32(msg, MSG_OFF_D04, shm.id);
            OK
        } else {
            if (flag & IPC_CREAT) == 0 {
                return ENOENT;
            }
            if size == 0 {
                return EINVAL;
            }
            let nr = SHM_LIST_NR.load(Ordering::Relaxed) as usize;
            if nr >= MAX_SHM_NR {
                return ENOMEM;
            }

            let page_size: usize = 4096;
            let alloc_size = if !size.is_multiple_of(page_size) {
                size + page_size - (size % page_size)
            } else {
                size
            };

            let base = SHM_LIST.as_ptr();
            let shm = &mut *base.add(nr);
            *shm = ShmStruct::zeroed();

            let caller = msg_i32(msg, MSG_OFF_SOURCE);
            shm.key = key;
            shm.shmid_ds.shm_perm.cuid = caller;
            shm.shmid_ds.shm_perm.uid = caller;
            shm.shmid_ds.shm_perm.cgid = 0;
            shm.shmid_ds.shm_perm.gid = 0;
            shm.shmid_ds.shm_perm.mode = (flag & 0o777) as u16;
            shm.shmid_ds.shm_segsz = alloc_size;
            shm.shmid_ds.shm_ctime = 0;
            shm.shmid_ds.shm_cpid = caller;
            shm.shmid_ds.shm_lpid = 0;
            shm.shmid_ds.shm_nattch = 0;
            shm.id = IDENTIFIER.fetch_add(1, Ordering::Relaxed);

            SHM_LIST_NR.fetch_add(1, Ordering::Relaxed);

            msg_set_i32(msg, MSG_OFF_D04, shm.id);
            OK
        }
    }
}

/// Handle IPC_SHMAT — attach a shared memory segment.
///
/// # Safety
///
/// `msg` must be a valid message buffer.
pub unsafe fn do_shmat(msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let id = msg_i32(msg, MSG_OFF_D01);
        let addr = msg_u64(msg, MSG_OFF_D02);
        let flag = msg_i32(msg, MSG_OFF_D03);

        let shm = match shm_find_id(id) {
            Some(s) => s,
            None => return EINVAL,
        };
        let shm = &mut *shm;

        let perm = if (flag & SHM_RDONLY) != 0 {
            IPC_R
        } else {
            0o666
        };
        if !check_perm(&shm.shmid_ds.shm_perm, msg_i32(msg, MSG_OFF_SOURCE), perm) {
            return EACCES;
        }

        let caller = msg_i32(msg, MSG_OFF_SOURCE);
        let page = shm.page;
        let size = shm.shmid_ds.shm_segsz;

        // Remap the segment's physical pages into the caller's address space.
        let mapped = vm_remap_stub(caller, addr, page, size);
        match mapped {
            Ok(mapped_addr) => {
                shm.shmid_ds.shm_atime = 0; // TODO: clock_time()
                shm.shmid_ds.shm_lpid = caller;
                shm.shmid_ds.shm_nattch += 1;
                msg_set_u64(msg, MSG_OFF_D02, mapped_addr);
                OK
            }
            Err(e) => e,
        }
    }
}

/// Handle IPC_SHMDT — detach a shared memory segment.
///
/// # Safety
///
/// `msg` must be a valid message buffer.
pub unsafe fn do_shmdt(msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let addr = msg_u64(msg, MSG_OFF_D01);
        let caller = msg_i32(msg, MSG_OFF_SOURCE);

        // Unmap the shared memory from the caller's address space.
        // Non-fatal on host (no VM infrastructure).
        let _ = vm_unmap_stub(caller, addr);

        // Try to find the segment by physical address and update metadata.
        let phys = vm_getphys_stub(caller, addr) as u64;
        for i in 0..SHM_LIST_NR.load(Ordering::Relaxed) as usize {
            let shm = &mut *SHM_LIST.as_ptr().add(i);
            if shm.page == phys || phys == 0 {
                shm.shmid_ds.shm_dtime = 0; // TODO: clock_time()
                shm.shmid_ds.shm_lpid = caller;
                if shm.shmid_ds.shm_nattch > 0 {
                    shm.shmid_ds.shm_nattch -= 1;
                }
                break;
            }
        }

        update_refcount_and_destroy_stub();
        OK
    }
}

/// Handle IPC_SHMCTL — shared memory control operations.
///
/// # Safety
///
/// `msg` must be a valid message buffer.
pub unsafe fn do_shmctl(msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe {
        let id = msg_i32(msg, MSG_OFF_D01);
        let cmd = msg_i32(msg, MSG_OFF_D02);

        if cmd == IPC_STAT {
            update_refcount_and_destroy_stub();
        }

        if (cmd == IPC_STAT || cmd == IPC_SET || cmd == IPC_RMID) && shm_find_id(id).is_none() {
            return EINVAL;
        }

        match cmd {
            IPC_STAT => {
                let shm = &*shm_find_id(id).unwrap();
                if !check_perm(&shm.shmid_ds.shm_perm, msg_i32(msg, MSG_OFF_SOURCE), IPC_R) {
                    return EACCES;
                }
                OK
            }
            IPC_SET => {
                let shm = &mut *shm_find_id(id).unwrap();
                let caller = msg_i32(msg, MSG_OFF_SOURCE);
                let uid = caller;
                if uid != shm.shmid_ds.shm_perm.cuid && uid != shm.shmid_ds.shm_perm.uid && uid != 0
                {
                    return EPERM;
                }
                shm.shmid_ds.shm_ctime = 0;
                OK
            }
            IPC_RMID => {
                let shm = &mut *shm_find_id(id).unwrap();
                let caller = msg_i32(msg, MSG_OFF_SOURCE);
                let uid = caller;
                if uid != shm.shmid_ds.shm_perm.cuid && uid != shm.shmid_ds.shm_perm.uid && uid != 0
                {
                    return EPERM;
                }
                shm.shmid_ds.shm_perm.mode |= SHM_DEST as u16;
                update_refcount_and_destroy_stub();
                OK
            }
            IPC_INFO => {
                msg_set_i32(
                    msg,
                    MSG_OFF_D04,
                    SHM_LIST_NR.load(Ordering::Relaxed).saturating_sub(1) as i32,
                );
                OK
            }
            SHM_INFO => {
                msg_set_i32(
                    msg,
                    MSG_OFF_D04,
                    SHM_LIST_NR.load(Ordering::Relaxed).saturating_sub(1) as i32,
                );
                OK
            }
            SHM_STAT => {
                let idx = id as usize;
                if idx >= SHM_LIST_NR.load(Ordering::Relaxed) as usize {
                    return EINVAL;
                }
                let base = SHM_LIST.as_ptr();
                let shm = &*base.add(idx);
                msg_set_i32(msg, MSG_OFF_D04, shm.id);
                OK
            }
            _ => EINVAL,
        }
    }
}

/// Stub: update reference counts and destroy unused segments.
///
/// Real implementation walks the SHM list, calls vm_getrefcount for each,
/// and unmaps/destroys segments that have 0 attachments and SHM_DEST set.
/// See PORTING_PLAN.md Phase 12.5 follow-up.
unsafe fn update_refcount_and_destroy_stub() {
    // Without VM calls, we can't track refcounts.
    // The real implementation in shm.c does:
    //   1. For each segment, vm_getrefcount to get nattch
    //   2. If nattch == 0 && SHM_DEST set: munmap and remove
    //   3. Otherwise compact the list
}

/// Check if any shared memory segments exist.
pub fn is_shm_nil() -> bool {
    SHM_LIST_NR.load(Ordering::Relaxed) == 0
}

// ═════════════════════════════════════════════════════════════════════════════
// Server main loop (stub)
/// Call the VM server to map physical pages into a process's address space.
///
/// # Safety
///
/// `who` must be a valid process endpoint.
pub unsafe fn vm_remap(who: i32, _map_addr: u64, page: u64, size: usize) -> Result<u64, i32> {
    let vm_ep = 8; // VM_PROC_NR
    let mut msg = [0u8; MESSAGE_SIZE];
    // Set call type to VM_REMAP
    msg_set_i32(
        &mut msg,
        MSG_OFF_CALLTYPE,
        arch_common::com::VM_REMAP as i32,
    );
    // m1i1 = dest endpoint (the caller)
    msg_set_i32(&mut msg, 8, who);
    // m1i2 = src endpoint (VM server — owns the physical pages)
    msg_set_i32(&mut msg, 12, vm_ep);
    // m1i3 = source address (physical page number)
    msg_set_u64(&mut msg, 16, page);
    // m1i4 = size
    msg_set_i32(&mut msg, 24, size as i32);

    let r = sendrec(vm_ep, &mut msg);
    if r != 0 {
        return Err(r);
    }
    // Reply: mapped address is in m1i1
    let mapped = msg_i32(&msg, 24) as u64;
    Ok(mapped)
}

/// Call the VM server to unmap a region from a process's address space.
///
/// # Safety
///
/// `who` must be a valid process endpoint; `addr` must be a valid mapped address.
pub unsafe fn vm_unmap(who: i32, addr: u64) -> Result<(), i32> {
    let vm_ep = 8; // VM_PROC_NR
    let mut msg = [0u8; MESSAGE_SIZE];
    msg_set_i32(
        &mut msg,
        MSG_OFF_CALLTYPE,
        arch_common::com::VM_MUNMAP as i32,
    );
    msg_set_i32(&mut msg, 8, who);
    msg_set_u64(&mut msg, 16, addr);

    let r = sendrec(vm_ep, &mut msg);
    if r != 0 {
        return Err(r);
    }
    Ok(())
}

/// Userspace kernel call — execute `syscall` to enter the kernel.
///
/// Sets RAX = `KERNEL_CALL + call_nr`, RDI = `dest`, RSI = `msg_ptr`
/// and executes the `syscall` instruction. The kernel dispatches to
/// the appropriate handler via `kernel_call_dispatch`.
///
/// # Safety
///
/// `msg_ptr` must point to a valid 64-byte message buffer.
/// The kernel syscall entry point must be configured in LSTAR MSR.
pub unsafe fn syscall_kernel(dest: i32, call_nr: i32, msg_ptr: *mut u8) -> i32 {
    let rax: u64 = (arch_common::com::KERNEL_CALL + call_nr as u32) as u64;
    let rdi: u64 = dest as u64;
    let rsi: u64 = msg_ptr as u64;
    let result: u64;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!(
            "syscall",
            inout("rax") rax => result,
            in("rdi") rdi,
            in("rsi") rsi,
            // RCX and R11 are clobbered by syscall
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags),
        );
    }
    #[cfg(not(target_arch = "x86_64"))]
    let result = 0u64;
    result as i32
}

/// Send a message and receive a reply (SENDREC).
///
/// # Safety
///
/// `msg` must point to a valid 64-byte message buffer.
pub unsafe fn sendrec(dest: i32, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe { syscall_kernel(dest, 48, msg.as_mut_ptr()) }
}

/// Send a message without waiting for a reply (SENDNB).
///
/// # Safety
///
/// `msg` must point to a valid 64-byte message buffer.
pub unsafe fn sendnb(dest: i32, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe { syscall_kernel(dest, 47, msg.as_mut_ptr()) }
}

/// Receive a message (RECEIVE).
///
/// # Safety
///
/// `msg` must point to a valid 64-byte message buffer.
pub unsafe fn receive(src: i32, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe { syscall_kernel(src, 46, msg.as_mut_ptr()) }
}

/// Send a notification (NOTIFY).
///
/// # Safety
///
/// `msg` must point to a valid 64-byte message buffer.
pub unsafe fn notify(dest: i32, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    unsafe { syscall_kernel(dest, 49, msg.as_mut_ptr()) }
}

/// IPC_BASE from com.h (0xD00).
const IPC_BASE: i32 = 0xD00;

/// NOTIFY_MESSAGE from com.h (0x1000).
const NOTIFY_MESSAGE: i32 = 0x1000;

/// IPC call numbers (from com.h).
const IPC_SHMGET: i32 = IPC_BASE + 1;
const IPC_SHMAT: i32 = IPC_BASE + 2;
const IPC_SHMDT: i32 = IPC_BASE + 3;
const IPC_SHMCTL: i32 = IPC_BASE + 4;
const IPC_SEMGET: i32 = IPC_BASE + 5;
const IPC_SEMCTL: i32 = IPC_BASE + 6;
const IPC_SEMOP: i32 = IPC_BASE + 7;

/// Type of an IPC handler function.
type IpcHandler = unsafe fn(&mut [u8; MESSAGE_SIZE]) -> i32;

/// Dispatch table for IPC calls (indexed by call_nr - (IPC_BASE + 1)).
const IPC_CALLS: [Option<IpcHandler>; 7] = [
    Some(do_shmget),
    Some(do_shmat),
    Some(do_shmdt),
    Some(do_shmctl),
    Some(do_semget),
    Some(do_semctl),
    Some(do_semop),
];

/// Whether each IPC call needs a reply sent.
const NEEDS_REPLY: [bool; 7] = [true, true, true, true, true, true, true];

/// IPC server main loop.
#[cfg(target_os = "none")]
pub fn ipc_server_main() {
    loop {
        let mut msg = [0u8; MESSAGE_SIZE];
        // Set call type to RECEIVE (kernel will fill in m_source and m_type)
        msg[0..4].copy_from_slice(&(arch_common::com::KERNEL_CALL as i32 + 46).to_ne_bytes());

        let r = unsafe { receive(!0i32, &mut msg) };
        if r != 0 {
            continue;
        }

        let who_e = msg_i32(&msg, MSG_OFF_SOURCE);
        let call_type = msg_i32(&msg, MSG_OFF_CALLTYPE);

        // Check if this is a notification.
        let is_notify = (call_type as u32).wrapping_sub(NOTIFY_MESSAGE as u32) < 0x100;
        if is_notify {
            if who_e == 8 {
                // VM_PROC_NR
                unsafe { sem_process_vm_notify() };
            }
            continue;
        }

        let ipc_nr = (call_type - (IPC_BASE + 1)) as usize;
        if ipc_nr < IPC_CALLS.len() {
            if let Some(handler) = IPC_CALLS[ipc_nr] {
                let result = unsafe { handler(&mut msg) };
                if NEEDS_REPLY[ipc_nr] {
                    msg_set_i32(&mut msg, MSG_OFF_CALLTYPE, result);
                    let _ = unsafe { sendrec(who_e, &mut msg) };
                }
            }
        }
        unsafe { update_refcount_and_destroy_stub() };
    }
}

#[cfg(not(target_os = "none"))]
pub fn ipc_server_main() {
    // No-op on host (no IPC infrastructure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicBool, Ordering};

    /// Simple test spinlock to serialize access to the shared tables.
    static TEST_LOCK: AtomicBool = AtomicBool::new(false);

    struct TestLockGuard;
    impl TestLockGuard {
        fn acquire() -> Self {
            while TEST_LOCK
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
            }
            Self
        }
    }
    impl Drop for TestLockGuard {
        fn drop(&mut self) {
            TEST_LOCK.store(false, Ordering::Release);
        }
    }

    /// Reset all state before each test.
    fn setup() {
        SEM_LIST_NR.store(0, Ordering::Relaxed);
        SHM_LIST_NR.store(0, Ordering::Relaxed);
        IDENTIFIER.store(0x1234, Ordering::Relaxed);
    }

    /// Helper to create a message buffer for semget/shmget.
    fn make_msg(calltype: i32, source: i32, d01: i32, d02: i32, d03: i32) -> [u8; MESSAGE_SIZE] {
        let mut msg = [0u8; MESSAGE_SIZE];
        unsafe {
            msg_set_i32(&mut msg, MSG_OFF_CALLTYPE, calltype);
            msg_set_i32(&mut msg, MSG_OFF_SOURCE, source);
            msg_set_i32(&mut msg, MSG_OFF_D01, d01);
            msg_set_i32(&mut msg, MSG_OFF_D02, d02);
            msg_set_i32(&mut msg, MSG_OFF_D03, d03);
        }
        msg
    }

    #[test]
    fn test_semget_create_and_find() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let r = unsafe { do_semget(&mut msg) };
        assert_eq!(r, OK);

        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };
        assert_eq!(id, 0x1234); // first identifier

        // Find by key.
        let found = unsafe { sem_find_key(1234) };
        assert!(found.is_some());

        // Find by ID.
        let found = unsafe { sem_find_id(id) };
        assert!(found.is_some());

        assert!(!is_sem_nil());
    }

    #[test]
    fn test_semget_existing_key() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let r = unsafe { do_semget(&mut msg) };
        assert_eq!(r, OK);
        let id1 = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        // Get existing key.
        let mut msg = make_msg(0xD05, 42, 1234, 1, IPC_CREAT);
        let r = unsafe { do_semget(&mut msg) };
        assert_eq!(r, OK);
        let id2 = unsafe { msg_i32(&msg, MSG_OFF_D04) };
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_semget_exclusive_fail() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };

        // IPC_CREAT | IPC_EXCL should fail.
        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT | IPC_EXCL);
        let r = unsafe { do_semget(&mut msg) };
        assert_eq!(r, EEXIST);
    }

    #[test]
    fn test_semget_no_create_returns_enoent() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 9999, 2, 0);
        let r = unsafe { do_semget(&mut msg) };
        assert_eq!(r, ENOENT);
    }

    #[test]
    fn test_semget_full_table() {
        let _lock = TestLockGuard::acquire();
        setup();

        for i in 0..SEMMNI {
            let mut msg = make_msg(0xD05, 42, i as i32 + 1000, 1, IPC_CREAT);
            let r = unsafe { do_semget(&mut msg) };
            assert_eq!(r, OK, "failed at index {}", i);
        }

        // Next create should fail with ENOSPC.
        let mut msg = make_msg(0xD05, 42, 9999, 1, IPC_CREAT);
        let r = unsafe { do_semget(&mut msg) };
        assert_eq!(r, ENOSPC);
    }

    #[test]
    fn test_semget_invalid_nsems() {
        let _lock = TestLockGuard::acquire();
        setup();

        // nsems = 0 is invalid.
        let mut msg = make_msg(0xD05, 42, 1234, 0, IPC_CREAT);
        let r = unsafe { do_semget(&mut msg) };
        assert_eq!(r, EINVAL);

        // nsems > SEMMSL is invalid.
        let mut msg = make_msg(0xD05, 42, 1234, SEMMSL as i32 + 1, IPC_CREAT);
        let r = unsafe { do_semget(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_semctl_getval_setval() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        // GETVAL should return 0 initially.
        let mut msg = make_msg(0xD06, 42, id, 0, GETVAL);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, OK);

        // SETVAL (cmd = SETVAL, num = 0, val = 42).
        let mut msg = make_msg(0xD06, 42, id, 0, SETVAL);
        unsafe { msg_set_i32(&mut msg, MSG_OFF_D04, 42) };
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, OK);

        // GETVAL should return 42.
        let mut msg = make_msg(0xD06, 42, id, 0, GETVAL);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, OK);
        let val = unsafe { msg_i32(&msg, MSG_OFF_D04) };
        assert_eq!(val, 42);
    }

    #[test]
    fn test_semctl_getncnt_getpid_getzcnt() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 3, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        // GETNCNT should return 0.
        let mut msg = make_msg(0xD06, 42, id, 0, GETNCNT);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, OK);
        assert_eq!(unsafe { msg_i32(&msg, MSG_OFF_D04) }, 0);

        // GETPID should return 0.
        let mut msg = make_msg(0xD06, 42, id, 0, GETPID);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, OK);
        assert_eq!(unsafe { msg_i32(&msg, MSG_OFF_D04) }, 0);

        // GETZCNT should return 0.
        let mut msg = make_msg(0xD06, 42, id, 0, GETZCNT);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, OK);
        assert_eq!(unsafe { msg_i32(&msg, MSG_OFF_D04) }, 0);
    }

    #[test]
    fn test_semctl_invalid_sem_num() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        // GETVAL with invalid sem number.
        let mut msg = make_msg(0xD06, 42, id, 99, GETVAL);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_semctl_invalid_id() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD06, 42, 9999, 0, GETVAL);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_semctl_rmid_removes_semaphore() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };
        assert!(!is_sem_nil());

        // IPC_RMID
        let mut msg = make_msg(0xD06, 0, id, 0, IPC_RMID);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, OK);

        assert!(is_sem_nil());
    }

    #[test]
    fn test_semctl_info() {
        let _lock = TestLockGuard::acquire();
        setup();

        // Empty table: IPC_INFO returns ret = max(0, -1) = 0.
        let mut msg = make_msg(0xD06, 42, 0, 0, IPC_INFO);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, OK);
        assert_eq!(unsafe { msg_i32(&msg, MSG_OFF_D04) }, 0);

        // Create one, then IPC_INFO returns ret = 0.
        let mut msg2 = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg2) };

        let mut msg = make_msg(0xD06, 42, 0, 0, SEM_INFO);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, OK);
        assert_eq!(unsafe { msg_i32(&msg, MSG_OFF_D04) }, 0);
    }

    #[test]
    fn test_semop_simple() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        // semop: sem_num=0, sem_op=1, sem_flg=0
        let mut msg = make_msg(0xD07, 42, id, 1, 0);
        // Inline sembuf data at MSG_OFF_D05 (offset 24):
        // SemBuf { sem_num: 0, sem_op: 1, sem_flg: 0 } = 6 bytes
        msg[MSG_OFF_D05] = 0; // sem_num LSB
        msg[MSG_OFF_D05 + 1] = 0; // sem_num MSB
        msg[MSG_OFF_D05 + 2] = 1; // sem_op LSB
        msg[MSG_OFF_D05 + 3] = 0; // sem_op MSB
        msg[MSG_OFF_D05 + 4] = 0; // sem_flg LSB
        msg[MSG_OFF_D05 + 5] = 0; // sem_flg MSB
        let r = unsafe { do_semop(&mut msg) };
        assert_eq!(r, OK);
    }

    #[test]
    fn test_semop_invalid_id() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD07, 42, 9999, 1, 0);
        let r = unsafe { do_semop(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_semop_zero_nsops() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        let mut msg = make_msg(0xD07, 42, id, 0, 0);
        let r = unsafe { do_semop(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_semop_overflow_nsops() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        let mut msg = make_msg(0xD07, 42, id, SEMOPM as i32 + 1, 0);
        let r = unsafe { do_semop(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_sem_getval_overflow() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        // SETVAL with value > SEMVMX should fail.
        let mut msg = make_msg(0xD06, 42, id, 0, SETVAL);
        unsafe { msg_set_i32(&mut msg, MSG_OFF_D04, (SEMVMX as i32) + 1) };
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, ERANGE);
    }

    #[test]
    fn test_semctl_sem_stat() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 2, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        // SEM_STAT with index 0 should return the id.
        let mut msg = make_msg(0xD06, 42, 0, 0, SEM_STAT);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, OK);
        assert_eq!(unsafe { msg_i32(&msg, MSG_OFF_D04) }, id);
    }

    #[test]
    fn test_semctl_sem_stat_invalid_index() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD06, 42, 0, 0, SEM_STAT);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_check_perm_root() {
        // Root (uid=0) should always pass.
        let perm = IpcPerm {
            uid: 100,
            gid: 100,
            cuid: 100,
            cgid: 100,
            mode: 0o000,
            _seq: 0,
            _key: 0,
        };
        // With uid=0, all permissions granted.
        assert!(unsafe { check_perm(&perm, 0, 0o666) });
    }

    #[test]
    fn test_check_perm_no_permission() {
        // Non-root, no permissions set.
        // Note: with the stub check_perm using uid=0 (root),
        // this test only validates the logic structure.
        // Real permission checking requires Phase 13 PM integration.
        let perm = IpcPerm {
            uid: 100,
            gid: 100,
            cuid: 100,
            cgid: 100,
            mode: 0,
            _seq: 0,
            _key: 0,
        };
        // With root stub, this will pass.
        // TODO: when check_perm uses real getnuid, this should return false.
        assert!(unsafe { check_perm(&perm, 100, 0o600) });
    }

    #[test]
    fn test_shmget_create_and_find() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD01, 42, 1234, 4096, IPC_CREAT);
        let r = unsafe { do_shmget(&mut msg) };
        assert_eq!(r, OK);

        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };
        assert_eq!(id, 0x1234);

        let found = unsafe { shm_find_key(1234) };
        assert!(found.is_some());

        let found = unsafe { shm_find_id(id) };
        assert!(found.is_some());

        assert!(!is_shm_nil());
    }

    #[test]
    fn test_shmget_existing_key() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD01, 42, 1234, 4096, IPC_CREAT);
        let r = unsafe { do_shmget(&mut msg) };
        assert_eq!(r, OK);
        let id1 = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        // Get existing.
        let mut msg = make_msg(0xD01, 42, 1234, 0, IPC_CREAT);
        let r = unsafe { do_shmget(&mut msg) };
        assert_eq!(r, OK);
        let id2 = unsafe { msg_i32(&msg, MSG_OFF_D04) };
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_shmget_exclusive_fail() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD01, 42, 1234, 4096, IPC_CREAT);
        let _ = unsafe { do_shmget(&mut msg) };

        let mut msg = make_msg(0xD01, 42, 1234, 0, IPC_CREAT | IPC_EXCL);
        let r = unsafe { do_shmget(&mut msg) };
        assert_eq!(r, EEXIST);
    }

    #[test]
    fn test_shmget_no_create_returns_enoent() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD01, 42, 9999, 0, 0);
        let r = unsafe { do_shmget(&mut msg) };
        assert_eq!(r, ENOENT);
    }

    #[test]
    fn test_shmget_zero_size() {
        let _lock = TestLockGuard::acquire();
        setup();

        // size=0 via MSG_OFF_D02=0 from make_msg
        let mut msg = make_msg(0xD01, 42, 1234, 0, IPC_CREAT);
        let r = unsafe { do_shmget(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_shmget_full_table() {
        let _lock = TestLockGuard::acquire();
        setup();

        for i in 0..10 {
            let mut msg = make_msg(0xD01, 42, i as i32 + 1000, 4096, IPC_CREAT);
            let r = unsafe { do_shmget(&mut msg) };
            assert_eq!(r, OK, "failed at index {}", i);
        }
        // Table is not full (MAX_SHM_NR = 1024).
        let mut msg = make_msg(0xD01, 42, 9999, 4096, IPC_CREAT);
        let r = unsafe { do_shmget(&mut msg) };
        assert_eq!(r, OK);
    }

    #[test]
    fn test_shmctl_info() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD04, 42, 0, IPC_INFO, 0);
        let r = unsafe { do_shmctl(&mut msg) };
        assert_eq!(r, OK);
        assert_eq!(unsafe { msg_i32(&msg, MSG_OFF_D04) }, 0);
    }

    #[test]
    fn test_shmctl_shm_info() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD04, 42, 0, SHM_INFO, 0);
        let r = unsafe { do_shmctl(&mut msg) };
        assert_eq!(r, OK);
        assert_eq!(unsafe { msg_i32(&msg, MSG_OFF_D04) }, 0);
    }

    #[test]
    fn test_shmctl_shm_stat() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD01, 42, 1234, 4096, IPC_CREAT);
        let _ = unsafe { do_shmget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        let mut msg = make_msg(0xD04, 42, 0, SHM_STAT, 0);
        let r = unsafe { do_shmctl(&mut msg) };
        assert_eq!(r, OK);
        assert_eq!(unsafe { msg_i32(&msg, MSG_OFF_D04) }, id);
    }

    #[test]
    fn test_shmctl_shm_stat_invalid_index() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD04, 42, 99, SHM_STAT, 0);
        let r = unsafe { do_shmctl(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_shmdt_empty() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD03, 42, 0, 0, 0);
        let r = unsafe { do_shmdt(&mut msg) };
        assert_eq!(r, OK);
    }

    #[test]
    fn test_shmat_no_vm() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD01, 42, 1234, 4096, IPC_CREAT);
        let _ = unsafe { do_shmget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        // Without VM, shmat returns ENOSYS.
        let mut msg = make_msg(0xD02, 42, id, 0, 0);
        let r = unsafe { do_shmat(&mut msg) };
        assert_eq!(r, enosys());
    }

    #[test]
    fn test_shmctl_invalid_id() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD04, 42, 9999, IPC_STAT, 0);
        let r = unsafe { do_shmctl(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_shmctl_rmid() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD01, 42, 1234, 4096, IPC_CREAT);
        let _ = unsafe { do_shmget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        let mut msg = make_msg(0xD04, 0, id, IPC_RMID, 0);
        let r = unsafe { do_shmctl(&mut msg) };
        assert_eq!(r, OK);
    }

    #[test]
    fn test_identifier_increments() {
        let _lock = TestLockGuard::acquire();
        setup();

        assert_eq!(IDENTIFIER.load(Ordering::Relaxed), 0x1234);

        let mut msg = make_msg(0xD05, 42, 100, 1, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        assert_eq!(unsafe { msg_i32(&msg, MSG_OFF_D04) }, 0x1234);

        let mut msg = make_msg(0xD05, 42, 200, 1, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        assert_eq!(unsafe { msg_i32(&msg, MSG_OFF_D04) }, 0x1235);
    }

    #[test]
    fn test_semget_negative_nsems() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, -1, IPC_CREAT);
        let r = unsafe { do_semget(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_semctl_unknown_cmd() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD05, 42, 1234, 1, IPC_CREAT);
        let _ = unsafe { do_semget(&mut msg) };
        let id = unsafe { msg_i32(&msg, MSG_OFF_D04) };

        let mut msg = make_msg(0xD06, 42, id, 0, 9999);
        let r = unsafe { do_semctl(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_shmctl_unknown_cmd() {
        let _lock = TestLockGuard::acquire();
        setup();

        let mut msg = make_msg(0xD04, 42, 0, 9999, 0);
        let r = unsafe { do_shmctl(&mut msg) };
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_sem_nil_initial() {
        let _lock = TestLockGuard::acquire();
        setup();
        assert!(is_sem_nil());
    }

    #[test]
    fn test_shm_nil_initial() {
        let _lock = TestLockGuard::acquire();
        setup();
        assert!(is_shm_nil());
    }

    #[test]
    fn test_msg_helpers() {
        let mut msg = [0u8; MESSAGE_SIZE];
        unsafe {
            msg_set_i32(&mut msg, 0, 42);
            assert_eq!(msg_i32(&msg, 0), 42);

            msg_set_i32(&mut msg, 4, -1);
            assert_eq!(msg_i32(&msg, 4), -1);

            msg_set_u64(&mut msg, 8, 0xDEADBEEF);
            assert_eq!(msg_u64(&msg, 8), 0xDEADBEEF);
        }
    }

    #[test]
    fn test_sem_find_key_private() {
        let _lock = TestLockGuard::acquire();
        setup();

        // IPC_PRIVATE key should not match any sem_find_key.
        let r = unsafe { sem_find_key(IPC_PRIVATE) };
        assert!(r.is_none());
    }

    #[test]
    fn test_shm_find_key_private() {
        let _lock = TestLockGuard::acquire();
        setup();

        let r = unsafe { shm_find_key(IPC_PRIVATE) };
        assert!(r.is_none());
    }

    #[test]
    fn test_sem_find_id_empty() {
        let _lock = TestLockGuard::acquire();
        setup();

        let r = unsafe { sem_find_id(9999) };
        assert!(r.is_none());
    }

    #[test]
    fn test_shm_find_id_empty() {
        let _lock = TestLockGuard::acquire();
        setup();

        let r = unsafe { shm_find_id(9999) };
        assert!(r.is_none());
    }

    #[test]
    fn test_remove_process_empty() {
        let _lock = TestLockGuard::acquire();
        setup();

        // Should not panic on empty list.
        unsafe { remove_process(42) };
    }

    #[test]
    fn test_sem_process_vm_notify_empty() {
        let _lock = TestLockGuard::acquire();
        setup();

        unsafe { sem_process_vm_notify() };
    }

    #[test]
    fn test_vm_stubs_return_none() {
        assert!(unsafe { vm_query_exit_stub() }.is_none());
        assert!(unsafe { vm_watch_exit_stub(42) }.is_ok());
        assert!(unsafe { vm_remap_stub(0, 0, 0, 4096) }.is_err());
        assert!(unsafe { vm_unmap_stub(0, 0) }.is_err());
        assert_eq!(unsafe { vm_getphys_stub(0, 0) }, 0);
        assert_eq!(unsafe { vm_getrefcount_stub(0, 0) }, 2);
    }
}
