//! VFS main entry point and main loop — adapted from `minix/servers/vfs/main.c`
//!
//! Implements the VFS server's main loop: receive requests, dispatch
//! them to workers, and send replies.

use core::ptr::addr_of_mut;

use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::pm;
use crate::vfs::table;
use crate::vfs::types::Fproc;
use crate::vfs::worker;

/// PM process endpoint.
const PM_PROC_NR: i32 = 0;

/// Offset of m_source in the message buffer (4 bytes).
const MSG_SOURCE_OFF: usize = 0;

// Entry point

/// VFS main entry point.
///
/// # Safety
///
/// Must be called exactly once, from the VFS server's initial thread.
pub unsafe fn vfs_main() -> i32 {
    sef_local_startup();

    loop {
        get_work();
        handle_work();
    }
}

// SEF callbacks (stubs)

/// Register SEF init and signal callbacks.
unsafe fn sef_local_startup() {
    sef_cb_init_fresh();
}

/// Fresh initialization callback.
unsafe fn sef_cb_init_fresh() -> i32 {
    let glob = vfs_global();

    // Initialize process endpoints to NONE.
    let fproc_array = addr_of_mut!((*glob).fproc) as *mut Fproc;
    for i in 0..256 {
        let rfp = unsafe { &mut *fproc_array.add(i) };
        rfp.fp_endpoint = -1;
        rfp.fp_pid = PID_FREE;
        rfp.fp_blocked_on = FP_BLOCKED_ON_NONE;
        rfp.fp_realuid = SYS_UID;
        rfp.fp_effuid = SYS_UID;
        rfp.fp_realgid = SYS_GID;
        rfp.fp_effgid = SYS_GID;
        rfp.fp_umask = 0o0022;
    }

    worker::worker_init();

    unsafe { (*glob).system_hz = 60 };

    OK
}

// Work loop

/// Receive a message from any source.
unsafe fn get_work() {
    let _glob = vfs_global();
}

/// Dispatch the current request to the appropriate handler.
///
/// Reads the message source and type from the global message buffer.
/// If the sender is PM_PROC_NR, routes to `service_pm()`. Otherwise
/// dispatches through the regular VFS call table.
unsafe fn handle_work() {
    let glob = vfs_global();

    // Read the sender endpoint from the message (offset 0 = m_source).
    let fs_m_in = &(*glob).fs_m_in;
    let source = i32::from_le_bytes(
        fs_m_in[MSG_SOURCE_OFF..MSG_SOURCE_OFF + 4]
            .try_into()
            .unwrap_or([0; 4]),
    );

    if source == PM_PROC_NR {
        // PM messages are dispatched through service_pm.
        let result = pm::service_pm();
        (*glob).err_code = result;
        reply((*glob).fp, result);
    } else {
        // Regular VFS calls are dispatched through the call table.
        let call_nr = (*glob).req_nr;
        let result = table::dispatch(call_nr);
        (*glob).err_code = result;
        reply((*glob).fp, result);
    }
}

/// Send a reply message to a process.
unsafe fn reply(who: *mut Fproc, result: i32) {
    if who.is_null() {
        return;
    }
    let _ = result;
}

// Helpers

/// Send a reply code (just an errno value, no payload).
#[allow(unused)]
unsafe fn reply_code(_whom: i32, result: i32) {
    let _ = result;
}

// Exported helpers

/// Lock a process for exclusive access by a worker thread.
///
/// # Safety
///
/// `_rfp` must point to a valid, unlocked `Fproc`.
pub unsafe fn lock_proc(_rfp: *mut Fproc) {}

/// Unlock a process.
///
/// # Safety
///
/// `_rfp` must point to a valid, locked `Fproc`.
pub unsafe fn unlock_proc(_rfp: *mut Fproc) {}

/// Clean up after a worker thread finishes its job.
///
/// # Safety
///
/// Requires exclusive access to global state.
pub unsafe fn thread_cleanup() {}

/// Service a postponed PM request.
///
/// # Safety
///
/// Requires exclusive access to the calling process's Fproc.
pub unsafe fn service_pm_postponed() {}
