//! ISO 9660 main server loop — adapted from `minix/fs/iso9660fs/main.c`
//!
//! Main loop that waits for VFS requests, dispatches them to the appropriate
//! handler via the call table, and sends replies.

use crate::iso9660::consts::*;
use crate::iso9660::glo;
use crate::iso9660::inode;
use crate::iso9660::table;

/// Main entry point for the ISO 9660 file system server.
///
/// Initializes the server, then enters an infinite loop waiting for
/// VFS requests and dispatching them.
///
/// # Safety
///
/// Requires exclusive access to globals. Must be called exactly once.
pub unsafe fn main_loop() -> i32 {
    // SEF local startup (stub — real Minix calls sef_local_startup())
    sef_local_startup();

    loop {
        // Wait for request message (stub — real Minix uses sef_receive())
        let who_e = get_work();

        let isofs = glo::isofs_ptr();

        (*isofs).caller_uid = INVAL_UID; // To trap errors
        (*isofs).caller_gid = INVAL_GID;

        if who_e != VFS_PROC_NR {
            continue;
        }

        let mut req_nr = (*isofs).req_nr;

        if req_nr < FS_BASE {
            req_nr += FS_BASE;
        }

        let ind = (req_nr - FS_BASE) as usize;

        let error = if ind >= NREQS {
            EINVAL
        } else {
            table::dispatch_call(ind)
        };

        // fs_m_out.m_type = error;
        // reply(who_e, &fs_m_out);
        let _ = error;
    }
}

/// SEF local startup — initialize the server.
///
/// # Safety
///
/// Must be called before entering the main loop.
unsafe fn sef_local_startup() {
    // Register init callbacks (stub)
    // sef_setcb_init_fresh(sef_cb_init_fresh);
    // sef_setcb_init_restart(sef_cb_init_fail);
    // sef_setcb_signal_handler(sef_cb_signal_handler);
    // sef_startup();

    inode::init_inode_cache();
    // lmfs_buf_pool(10);
}

/// Wait for a request message.
///
/// In the real Minix implementation this calls `sef_receive(ANY, m_in)`.
/// Returns the source endpoint of the message.
fn get_work() -> i32 {
    // stub: sef_receive(ANY, &fs_m_in)
    VFS_PROC_NR
}

/// Send a reply message to the caller.
///
/// In the real Minix implementation this calls `ipc_send(who, m_out)`.
#[allow(dead_code)]
fn reply(who: i32) {
    // stub: ipc_send(who, &fs_m_out)
    let _ = who;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso9660::glo;

    #[test]
    fn test_sef_local_startup() {
        unsafe {
            glo::isofs_init_globals();
            sef_local_startup();
            // Should not panic
        }
    }

    #[test]
    fn test_get_work_stub() {
        let who = get_work();
        assert_eq!(who, VFS_PROC_NR);
    }

    #[test]
    fn test_reply_stub() {
        reply(VFS_PROC_NR);
        // Should not panic
    }
}
