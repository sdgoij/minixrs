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
//! The IPC message loop runs, and `tools/wasm-servers` drives it. It also hands
//! DS the public process table — C's `RS_INIT` handshake, where `rproctab_gid`
//! is a read-only grant over `rprocpub` — which is what seeds DS's label table and
//! lets a service publish at all (Phase 12.4). DS answers that request with its
//! own `RS_INIT` result, which `do_init_ready` consumes: the slot RS put into
//! `RS_INITIALIZING` when it sent the request becomes `RS_ACTIVE`.
//!
//! DS is the only service this port sends an init request to, so it is the only
//! one whose slot waits for a reply — the other boot services are marked active at
//! init because nothing asked them to initialise. C sends a request to every
//! service it starts, which is what a runtime-start path will need before those
//! services can report ready.
//! All service table management and lookup functions are fully implemented.

#![allow(dead_code, clippy::missing_safety_doc)]

use arch_common::ipc::{EDONTREPLY, Message};
use arch_common::safecopies::{
    CPF_DIRECT, CPF_READ, CPF_USED, CPF_VALID, CpDirect, CpGrant, CpUnion, GRANTEE_ANY,
};

// Constants

/// `sef_init_info_t`'s init type for a fresh start (`SEF_INIT_FRESH`).
const SEF_INIT_FRESH: i32 = 0;

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

// RS call numbers.
//
// Taken from `arch_common::com`, which carries C's `com.h` values, rather than
// kept as a second literal list: the port's local copy had drifted — `RS_INIT`
// was 0x70A where C has 0x714, and `RS_LOOKUP`/`RS_GETSYSINFO` were transposed —
// so a request bearing C's number arrived at RS as an unknown call and got
// ENOSYS. The i32 cast is because the dispatch matches on the message's `m_type`.
pub const RS_UP: i32 = arch_common::com::RS_UP as i32;
pub const RS_DOWN: i32 = arch_common::com::RS_DOWN as i32;
pub const RS_REFRESH: i32 = arch_common::com::RS_REFRESH as i32;
pub const RS_RESTART: i32 = arch_common::com::RS_RESTART as i32;
pub const RS_SHUTDOWN: i32 = arch_common::com::RS_SHUTDOWN as i32;
pub const RS_UPDATE: i32 = arch_common::com::RS_UPDATE as i32;
pub const RS_CLONE: i32 = arch_common::com::RS_CLONE as i32;
pub const RS_EDIT: i32 = arch_common::com::RS_EDIT as i32;
pub const RS_GETSYSINFO: i32 = arch_common::com::RS_GETSYSINFO as i32;
pub const RS_LOOKUP: i32 = arch_common::com::RS_LOOKUP as i32;
pub const RS_INIT: i32 = arch_common::com::RS_INIT as i32;
pub const RS_LU_PREPARE: i32 = arch_common::com::RS_LU_PREPARE as i32;

const ESRCH: i32 = -3;
const EEXIST: i32 = -17;

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

// ---- The public process table grant ----
//
// DS seeds its label table from `RPROCPUB`, and C's RS hands the table over as a
// read-only direct grant over its own `rprocpub` — `sef_cb_init_fresh`:
// `cpf_grant_direct(ANY, (vir_bytes) rprocpub, sizeof(rprocpub), CPF_READ)` —
// with the grant's id travelling in the `RS_INIT` message `init_service` sends.

/// The registered grant table. One entry, because the whole table is one range.
const NR_RPROCTAB_GRANTS: usize = 1;

struct RproctabGrantRaw(UnsafeCell<[CpGrant; NR_RPROCTAB_GRANTS]>);
unsafe impl Sync for RproctabGrantRaw {}

impl RproctabGrantRaw {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [const {
                CpGrant {
                    cp_flags: 0,
                    cp_u: CpUnion {
                        cp_direct: CpDirect {
                            cp_who_to: 0,
                            cp_start: 0,
                            cp_len: 0,
                            cp_reserved: [0u8; 8],
                        },
                    },
                    cp_reserved: [0u8; 8],
                }
            }; NR_RPROCTAB_GRANTS],
        ))
    }

    fn as_ptr(&self) -> *mut CpGrant {
        self.0.get() as *mut CpGrant
    }

    /// Address of the table, which is what `SYS_SETGRANT` is given.
    fn table_addr(&self) -> u64 {
        self.as_ptr() as u64
    }
}

static RPROCTAB_GRANT: RproctabGrantRaw = RproctabGrantRaw::new();

/// Give the single grant entry read-only access to the whole [`RPROCPUB`] table
/// and return its id (always 0).
///
/// The grantee is the wildcard, as in C: the table is handed to whichever
/// service asks for it in the `RS_INIT` message, not to a named one.
pub unsafe fn build_rproctab_grant() -> i32 {
    let entry = unsafe { &mut *RPROCTAB_GRANT.as_ptr() };
    entry.cp_flags = CPF_USED | CPF_VALID | CPF_DIRECT | CPF_READ;
    entry.cp_u.cp_direct = CpDirect {
        cp_who_to: GRANTEE_ANY,
        cp_start: RPROCPUB.as_ptr() as u64,
        cp_len: core::mem::size_of::<[RprocPub; NR_SYS_PROCS]>(),
        cp_reserved: [0u8; 8],
    };
    0
}

/// The `RS_INIT` message C's `init_service` sends to a service it starts.
///
/// The grant over [`RPROCPUB`] travels in `mess_rs_init.rproctab_gid`, which C
/// lays out payload-relative at offset 8 (`result@0, type@4, rproctab_gid@8`), so
/// in this port's `Message` it is `m2i3`.
fn rproctab_init_msg(gid: i32) -> Message {
    let mut msg = Message {
        m_source: 0,
        m_type: RS_INIT,
        // SAFETY: the payload is a union of plain integers and byte arrays.
        m_payload: unsafe { core::mem::zeroed() },
    };
    msg.m_payload.m2.m2i2 = SEF_INIT_FRESH;
    msg.m_payload.m2.m2i3 = gid;
    msg
}

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

/// Mark a service as initializing, so its init reply is expected and a second
/// one is refused. C's `init_service`: `rp->r_flags |= RS_INITIALIZING` before the
/// `RS_INIT` request goes out.
pub unsafe fn mark_initializing(idx: usize) -> Result<(), i32> {
    if idx >= NR_SYS_PROCS {
        return Err(EINVAL);
    }
    let base = RPROC.as_ptr();
    let rp = unsafe { &mut *base.add(idx) };
    if rp.flags & RS_IN_USE == 0 {
        return Err(EINVAL);
    }
    rp.flags |= RS_INITIALIZING;
    rp.flags &= !RS_ACTIVE;
    Ok(())
}

/// Whether `endpoint`'s slot is active and no longer initializing — the state a
/// consumed init reply is supposed to produce.
pub unsafe fn is_active(endpoint: i32) -> bool {
    match unsafe { lookup_slot_by_endpoint(endpoint) } {
        Some(idx) => {
            let flags = unsafe { (*RPROC.as_ptr().add(idx)).flags };
            flags & RS_ACTIVE != 0 && flags & RS_INITIALIZING == 0
        }
        None => false,
    }
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

/// Publish a service's label to DS.
///
/// This is C's `publish_service`, which calls `ds_publish_label(rpub->label,
/// rpub->endpoint, DSF_OVERWRITE)`. It matters because a service DS cannot name
/// is refused every publish — the label table is the whole of what authorises a
/// writer — so without this a service registered at runtime stays mute.
///
/// Returns 0, or the errno the publish failed with.
#[cfg(target_os = "minix")]
unsafe fn publish_service(idx: usize) -> i32 {
    let pub_base = RPROCPUB.as_ptr();
    let rpub = unsafe { &*pub_base.add(idx) };
    let label_len = rpub
        .label
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(RS_MAX_LABEL_LEN);

    let mut msg = Message {
        m_source: 0,
        m_type: arch_common::com::DS_PUBLISH as i32,
        // SAFETY: the payload is a union of plain integers and byte arrays.
        m_payload: unsafe { core::mem::zeroed() },
    };
    // DS's `DS_PUBLISH` layout: key pointer in `m2l1`, its length in `m2i1`, the
    // type flags in `m2i3`, and the label's endpoint in `m2l2`.
    msg.m_payload.m2.m2i1 = label_len as i32;
    msg.m_payload.m2.m2i3 = (crate::ds::DSF_TYPE_LABEL | crate::ds::DSF_OVERWRITE) as i32;
    msg.m_payload.m2.m2l1 = rpub.label.as_ptr() as i64;
    msg.m_payload.m2.m2l2 = rpub.endpoint as i64;

    let r = unsafe {
        minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            arch_common::com::DS_PROC_NR as u64,
            &mut msg as *mut Message as u64,
        )
    };
    if r < 0 {
        return r as i32;
    }
    msg.m_type
}

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

    // Tell DS, as C's `create_service` does through `publish_service`. A service
    // DS cannot name may not publish anything, so leaving this out would register
    // the service everywhere except the one place that authorises it.
    #[cfg(target_os = "minix")]
    {
        let r = unsafe { publish_service(slot) };
        if r != OK {
            return r;
        }
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

/// Service reports initialization complete (`RS_INIT`).
///
/// C's `do_init_ready`. A service answers the `RS_INIT` request RS sent it — for
/// DS that request carries the `rproctab` grant — with its own result, and the
/// reply is what moves the slot out of `RS_INITIALIZING`. A reply from a slot RS
/// never asked to initialise is `EINVAL`, and a service that reports a *failed*
/// init is treated as crashed (C's `crash_service`, whose restart policy this port
/// does not have yet: the slot is marked terminated and left for it). That last
/// case is why the caller gets no reply — `EDONTREPLY` keeps RS's loop silent,
/// because the sender is being killed rather than answered.
unsafe fn do_init_ready(msg: &Message) -> i32 {
    let endpoint = msg.m_source;
    // The payload's first word is `mess_rs_init.result`, which the service's
    // init-response callback writes (C `sef_init.c`).
    // SAFETY: reading a `Payload` union field is unsafe; `m2i1` is the first word
    // of the payload and the message is a live value, so the read is in bounds.
    let result = unsafe { msg.m_payload.m2.m2i1 };

    // RS has no init request to answer, so a failure result here is RS's own.
    if endpoint == arch_common::com::RS_PROC_NR && result != OK {
        return result;
    }

    let slot = match unsafe { lookup_slot_by_endpoint(endpoint) } {
        Some(slot) => slot,
        None => return ESRCH,
    };

    let flags = unsafe { (*RPROC.as_ptr().add(slot)).flags };
    if flags & RS_INITIALIZING == 0 {
        return EINVAL;
    }

    if result != OK {
        unsafe { mark_terminated(slot) };
        return EDONTREPLY;
    }

    unsafe { mark_initialized(slot, endpoint) }.map_or_else(|e| e, |_| OK)
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
        // IPC syscall numbers.
        const RECEIVE_CALL: u64 = 47;
        const SENDNB_CALL: u64 = 51;
        const ANY: i32 = 0x0000ffff;

        // Initialize RS's process table.
        unsafe {
            rs_init();
        }

        // Register boot services with their known endpoints.
        //
        // This is the list RS can *see*: `lookup_slot_by_endpoint` scans these
        // entries and nothing else, so a service that is missing here cannot be
        // asked to initialise — the lookup returns `None` and RS panics before it
        // gets that far. The RAM disk driver and the virtio block driver were the
        // two the wasm work spawned without adding here, which is what made
        // extending `asked` panic rather than ask (finding 26).
        let boot_svcs: &[(i32, &[u8])] = &[
            (arch_common::com::DS_PROC_NR, b"ds"),
            (arch_common::com::RS_PROC_NR, b"rs"),
            (arch_common::com::PM_PROC_NR, b"pm"),
            (arch_common::com::SCHED_PROC_NR, b"sched"),
            (arch_common::com::VFS_PROC_NR, b"vfs"),
            (arch_common::com::VM_PROC_NR, b"vm"),
            (arch_common::com::TTY_PROC_NR, b"tty"),
            (arch_common::com::MFS_PROC_NR, b"mfs"),
            (arch_common::com::RAMDISK_PROC_NR, b"ramdisk"),
            (arch_common::com::VIRTIO_BLK_PROC_NR, b"virtio_blk"),
            (arch_common::com::DEVMAN_PROC_NR, b"devman"),
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

        // Hand DS the table, which is what C's `sef_cb_init_fresh` sets up and
        // `init_service` delivers: the table itself is granted once, told to the
        // kernel with `SYS_SETGRANT`, and its grant id travels in the `RS_INIT`
        // message. The order matters — DS copies whatever is in the table when
        // the message arrives, so the services above must be registered first.
        let gid = unsafe { build_rproctab_grant() };
        let mut reg = [0u8; 64];
        reg[8..16].copy_from_slice(&RPROCTAB_GRANT.table_addr().to_ne_bytes());
        reg[16..20].copy_from_slice(&(NR_RPROCTAB_GRANTS as i32).to_ne_bytes());
        if minix_rt::kernel_call(34, &mut reg) != 0 {
            // C panics here too: with no registered table there is no grant for
            // DS to resolve its copy against, so no service could be named.
            panic!("rs: SYS_SETGRANT failed");
        }

        // Ask each service this port starts *and* whose main loop answers an init
        // request, then wait for the answer — C's `init_service` followed by
        // `catch_boot_init_ready`. C gates the wait on `SF_SYNCH_BOOT` in the boot
        // image's privilege flags; this port has no boot-image flags wired, so this
        // table is that gate's stand-in: a service belongs here once its loop answers
        // `RS_INIT`, and RS then blocks for it. The other boot services are marked
        // active at their slot's creation because nothing asks them yet.
        //
        // The `rproctab` grant travels with every request (C's `init_service` sets
        // `rproctab_gid` unconditionally) and only DS reads it.
        let asked: &[i32] = &[
            arch_common::com::DS_PROC_NR,
            arch_common::com::PM_PROC_NR,
            arch_common::com::RAMDISK_PROC_NR,
            arch_common::com::VIRTIO_BLK_PROC_NR,
            arch_common::com::DEVMAN_PROC_NR,
        ];
        for &ep in asked {
            let slot = match unsafe { lookup_slot_by_endpoint(ep) } {
                Some(slot) => slot,
                None => panic!("rs: endpoint {ep} cannot be told to initialize"),
            };
            if let Err(e) = unsafe { mark_initializing(slot) } {
                panic!("rs: cannot put endpoint {ep} into RS_INITIALIZING: {e}");
            }

            // A blocking send, not `asynsend` as in C: the destination is already
            // in the process table, so this is a rendezvous that completes as soon
            // as it reaches its first receive — which makes it independent of which
            // of the two is scheduled first.
            let mut init = rproctab_init_msg(gid);
            let sent = unsafe {
                minix_rt::syscall2(
                    minix_rt::SEND_CALL,
                    ep as u64,
                    &mut init as *mut Message as u64,
                )
            };
            if sent < 0 {
                panic!("rs: RS_INIT to endpoint {ep} failed: {sent}");
            }

            // Wait for the answer here rather than letting it land in the loop, as
            // C's `catch_boot_init_ready` does: RS has not finished initialising
            // until the service it asked has, and blocking for the answer keeps a
            // client's first request from interleaving with the handshake — the
            // service is half-way through its own initiation while its answer is
            // outstanding, and a new request arriving in that window would go to a
            // service that is still waiting to hear back from RS (finding 21).
            //
            // A message from this endpoint that is *not* an init reply is a panic
            // rather than a `continue`: it means the service talked to RS before
            // answering, which is not a shape this wait can absorb.
            let mut reply = Message {
                m_source: 0,
                m_type: 0,
                // SAFETY: the payload is a union of plain integers and byte arrays.
                m_payload: unsafe { core::mem::zeroed() },
            };
            let got = unsafe {
                minix_rt::syscall2(RECEIVE_CALL, ep as u64, &mut reply as *mut Message as u64)
            };
            if got < 0 {
                panic!("rs: waiting for endpoint {ep}'s init reply failed: {got}");
            }
            if reply.m_type != RS_INIT {
                panic!(
                    "rs: unexpected message from endpoint {ep}: {}",
                    reply.m_type
                );
            }
            let result = unsafe { reply.m_payload.m2.m2i1 };
            if result != OK {
                // C panics in `catch_boot_init_ready` for the same reason: a boot
                // service that cannot initialise leaves the system without whatever
                // it was going to provide.
                panic!("rs: endpoint {ep} failed to initialize: {result}");
            }

            // Unblock the service, then record the state change through the same
            // handler the loop uses, so the two paths cannot diverge. C's order too:
            // "Reply and unblock the service before doing anything else."
            reply.m_type = OK;
            unsafe {
                minix_rt::syscall2(SENDNB_CALL, got as u64, &mut reply as *mut Message as u64);
            }
            if unsafe { do_init_ready(&reply) } != OK {
                panic!("rs: endpoint {ep}'s init reply was not accepted");
            }
        }

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

            // Reply to sender, unless the handler suppressed it: C's loop skips
            // the reply for `EDONTREPLY`, which is what the sender of a failed
            // init report gets — it is being killed, not answered. A plain SENDNB
            // (C `reply()` uses `ipc_send`): a SENDREC here would block in its
            // receive phase and swallow the next request instead of dispatching
            // it, which is the same defect DS's loop documents.
            if result != EDONTREPLY {
                msg.m_type = result;
                unsafe {
                    minix_rt::syscall2(SENDNB_CALL, src as u64, &mut msg as *mut Message as u64);
                }
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
        // The request numbers are C's (`com.h`), not the port's old local copy.
        assert_eq!(RS_UP, 0x700);
        assert_eq!(RS_LOOKUP, 0x708);
        assert_eq!(RS_GETSYSINFO, 0x709);
        assert_eq!(RS_INIT, 0x714);
        assert_eq!(RS_LU_PREPARE, 0x715);
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
    fn test_mark_initializing_clears_active() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 106, -1, b"reinit").unwrap();
            mark_initialized(idx, 106).unwrap();

            // The boot handshake's shape: the slot is active, then RS asks it to
            // initialise, so it waits for a reply again.
            mark_initializing(idx).unwrap();
            let rp = &*RPROC.as_ptr().add(idx);
            assert!(rp.flags & RS_INITIALIZING != 0);
            assert!(rp.flags & RS_ACTIVE == 0);
        }
    }

    /// The reply a service builds in `sef_init.c`: the payload's first word is
    /// the init result.
    fn init_reply(source: i32, result: i32) -> Message {
        let mut msg = Message {
            m_source: source,
            m_type: RS_INIT,
            // SAFETY: the payload is a union of plain integers and byte arrays.
            m_payload: unsafe { core::mem::zeroed() },
        };
        msg.m_payload.m2.m2i1 = result;
        msg
    }

    #[test]
    fn test_do_init_ready_completes_initialization() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 102, -1, b"ready").unwrap();

            assert_eq!(do_init_ready(&init_reply(102, 0)), OK);

            let rp = &*RPROC.as_ptr().add(idx);
            assert!(rp.flags & RS_ACTIVE != 0);
            assert!(rp.flags & RS_INITIALIZING == 0);
            assert!(is_active(102));
        }
    }

    #[test]
    fn test_do_init_ready_refuses_a_slot_that_was_not_asked() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 103, -1, b"unexpected").unwrap();
            mark_initialized(idx, 103).unwrap();

            // Already active, so no request is outstanding: C answers EINVAL.
            assert_eq!(do_init_ready(&init_reply(103, 0)), EINVAL);
        }
    }

    #[test]
    fn test_do_init_ready_unknown_service() {
        let _g = setup();
        unsafe {
            assert_eq!(do_init_ready(&init_reply(104, 0)), ESRCH);
        }
    }

    #[test]
    fn test_do_init_ready_failure_terminates_and_suppresses_the_reply() {
        let _g = setup();
        unsafe {
            let idx = alloc_slot().unwrap();
            init_slot(idx, 105, -1, b"broken").unwrap();

            // `EDONTREPLY` is what keeps RS's loop silent, and the slot is left
            // terminated rather than active — the restart policy C's
            // `crash_service` applies is not in this port yet.
            assert_eq!(do_init_ready(&init_reply(105, -5)), EDONTREPLY);

            let rp = &*RPROC.as_ptr().add(idx);
            assert!(rp.flags & RS_TERMINATED != 0);
            assert!(rp.flags & RS_ACTIVE == 0);
            assert!(!is_active(105));
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
