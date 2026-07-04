//! VFS core types — adapted from `minix/servers/vfs/`
//! `type.h`, `fproc.h`, `file.h`, `vnode.h`, `vmnt.h`,
//! `dmap.h`, `lock.h`, `scratchpad.h`, `threads.h`.

use crate::vfs::consts::*;

// Fproc (per-process VFS state, from fproc.h)

/// Per-process VFS information.
///
/// One slot per process (`NR_PROCS`). Mirrors `struct fproc` from the
/// original C source.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Fproc {
    pub fp_flags: u32,
    pub fp_realuid: u16,
    pub fp_effuid: u16,
    pub fp_realgid: u16,
    pub fp_effgid: u16,
    pub fp_umask: u16,
    pub fp_ngroups: i32,
    pub fp_sgroups: [u16; NGROUPS_MAX],
    pub fp_endpoint: i32,
    pub fp_pid: i32,
    pub fp_text_size: i64,
    pub fp_data_size: i64,
    pub fp_vminode: i32,
    /// Working directory vnode pointer (NULL during reboot).
    pub fp_cdir: *mut Vnode,
    /// Root directory vnode pointer (NULL during reboot).
    pub fp_rdir: *mut Vnode,
    pub fp_filp: [i32; OPEN_MAX],
    pub fp_cloexec: u64,
    pub fp_blocked_on: i32,
    pub fp_task: i32,
    pub fp_tty: i32,
    pub fp_suspended: u8,
    pub fp_reopen: u8,
    pub fp_flush_on_wr: u8,
    pub fp_flush_on_rd: u8,
    pub fp_sesstype: u8,
    pub fp_session: u32,
    pub fp_sessdev: u32,
    pub fp_exit_signal: i32,
    pub fp_sesstask: i32,
    pub fp_suspended_ep: i32,
    pub fp_susp_owner: *mut core::ffi::c_void,
}

impl Default for Fproc {
    fn default() -> Self {
        Self {
            fp_flags: 0,
            fp_realuid: SYS_UID,
            fp_effuid: SYS_UID,
            fp_realgid: SYS_GID,
            fp_effgid: SYS_GID,
            fp_umask: 0o0022,
            fp_ngroups: 0,
            fp_sgroups: [0; NGROUPS_MAX],
            fp_endpoint: -1, // NONE
            fp_pid: PID_FREE,
            fp_text_size: 0,
            fp_data_size: 0,
            fp_vminode: 0,
            fp_cdir: core::ptr::null_mut(),
            fp_rdir: core::ptr::null_mut(),
            fp_filp: [-1i32; OPEN_MAX],
            fp_cloexec: 0,
            fp_blocked_on: FP_BLOCKED_ON_NONE,
            fp_task: -1, // NONE
            fp_tty: NO_DEV as i32,
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
        }
    }
}

/// NO_DEV value (defined in arch-common consts).
const NO_DEV: u32 = 0xffff;

// Filp (open file description, from file.h)

/// Open file description (the "filp" table).
///
/// Intermediary between file descriptors and inodes.
/// A slot is free if `filp_count == 0`.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Filp {
    pub filp_count: i32,
    pub filp_flags: u32,
    pub filp_mode: u32,
    pub filp_state: u32,
    pub filp_ino: u32,
    pub filp_pos: i64,
    /// Vnode this filp refers to (matches C struct filp_vno).
    pub filp_vno: *mut Vnode,
    pub filp_selectors: u32,
    pub filp_select_ops: u32,
    pub filp_select_flags: u32,
    pub filp_select_ep: i32,
    pub filp_pipe_select_ops: u32,
    pub filp_pipe_select_ep: [i32; 2],
    pub filp_pipe_ino: u32,
}

impl Default for Filp {
    fn default() -> Self {
        Self {
            filp_count: 0,
            filp_flags: 0,
            filp_mode: FILP_CLOSED,
            filp_state: 0,
            filp_ino: 0,
            filp_pos: 0,
            filp_vno: core::ptr::null_mut(),
            filp_selectors: 0,
            filp_select_ops: 0,
            filp_select_flags: 0,
            filp_select_ep: -1,
            filp_pipe_select_ops: 0,
            filp_pipe_select_ep: [-1; 2],
            filp_pipe_ino: 0,
        }
    }
}

// Vnode (virtual inode, from vnode.h)

/// Virtual inode (vnode).
///
/// Represents an open file description at the VFS layer,
/// pointing to an inode on a specific FS endpoint.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Vnode {
    pub v_fs: u32,
    pub v_fs_e: i32,
    pub v_inode_nr: u32,
    pub v_mode: u32,
    pub v_size: i64,
    pub v_ref_count: i32,
    pub v_ref_check: i32,
    pub v_fs_count: i32,
    pub v_fs_count_check: i32,
    pub v_smoothed: u8,
    pub v_pipe: u8,
    pub v_bfs_e: i32,
    pub v_dev: u32,
    pub v_fs_dev: u32,
    pub v_fs_count_inc: i32,
}

impl Default for Vnode {
    fn default() -> Self {
        Self {
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
        }
    }
}

// Vmnt (mount point, from vmnt.h)

/// Mount point entry.
///
/// Describes a mounted filesystem: the FS process endpoint,
/// device, root vnode, and its location in the global namespace.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Vmnt {
    pub m_fs: i32,
    pub m_dev: u32,
    pub m_flags: u32,
    pub m_fs_e: i32,
    pub m_root_node: u32,
    pub m_mounted_on: u32,
    pub m_path: [u8; PATH_MAX],
    pub m_label: [u8; LABEL_MAX],
}

impl Default for Vmnt {
    fn default() -> Self {
        Self {
            m_fs: -1,
            m_dev: 0,
            m_flags: 0,
            m_fs_e: -1,
            m_root_node: 0,
            m_mounted_on: 0,
            m_path: [0u8; PATH_MAX],
            m_label: [0u8; LABEL_MAX],
        }
    }
}

// Dmap (device mapper entry, from dmap.h)

/// Device <-> Driver table entry.
///
/// One entry per major device number. Provides the link between
/// major device numbers and the driver process that handles them.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Dmap {
    pub dmap_driver: i32,
    pub dmap_ep: i32,
    pub dmap_style: i32,
    pub dmap_label: [u8; LABEL_MAX],
}

impl Default for Dmap {
    fn default() -> Self {
        Self {
            dmap_driver: -1,
            dmap_ep: -1,
            dmap_style: 0,
            dmap_label: [0u8; LABEL_MAX],
        }
    }
}

// FileLock (advisory locking, from lock.h)

/// Advisory file lock entry.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct FileLock {
    pub lock_type: i16,
    pub lock_pid: i32,
    pub lock_vnode: u32,
    pub lock_first: i64,
    pub lock_last: i64,
}

// WorkerThread (from threads.h)

/// Worker thread state.
///
/// Each worker thread processes VFS requests. Threads are created
/// at startup and wait for work via a condition variable.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct WorkerThread {
    pub w_tid: i32,
    pub w_flags: u32,
    pub w_fp: *mut Fproc,
    pub w_io_vmnt: *mut Vmnt,
    pub w_task: i32,
    pub w_fs_e: i32,
    pub w_drv_e: i32,
    pub w_sendrec: u8,
    pub w_susp: u8,
    pub w_job_typ: i32,
    pub w_job_ref_nr: i32,
}

impl Default for WorkerThread {
    fn default() -> Self {
        Self {
            w_tid: INVALID_THREAD,
            w_flags: 0,
            w_fp: core::ptr::null_mut(),
            w_io_vmnt: core::ptr::null_mut(),
            w_task: -1, // NONE
            w_fs_e: -1,
            w_drv_e: -1,
            w_sendrec: 0,
            w_susp: 0,
            w_job_typ: 0,
            w_job_ref_nr: 0,
        }
    }
}

// Scratchpad (from scratchpad.h)

/// Per-process scratchpad for temporary I/O state.
#[derive(Clone, Copy)]
#[repr(C)]
pub union ScratchpadData {
    pub fd_nr: i32,
    pub filp: *mut Filp,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct IoCmd {
    pub io_buffer: *mut u8,
    pub io_nbytes: usize,
}

impl Default for IoCmd {
    fn default() -> Self {
        Self {
            io_buffer: core::ptr::null_mut(),
            io_nbytes: 0,
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Scratchpad {
    pub file: ScratchpadData,
    pub io: IoCmd,
}

impl Default for Scratchpad {
    fn default() -> Self {
        Self {
            file: ScratchpadData { fd_nr: -1 },
            io: IoCmd::default(),
        }
    }
}


/// Communication state between VFS and a filesystem process.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Comm {
    pub c_max_reqs: i32,
    pub c_cur_reqs: i32,
    pub c_req_queue: *mut WorkerThread,
}

impl Default for Comm {
    fn default() -> Self {
        Self {
            c_max_reqs: 0,
            c_cur_reqs: 0,
            c_req_queue: core::ptr::null_mut(),
        }
    }
}


/// File offset (64-bit, used in VFS<->FS messages).
#[allow(non_camel_case_types)]
pub type off_t = i64;


/// Result of a FS create/lookup/readsuper/newnode operation.
///
/// Mirrors `node_details_t` from `request.h`.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct NodeDetails {
    pub inode_nr: u32,
    pub mode: u32,
    pub file_size: i64,
    pub dev: u32,
}

/// Filesystem statistics (from `statvfs`).
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Statvfs {
    pub f_flags: u64,
    pub f_bsize: u32,
    pub f_frsize: u32,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_favail: u64,
    pub f_fsid: u64,
    pub f_flag: u64,
    pub f_namemax: u64,
}

/// Result of a path lookup.
///
/// Mirrors `lookup_res_t` from `request.h`.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct LookupRes {
    pub inode_nr: u32,
    pub fs_e: i32,
    pub mode: u32,
    pub file_size: i64,
    pub dev: u32,
    pub result: i32,
}

/// Pathname resolution parameters — passed to eat_path/last_dir/advance.
///
/// Mirrors `struct lookup` from `minix/servers/vfs/path.h`.
#[derive(Debug, Clone, Copy)]
pub struct Lookup {
    pub l_flags: u32,
    pub l_vnode_lock: i32,
    pub l_vmnt_lock: i32,
    pub l_path: [u8; PATH_MAX],
    pub l_path_len: usize,
    pub l_uid: u16,
    pub l_gid: u16,
    /// Output: pointer to resolved vnode (set by advance/lookup).
    pub l_vnode: *mut Vnode,
    /// Output: pointer to resolved vmnt (set by advance/lookup).
    pub l_vmp: *mut Vmnt,
}

impl Default for Lookup {
    fn default() -> Self {
        Self {
            l_flags: 0,
            l_vnode_lock: VNODE_READ,
            l_vmnt_lock: VMNT_READ,
            l_path: [0u8; PATH_MAX],
            l_path_len: 0,
            l_uid: 0,
            l_gid: 0,
            l_vnode: core::ptr::null_mut(),
            l_vmp: core::ptr::null_mut(),
        }
    }
}
