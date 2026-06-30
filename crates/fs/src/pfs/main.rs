//! PFS main server loop — adapted from `minix/fs/pfs/main.c`
//!
//! The main loop receives VFS requests, dispatches them through
//! the call vector, and sends replies.  This is the entry point
//! for the Pipe File Server.

use crate::pfs::buffer::*;
use crate::pfs::consts::*;
use crate::pfs::glo;
use crate::pfs::inode::*;
/// Initialize the PFS server.
///
/// Called once at startup to set up inode table and buffer pool.
// Reference: main.c sef_cb_init_fresh()
pub fn pfs_init() -> i32 {
    unsafe {
        glo::pfs_init_globals();

        for i in 0..PFS_NR_INODES {
            let inode_ptr = glo::get_inode_ptr(i);
            (*inode_ptr).i_count = 0;
        }

        init_inode_cache();
        init_buffer_pool();

        let pfs = glo::pfs_ptr();
        (*pfs).exitsignaled = 0;
        (*pfs).unmountdone = FALSE;
    }
    OK
}

/// Main server loop entry point.
///
/// After initialization, enters an infinite loop receiving VFS requests,
/// dispatching them through the call vector, and sending replies.
///
/// On the host platform, acts as a no-op placeholder (IPC not available).
/// On the Minix target, uses minix_std::receive/send for IPC.
// Reference: main.c main()
pub fn pfs_main() -> i32 {
    pfs_init();

    // The main loop is only active on the Minix target.
    // On the host, it returns immediately.
    #[cfg(target_os = "none")]
    unsafe {
        loop {
            let pfs = glo::pfs_ptr();
            if (*pfs).unmountdone != FALSE && (*pfs).exitsignaled != 0 {
                break;
            }
            // IPC receive/dispatch/reply would go here.
            // Waits for minix_std::receive() to be available.
        }
    }

    OK
}

/// Signal handler for termination.
///
/// Only responds to SIGTERM (signal 15).
// Reference: main.c sef_cb_signal_handler()
pub fn signal_handler(signo: i32) {
    if signo != 15 {
        return;
    }
    unsafe {
        (*glo::pfs_ptr()).exitsignaled = 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pfs_init() {
        assert_eq!(pfs_init(), OK);
        unsafe {
            let pfs = glo::pfs_ptr();
            assert_eq!((*pfs).exitsignaled, 0);
            assert_eq!((*pfs).unmountdone, FALSE);
        }
    }

    #[test]
    fn test_pfs_main_returns_ok() {
        // This should initialize and return OK
        let r = pfs_main();
        assert_eq!(r, OK);
    }

    #[test]
    fn test_signal_handler_ignores_non_sigterm() {
        unsafe {
            glo::pfs_init_globals();
            signal_handler(10); // Not SIGTERM
            let pfs = glo::pfs_ptr();
            let flags = core::ptr::addr_of_mut!((*pfs).exitsignaled);
            assert_eq!(flags.read(), 0);
        }
    }

    #[test]
    fn test_signal_handler_sigterm() {
        unsafe {
            glo::pfs_init_globals();
            signal_handler(15); // SIGTERM
            let pfs = glo::pfs_ptr();
            let flags = core::ptr::addr_of_mut!((*pfs).exitsignaled);
            assert_eq!(flags.read(), 1);
        }
    }
}
