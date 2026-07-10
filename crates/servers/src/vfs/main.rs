//! VFS main entry point and main loop — adapted from `minix/servers/vfs/main.c`
//!
//! Implements the VFS server's main loop: receive requests, dispatch
//! them to workers, and send replies.

use core::ptr::addr_of_mut;

use crate::vfs::consts::*;
use crate::vfs::dmap;
use crate::vfs::glo::vfs_global;
use crate::vfs::mount;
use crate::vfs::pm;
use crate::vfs::table;
use crate::vfs::types::Fproc;
use crate::vfs::worker;

/// PM process endpoint.
const PM_PROC_NR: i32 = 0;

/// Boot process endpoints — processes the kernel loads before starting
/// the scheduler.  Their Fproc slots must be populated before
/// `mount_root` so they get `fp_rdir` / `fp_cdir` assigned.
///
/// Matches the `boot_procs` table in `crates/kernel-boot/src/main.rs`.
const BOOT_ENDPOINTS: &[i32] = &[
    arch_common::com::PM_PROC_NR,      // 0: Process Manager
    arch_common::com::VFS_PROC_NR,     // 1: Virtual File System (self)
    arch_common::com::RS_PROC_NR,      // 2: Reincarnation Server
    arch_common::com::MFS_PROC_NR,     // 7: Minix File System
    arch_common::com::INIT_PROC_NR,    // 10: init
    arch_common::com::RAMDISK_PROC_NR, // 11: RAM disk driver
];

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

    // Note: SYS_BOOT_COMPLETE (syscall 60) is now called in
    // sef_cb_init_fresh before mount_root, so boot tests run
    // even when mount_root blocks.
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

    // Pre-populate Fproc entries for all boot processes so that
    // mount_root can assign them fp_rdir / fp_cdir.
    // PIDs start at 1 (PM) and increase; exact values don't matter for
    // boot since PM handles the authoritative PID assignment.
    for (pid, &ep) in (1..).zip(BOOT_ENDPOINTS.iter()) {
        let slot = (ep & 0xff) as usize;
        if slot < 256 {
            let fp = unsafe { &mut *fproc_array.add(slot) };
            fp.fp_endpoint = ep;
            fp.fp_pid = pid;
        }
    }

    worker::worker_init();

    unsafe { (*glob).system_hz = 60 };

    // Initialize VFS data structures before registering with device map.
    mount::init_vnodes();
    mount::init_vmnts();
    crate::vfs::filedes::init_filps();

    // Initialise device map and mount the root filesystem.
    dmap::init_dmap();

    // Register the grant table with the kernel so FS servers can
    // use SAFECOPYTO/SAFECOPYFROM to transfer data through grants.
    crate::vfs::grant::vfs_grant_init();

    // Signal kernel that VFS init is complete (before blocking mount).
    // Boot-test: kernel runs non-filesystem tests and exits QEMU.
    // Normal boot: no handler registered, returns -38 (ENOSYS), ignored.
    #[cfg(target_os = "none")]
    unsafe {
        let _ = minix_rt::syscall1(60, 0);
    }

    let root_vp = mount::mount_root();
    if !root_vp.is_null() {
        // Set up root and working directories for all boot processes,
        // so they can resolve absolute paths immediately.
        let fproc_array = addr_of_mut!((*glob).fproc) as *mut Fproc;
        for i in 0..256 {
            let rfp = unsafe { &mut *fproc_array.add(i) };
            if rfp.fp_endpoint >= 0 {
                mount::dup_vnode(root_vp);
                rfp.fp_rdir = root_vp;
                mount::dup_vnode(root_vp);
                rfp.fp_cdir = root_vp;
            }
        }
    }

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

    // Notifications (m_type == -10) are fire-and-forget — no reply needed.
    // Replying to a notification would send a message back to the sender,
    // who is NOT waiting for a reply, creating an infinite IPC loop.
    if call_nr == -10 {
        // Update req_nr, but skip dispatch and reply.
        return;
    }

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

    // Store the caller endpoint so handlers can use it for data copies.
    if !fp.is_null() {
        (*fp).fp_endpoint = source;
    }

    if source == PM_PROC_NR {
        // PM messages are handled by service_pm, which sends its own
        // reply directly — do NOT call reply() here. This matches the C
        // pattern where service_pm calls ipc_send() directly (main.c:796).
        let _ = pm::service_pm();
    } else {
        // Regular VFS calls are dispatched through the call table.
        let result = table::dispatch(call_nr);
        (*glob).err_code = result;
        reply(fp, result);
    }
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

        const SEND_CALL: u64 = 46;
        let dest = (*who).fp_endpoint;
        if dest >= 0 {
            minix_rt::syscall2(SEND_CALL, dest as u64, out as *mut [u8; 64] as u64);
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
