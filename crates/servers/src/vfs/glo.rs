//! VFS global state — adapted from `minix/servers/vfs/glo.h`
//!
//! Singleton wrapped in `UnsafeCell` for interior mutability without
//! `static mut`. All access goes through `vfs_global()` which returns
//! a raw `*mut VfsGlobal`.

use core::cell::UnsafeCell;
use core::ptr::addr_of_mut;

use crate::vfs::consts::*;
use crate::vfs::types::*;

/// Sync wrapper for `UnsafeCell<VfsGlobal>`.
///
/// # Safety
///
/// Single-threaded kernel — no concurrent access. All access is
/// mediated through raw pointers and `unsafe` blocks.
pub struct VfsGlobalCell(UnsafeCell<VfsGlobal>);
unsafe impl Sync for VfsGlobalCell {}

impl VfsGlobalCell {
    pub(crate) const fn new(val: VfsGlobal) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub(crate) fn get(&self) -> *mut VfsGlobal {
        self.0.get()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Global state singleton
// ─────────────────────────────────────────────────────────────────────────────

/// All VFS global state in a single struct.
#[repr(C)]
pub struct VfsGlobal {
    // ── Table arrays ────────────────────────────────────────────────────────
    pub fproc: [Fproc; NR_PROCS],
    pub filp: [Filp; NR_FILPS],
    pub vnode: [Vnode; NR_VNODES],
    pub vmnt: [Vmnt; NR_MNTS],
    pub dmap: [Dmap; NR_DEVICES],
    pub file_lock: [FileLock; NR_LOCKS],
    pub workers: [WorkerThread; NR_WTHREADS],
    pub scratchpad: [Scratchpad; NR_PROCS],

    // ── Per-request state ───────────────────────────────────────────────────
    pub caller_uid: u16,
    pub caller_gid: u16,
    pub req_nr: i32,
    pub fp: *mut Fproc,
    pub err_code: i32,
    pub self_thread: *mut WorkerThread,

    // ── Message buffers ─────────────────────────────────────────────────────
    pub fs_m_in: [u8; 64],
    pub fs_m_out: [u8; 64],

    // ── Flags and counters ──────────────────────────────────────────────────
    pub susp_count: i32,
    pub nr_locks: i32,
    pub reviving: i32,
    pub pending: i32,
    pub sending: i32,
    pub verbose: i32,
    pub deadlock_resolving: i32,
    pub receive_from: i32,

    // ── Device & FS identifiers ─────────────────────────────────────────────
    pub root_dev: u32,
    pub root_fs_e: i32,
    pub system_hz: u32,
    pub mount_label: [u8; LABEL_MAX],
}

// ─────────────────────────────────────────────────────────────────────────────
// Global static
// ─────────────────────────────────────────────────────────────────────────────

pub static VFS_GLOBAL: VfsGlobalCell = VfsGlobalCell::new(VfsGlobal {
    fproc: new_fproc_array(),
    filp: new_filp_array(),
    vnode: new_vnode_array(),
    vmnt: new_vmnt_array(),
    dmap: new_dmap_array(),
    file_lock: new_file_lock_array(),
    workers: new_worker_array(),
    scratchpad: new_scratchpad_array(),
    caller_uid: 0,
    caller_gid: 0,
    req_nr: 0,
    fp: core::ptr::null_mut(),
    err_code: 0,
    self_thread: core::ptr::null_mut(),
    fs_m_in: [0u8; 64],
    fs_m_out: [0u8; 64],
    susp_count: 0,
    nr_locks: 0,
    reviving: 0,
    pending: 0,
    sending: 0,
    verbose: 0,
    deadlock_resolving: 0,
    receive_from: -1, // ANY
    root_dev: 0,
    root_fs_e: -1,
    system_hz: 60,
    mount_label: [0u8; LABEL_MAX],
});

// ─────────────────────────────────────────────────────────────────────────────
// Helper: `const` array constructors
// ─────────────────────────────────────────────────────────────────────────────

const NR_PROCS: usize = 256;

const fn new_fproc_array() -> [Fproc; NR_PROCS] {
    [Fproc {
        fp_flags: 0,
        fp_realuid: 0,
        fp_effuid: 0,
        fp_realgid: 0,
        fp_effgid: 0,
        fp_umask: 0,
        fp_ngroups: 0,
        fp_sgroups: [0; NGROUPS_MAX],
        fp_endpoint: -1,
        fp_pid: 0,
        fp_vminode: 0,
        fp_cdir: 0,
        fp_rdir: 0,
        fp_filp: [-1i32; OPEN_MAX],
        fp_cloexec: 0,
        fp_blocked_on: 0,
        fp_task: -1,
        fp_tty: 0,
        fp_suspended: 0,
        fp_reopen: 0,
        fp_flush_on_wr: 0,
        fp_flush_on_rd: 0,
        fp_sesstype: 0,
        fp_session: 0,
        fp_sessdev: 0,
        fp_exit_signal: 0,
        fp_sesstask: 0,
        fp_suspended_ep: -1,
        fp_susp_owner: core::ptr::null_mut(),
    }; NR_PROCS]
}

const fn new_filp_array() -> [Filp; NR_FILPS] {
    [Filp {
        filp_count: 0,
        filp_flags: 0,
        filp_mode: 0,
        filp_state: 0,
        filp_ino: 0,
        filp_pos: 0,
        filp_selectors: 0,
        filp_select_ops: 0,
        filp_select_flags: 0,
        filp_select_ep: -1,
        filp_pipe_select_ops: 0,
        filp_pipe_select_ep: [-1; 2],
        filp_pipe_ino: 0,
    }; NR_FILPS]
}

const fn new_vnode_array() -> [Vnode; NR_VNODES] {
    [Vnode {
        v_fs: 0,
        v_fs_e: -1,
        v_inode_nr: 0,
        v_mode: 0,
        v_size: 0,
        v_ref_count: 0,
        v_ref_check: 0,
        v_fs_count: 0,
        v_fs_count_check: 0,
        v_smoothed: 0,
        v_pipe: 0,
        v_bfs_e: -1,
        v_dev: 0,
        v_fs_dev: 0,
        v_fs_count_inc: 0,
    }; NR_VNODES]
}

const fn new_vmnt_array() -> [Vmnt; NR_MNTS] {
    [Vmnt {
        m_fs: -1,
        m_dev: 0,
        m_flags: 0,
        m_fs_e: -1,
        m_root_node: 0,
        m_mounted_on: 0,
        m_path: [0u8; PATH_MAX],
        m_label: [0u8; LABEL_MAX],
    }; NR_MNTS]
}

const fn new_dmap_array() -> [Dmap; NR_DEVICES] {
    [Dmap {
        dmap_driver: -1,
        dmap_ep: -1,
        dmap_style: 0,
        dmap_label: [0u8; LABEL_MAX],
    }; NR_DEVICES]
}

const fn new_file_lock_array() -> [FileLock; NR_LOCKS] {
    [FileLock {
        lock_type: 0,
        lock_pid: 0,
        lock_vnode: 0,
        lock_first: 0,
        lock_last: 0,
    }; NR_LOCKS]
}

const fn new_worker_array() -> [WorkerThread; NR_WTHREADS] {
    [WorkerThread {
        w_tid: -1,
        w_flags: 0,
        w_fp: core::ptr::null_mut(),
        w_io_vmnt: core::ptr::null_mut(),
        w_task: -1,
        w_fs_e: -1,
        w_drv_e: -1,
        w_sendrec: 0,
        w_susp: 0,
        w_job_typ: 0,
        w_job_ref_nr: 0,
    }; NR_WTHREADS]
}

const fn new_scratchpad_array() -> [Scratchpad; NR_PROCS] {
    [Scratchpad {
        file: ScratchpadData { fd_nr: -1 },
        io: IoCmd {
            io_buffer: core::ptr::null_mut(),
            io_nbytes: 0,
        },
    }; NR_PROCS]
}

// ─────────────────────────────────────────────────────────────────────────────
// Accessor helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Get a raw pointer to the VFS global state.
///
/// The data lives in a safe `static UnsafeCell<VfsGlobal>` (no
/// `static mut`). `UnsafeCell::get()` returns a `*mut VfsGlobal`
/// without creating any references, avoiding `static_mut_refs`.
#[inline]
pub fn vfs_global() -> *mut VfsGlobal {
    VFS_GLOBAL.get()
}

/// Get a `*mut Fproc` for a given endpoint number.
///
/// # Safety
///
/// The caller must ensure `ep` corresponds to a valid process slot.
#[inline]
pub unsafe fn get_fproc(ep: i32) -> *mut Fproc {
    let slot = (ep & 0xff) as usize;
    let glob = vfs_global();
    let fp = addr_of_mut!((*glob).fproc) as *mut Fproc;
    if slot >= NR_PROCS {
        core::ptr::null_mut()
    } else {
        fp.add(slot)
    }
}

/// Get the current `Fproc` pointer.
///
/// # Safety
///
/// The globals must be properly initialized.
#[inline]
pub unsafe fn current_fp() -> *mut Fproc {
    unsafe { (*vfs_global()).fp }
}

/// Initialize all VFS globals to their default state.
///
/// # Safety
///
/// Must be called exactly once during VFS initialization, before any
/// other VFS code runs.
pub unsafe fn vfs_init() {
    let glob = VFS_GLOBAL.get();

    // Zero tables
    core::ptr::write_bytes(addr_of_mut!((*glob).fproc), 0, 1);
    core::ptr::write_bytes(addr_of_mut!((*glob).filp), 0, 1);
    core::ptr::write_bytes(addr_of_mut!((*glob).vnode), 0, 1);
    core::ptr::write_bytes(addr_of_mut!((*glob).vmnt), 0, 1);
    core::ptr::write_bytes(addr_of_mut!((*glob).dmap), 0, 1);
    core::ptr::write_bytes(addr_of_mut!((*glob).file_lock), 0, 1);
    core::ptr::write_bytes(addr_of_mut!((*glob).workers), 0, 1);
    core::ptr::write_bytes(addr_of_mut!((*glob).scratchpad), 0, 1);

    // Per-request fields
    let g = &mut *glob;
    g.caller_uid = 0;
    g.caller_gid = 0;
    g.req_nr = 0;
    g.fp = core::ptr::null_mut();
    g.err_code = 0;
    g.self_thread = core::ptr::null_mut();

    g.fs_m_in = [0u8; 64];
    g.fs_m_out = [0u8; 64];

    g.susp_count = 0;
    g.nr_locks = 0;
    g.reviving = 0;
    g.pending = 0;
    g.sending = 0;
    g.verbose = 0;
    g.deadlock_resolving = 0;
    g.receive_from = -1;

    g.root_dev = 0;
    g.root_fs_e = -1;
    g.system_hz = 60;
    g.mount_label = [0u8; LABEL_MAX];

    // Initialize fproc endpoint fields
    let fp_base = addr_of_mut!((*glob).fproc) as *mut Fproc;
    for i in 0..NR_PROCS {
        let fp = fp_base.add(i);
        (*fp).fp_endpoint = -1;
        (*fp).fp_pid = PID_FREE;
        (*fp).fp_filp = [-1i32; OPEN_MAX];
        (*fp).fp_blocked_on = FP_BLOCKED_ON_NONE;
        (*fp).fp_task = -1;
        (*fp).fp_suspended_ep = -1;
    }
}
