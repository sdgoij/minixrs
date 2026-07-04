//! MFS main server loop — adapted from `minix/fs/mfs/main.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::misc::*;

/// IPC receive/send syscall numbers.  Only used when compiling for the
/// MINIX target; marked `#[allow(dead_code)]` because the library build
/// (`cargo check`) compiles without `target_os = "none"`.
#[cfg(target_os = "none")]
const RECEIVE_CALL: u64 = 47;
#[cfg(target_os = "none")]
const SENDREC_CALL: u64 = 48;
#[allow(dead_code)]
const ANY: i32 = 0x0000ffff;

// Reference: main.c sef_cb_init_fresh()
pub fn mfs_init() -> i32 {
    unsafe {
        glo::mfs_init_globals();
        for i in 0..NR_INODES {
            let inode_ptr = glo::get_inode_ptr(i);
            (*inode_ptr).i_count = 0;
            (*glo::mfs_ptr()).cch[i] = 0;
        }
        init_inode_cache();

        // Register the block I/O callback if a RAM disk is configured.
        if crate::block_io::ram_disk_is_initialized() {
            libs::libminixfs::cache::lmfs_set_block_io(crate::block_io::ram_disk_io);
        }
        // `mfs_init` is called.
        if crate::block_io::ram_disk_is_initialized() {
            libs::libminixfs::cache::lmfs_set_block_io(crate::block_io::ram_disk_io);
        }
    }
    OK
}

// Reference: main.c main()
pub fn mfs_main() -> i32 {
    #[cfg(target_os = "none")]
    {
        mfs_init();

        loop {
            let mut msg = arch_common::ipc::Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };

            // Receive a message from any sender.
            // syscall2(RECEIVE_CALL=47, src=ANY, msg_ptr) → sender endpoint
            let src = unsafe {
                minix_rt::syscall2(
                    RECEIVE_CALL,
                    ANY as u64,
                    &mut msg as *mut arch_common::ipc::Message as u64,
                )
            };
            if src < 0 {
                continue;
            }
            let _src_ep = src as i32;

            // Determine request number by subtracting FS_BASE from m_type.
            let req_type = msg.m_type;
            let req_nr = (req_type - crate::mfs::consts::FS_BASE) as usize;
            // Extract caller credentials before moving msg into global state.
            // Union field access requires unsafe.
            let (caller_uid, caller_gid) =
                unsafe { (msg.m_payload.m1.m1i1 as u16, msg.m_payload.m1.m1i2 as u16) };
            // Store the incoming message and derived fields in global state.
            unsafe {
                (*glo::mfs_ptr()).m_in = msg;
                (*glo::mfs_ptr()).req_nr = req_nr as i32;
                (*glo::mfs_ptr()).caller_uid = caller_uid;
                (*glo::mfs_ptr()).caller_gid = caller_gid;
            }

            // Dispatch the request.
            let status = crate::mfs::table::dispatch(req_nr);

            // Build and send the reply.
            let mut reply = arch_common::ipc::Message {
                m_source: 0,
                m_type: status,
                m_payload: unsafe { core::mem::zeroed() },
            };
            // Clone the reply into global state (Message is Clone).
            unsafe {
                (*glo::mfs_ptr()).m_out = reply.clone();
            }
            let _ = unsafe {
                minix_rt::syscall2(
                    SENDREC_CALL,
                    src as u64,
                    &mut reply as *mut arch_common::ipc::Message as u64,
                )
            };
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        mfs_init();
        OK
    }
}

// Reference: main.c sef_cb_signal_handler()
pub fn signal_handler(signo: i32) {
    if signo != 15 {
        return;
    }
    unsafe {
        (*glo::mfs_ptr()).exitsignaled = TRUE;
    }
    fs_sync();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mfs_init() {
        assert_eq!(mfs_init(), OK);
    }

    /// Verify the buffer cache wiring end-to-end:
    /// 1. Set up a RAM disk with known data
    /// 2. Initialise the cache and block I/O callback
    /// 3. Read a block via lmfs_get_block and verify data
    #[test]
    fn test_buffer_cache_read_from_ram_disk() {
        use alloc::vec;

        // Create a RAM disk with a recognizable pattern.
        let mut image = vec![0u8; 8192];
        // Write a signature at the start of block 0.
        image[0..4].copy_from_slice(b"MFS\0");
        // Write another at block 1.
        image[4096..4100].copy_from_slice(b"BLK1");

        // Initialise RAM disk and buffer cache.
        unsafe {
            crate::block_io::ram_disk_init(image.as_ptr(), image.len());
            libs::libminixfs::cache::lmfs_buf_pool(10);
            libs::libminixfs::cache::lmfs_set_blocksize(4096, 0);
            libs::libminixfs::cache::lmfs_set_block_io(crate::block_io::ram_disk_io);
        }

        unsafe {
            // Read block 0.
            let bp = libs::libminixfs::cache::lmfs_get_block(0, 0);
            assert!(!bp.is_null(), "lmfs_get_block should return a buffer");
            let data = (*bp).data_ptr;
            assert!(!data.is_null());
            let header = core::slice::from_raw_parts(data, 4);
            assert_eq!(header, b"MFS\0", "block 0 should contain signature");
            libs::libminixfs::cache::lmfs_put_block(bp, FULL_DATA_BLOCK);

            // Read block 1.
            let bp2 = libs::libminixfs::cache::lmfs_get_block(0, 1);
            assert!(!bp2.is_null());
            let data2 = (*bp2).data_ptr;
            let header2 = core::slice::from_raw_parts(data2, 4);
            assert_eq!(header2, b"BLK1", "block 1 should contain signature");
            libs::libminixfs::cache::lmfs_put_block(bp2, FULL_DATA_BLOCK);
        }
    }
}
