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

    // Set up PM's Fproc entry so VFS can reply to PM messages.
    // PM has endpoint 0, slot 0.
    let pm_fp = unsafe { &mut *fproc_array.add(0) };
    pm_fp.fp_endpoint = PM_PROC_NR;
    pm_fp.fp_pid = 1;

    worker::worker_init();

    unsafe { (*glob).system_hz = 60 };

    OK
}

// Work loop

/// Receive a message from any source.
///
/// Uses the kernel `receive` syscall to block until a message arrives.
/// The message is stored in `fs_m_in`.
unsafe fn get_work() {
    let glob = vfs_global();
    let buf = &mut (*glob).fs_m_in as *mut [u8; 64];

    #[cfg(target_os = "none")]
    {
        const RECEIVE_CALL: u64 = 47;
        const ANY: i32 = 0x0000ffff;
        let src = unsafe { minix_rt::syscall2(RECEIVE_CALL, ANY as u64, buf as u64) };
        if src >= 0 {
            // Store the sender endpoint at offset 0 (m_source)
            let src_bytes = (src as i32).to_le_bytes();
            core::ptr::copy_nonoverlapping(src_bytes.as_ptr(), buf as *mut u8, 4);
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = buf;
    }
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

    // Read the call number (message type) from offset 4.
    let call_nr = i32::from_le_bytes(fs_m_in[4..8].try_into().unwrap_or([0; 4]));
    (*glob).req_nr = call_nr;

    // Look up the caller's Fproc slot from the source endpoint.
    let fp = if source >= 0 {
        let slot = (source & 0xff) as usize;
        if slot < 256 {
            let fp_base = addr_of_mut!((*glob).fproc) as *mut Fproc;
            fp_base.add(slot)
        } else {
            core::ptr::null_mut()
        }
    } else {
        core::ptr::null_mut()
    };
    (*glob).fp = fp;

    let result = if source == PM_PROC_NR {
        // PM messages are dispatched through service_pm.
        pm::service_pm()
    } else {
        // Regular VFS calls are dispatched through the call table.
        table::dispatch(call_nr)
    };

    (*glob).err_code = result;
    reply(fp, result);
}

/// Send a reply message to a process.
///
/// Writes the result code into the outgoing message buffer and sends
/// it via `sendrec` to the caller. The `who` pointer identifies the
/// caller's Fproc slot; if null, the reply is skipped.
unsafe fn reply(who: *mut Fproc, result: i32) {
    if who.is_null() {
        return;
    }

    #[cfg(target_os = "none")]
    {
        let glob = vfs_global();
        let out = &mut (*glob).fs_m_out;
        // Write the result code into the message type field (offset 4).
        out[4..8].copy_from_slice(&result.to_le_bytes());

        const SENDREC_CALL: u64 = 48;
        let dest = (*who).fp_endpoint;
        if dest >= 0 {
            minix_rt::syscall2(SENDREC_CALL, dest as u64, out as *mut [u8; 64] as u64);
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = result;
    }
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
