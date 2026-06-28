//! Worker thread management — adapted from `minix/servers/vfs/worker.c`
//!
//! Worker threads accept VFS requests and dispatch them. Each worker
//! runs in its own thread, waiting on a condition variable for work.

use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::types::*;

// ── Initialization ─────────────────────────────────────────────────────────

/// Initialize the worker thread pool.
///
/// # Safety
///
/// Must be called exactly once during VFS initialization, before any
/// threads are spawned.
pub unsafe fn worker_init() {
    let glob = vfs_global();
    let workers = unsafe { core::ptr::addr_of_mut!((*glob).workers) as *mut WorkerThread };
    for i in 0..NR_WTHREADS {
        let w = unsafe { &mut *workers.add(i) };
        *w = WorkerThread::default();
        w.w_tid = INVALID_THREAD;
        w.w_task = -1;
    }
}

// ── Worker management ──────────────────────────────────────────────────────

/// Start a worker thread for a given process.
///
/// # Safety
///
/// Requires exclusive access to the worker table.
pub unsafe fn worker_start(rfp: *mut Fproc) -> i32 {
    let glob = vfs_global();
    let workers = unsafe { core::ptr::addr_of_mut!((*glob).workers) as *mut WorkerThread };

    for i in 0..NR_WTHREADS {
        let w = unsafe { &mut *workers.add(i) };
        if w.w_fp.is_null() {
            w.w_fp = rfp;
            w.w_flags = 0;
            w.w_task = -1;
            w.w_fs_e = -1;
            w.w_drv_e = -1;
            w.w_sendrec = 0;
            w.w_susp = 0;
            w.w_job_typ = 0;
            w.w_job_ref_nr = 0;
            return i as i32;
        }
    }

    ENFILE
}

/// Stop a worker thread.
///
/// # Safety
///
/// Requires exclusive access to the worker table.
pub unsafe fn worker_stop(worker_idx: i32) {
    let glob = vfs_global();
    let workers = unsafe { core::ptr::addr_of_mut!((*glob).workers) as *mut WorkerThread };
    if worker_idx < 0 || (worker_idx as usize) >= NR_WTHREADS {
        return;
    }
    let w = unsafe { &mut *workers.add(worker_idx as usize) };
    w.w_fp = core::ptr::null_mut();
    w.w_flags = 0;
    w.w_task = -1;
    w.w_fs_e = -1;
    w.w_drv_e = -1;
    w.w_job_typ = 0;
    w.w_job_ref_nr = 0;
}

/// Check how many worker threads are currently available.
///
/// # Safety
///
/// Requires shared access to the worker table.
pub unsafe fn worker_available() -> i32 {
    let glob = vfs_global();
    let workers = unsafe { core::ptr::addr_of_mut!((*glob).workers) as *mut WorkerThread };
    let mut avail = 0;
    for i in 0..NR_WTHREADS {
        let w = unsafe { &mut *workers.add(i) };
        if w.w_fp.is_null() {
            avail += 1;
        }
    }
    avail
}
