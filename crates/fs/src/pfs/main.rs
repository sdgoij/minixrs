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
pub fn pfs_main() -> i32 {
    pfs_init();

    #[cfg(target_os = "minix")]
    unsafe {
        use arch_common::ipc::Message;
        const RECEIVE_CALL: u64 = 47;
        const SEND_CALL: u64 = 46;
        const ANY: i32 = 0x0000ffff;

        loop {
            let pfs = glo::pfs_ptr();
            if (*pfs).unmountdone != 0 && (*pfs).exitsignaled != 0 {
                break;
            }

            let mut msg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: core::mem::zeroed(),
            };
            let src = minix_rt::syscall2(RECEIVE_CALL, ANY as u64, &mut msg as *mut Message as u64);
            if src < 0 {
                continue;
            }

            // Copy message fields into PFS globals for handler access.
            let call_nr = msg.m_type as u32;
            (*pfs).m_in_type = msg.m_type;
            (*pfs).m_source = msg.m_source;
            // Copy first 48 bytes of payload (m_in_data size)
            let src_data = &msg as *const Message as *const u8;
            let dst_data = core::ptr::addr_of_mut!((*pfs).m_in_data) as *mut u8;
            core::ptr::copy_nonoverlapping(src_data.add(8), dst_data, 48);

            // Dispatch
            let idx = (call_nr.wrapping_sub(FS_BASE as u32)) as usize;
            let result = crate::pfs::table::dispatch(idx);

            // Copy reply data from m_out_data into message
            let reply_data = core::ptr::addr_of!((*pfs).m_out_data) as *const u8;
            let msg_data = &mut msg as *mut Message as *mut u8;
            core::ptr::copy_nonoverlapping(reply_data, msg_data.add(8), 48);
            msg.m_type = result;

            // Reply with a plain SEND (C `ipc_send`), matching MFS: a
            // SENDREC here would make the reply's receive phase swallow the
            // client's next request (the DS hang this port already hit).
            minix_rt::syscall2(SEND_CALL, src as u64, &mut msg as *mut Message as u64);
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
