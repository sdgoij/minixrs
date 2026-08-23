//! MFS main server loop — adapted from `minix/fs/mfs/main.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::misc::*;

/// Virtual address of the boot filesystem image in the ramdisk driver
/// server's address space (set up by the kernel boot code). Only used by the
/// host test path, which fakes a direct-memory RAM disk at this address.
#[cfg(not(target_os = "minix"))]
const RAMDISK_IMAGE_VA: u64 = arch_common::com::RAMDISK_IMAGE_VA;

/// Parse a REQ_MKNOD message payload (VFS `req_mknod`'s embedded-path
/// layout) into the request fields `fs_mknod` reads: cch[0] = dir_ino,
/// cch[1] = mode, cch[2] = device, cch[3] = uid, cch[4] = gid and
/// user_path = the entry name (null-terminated).
///
/// Payload-relative byte offsets (kept in lockstep with `vfs/request.rs`
/// `build_mknod_msg`):
///   raw[0..4]   = dir_ino (u32)
///   raw[4..6]   = mode (u16)
///   raw[6..8]   = uid (u16)
///   raw[8..10]  = gid (u16)
///   raw[10..14] = device (u32)
///   raw[14..18] = path_len (u32)
///   raw[18..]   = name (up to 30 bytes, null-terminated)
///
/// Short/truncated payloads parse defensively (zeros, empty name).
#[cfg(any(test, target_os = "minix"))]
pub(crate) fn parse_mknod_request(raw: &[u8], cch: &mut [i32], user_path: &mut [u8]) {
    let rd32 = |off: usize| {
        u32::from_le_bytes(
            raw.get(off..off + 4)
                .and_then(|s| s.try_into().ok())
                .unwrap_or([0u8; 4]),
        )
    };
    let rd16 = |off: usize| {
        u16::from_le_bytes(
            raw.get(off..off + 2)
                .and_then(|s| s.try_into().ok())
                .unwrap_or([0u8; 2]),
        )
    };

    let dir_ino = rd32(0);
    let mode = rd16(4);
    let uid = rd16(6);
    let gid = rd16(8);
    let device = rd32(10);
    let path_len = rd32(14) as usize;

    cch[0] = dir_ino as i32;
    cch[1] = mode as i32;
    cch[2] = device as i32;
    cch[3] = uid as i32;
    cch[4] = gid as i32;

    let copy_len = path_len
        .min(PATH_MAX - 1)
        .min(30)
        .min(raw.len().saturating_sub(18));
    if copy_len > 0 {
        user_path[..copy_len].copy_from_slice(&raw[18..18 + copy_len]);
    }
    user_path[copy_len] = 0;
}
#[cfg(not(target_os = "minix"))]
const RAMDISK_IMAGE_SIZE: usize = arch_common::com::RAMDISK_IMAGE_SIZE;

/// IPC receive/send syscall numbers.  Only used when compiling for the
/// MINIX target; marked `#[allow(dead_code)]` because the library build
/// (`cargo check`) compiles without `target_os = "minix"`.
#[cfg(target_os = "minix")]
const RECEIVE_CALL: u64 = 47;
#[cfg(target_os = "minix")]
const SEND_CALL: u64 = 46;
#[cfg(target_os = "minix")]
#[allow(dead_code)]
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

        // Initialise the buffer cache.
        libs::libminixfs::cache::lmfs_buf_pool(crate::mfs::consts::DEFAULT_NR_BUFS as i32);
        libs::libminixfs::cache::lmfs_set_blocksize(4096, 0);

        // Register the block I/O callback.
        #[cfg(target_os = "minix")]
        {
            // Root-filesystem block I/O goes through the BDEV protocol to the
            // ramdisk driver server, which serves the boot image mapped into
            // its address space by the kernel boot code.
            crate::block_io::bdev_init();
            libs::libminixfs::cache::lmfs_set_block_io(crate::block_io::bdev_ram_disk_io);
        }
        #[cfg(not(target_os = "minix"))]
        {
            // Host tests: direct-memory RAM disk (no driver server).
            crate::block_io::ram_disk_init(RAMDISK_IMAGE_VA as *const u8, RAMDISK_IMAGE_SIZE);
            libs::libminixfs::cache::lmfs_set_block_io(crate::block_io::ram_disk_io);
        }
    }
    OK
}

// Reference: main.c main()
pub fn mfs_main() -> i32 {
    #[cfg(target_os = "minix")]
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

            // For lookup requests, extract fields from the message payload.
            // VFS req_lookup writes at PAYLOAD_OFF (offset 8 in Message):
            //   +0  = dir_ino  (u64) -> m_payload.m4.l1, m4.l2
            //   +8  = root_ino (u64) -> m_payload.m4.l3, m4.l4
            //   +16 = flags    (u32) -> m_payload.m1.m1i5
            //   +20 = path_len (u32) -> m_payload.m1.m1i6
            //   +24 = path data
            if req_nr == 26 {
                unsafe {
                    let mfs = glo::mfs_ptr();
                    let dir_ino = (*mfs).m_in.m_payload.m4.m4l1 as u64;
                    let root_ino = (*mfs).m_in.m_payload.m4.m4l2 as u64;
                    let flags = (*mfs).m_in.m_payload.m1.m1i5;
                    let path_len = (*mfs).m_in.m_payload.m1.m1i6 as usize;
                    (*mfs).lookup_dir_ino = dir_ino as u32;
                    (*mfs).lookup_root_ino = root_ino as u32;
                    (*mfs).lookup_flags = flags;
                    (*mfs).lookup_path_len = path_len;
                    // Copy path from embedded location in payload (+24 = m_payload.raw[24])
                    let raw = (*mfs).m_in.m_payload.raw;
                    let copy_len = path_len.min(PATH_MAX - 1).min(24);
                    if copy_len > 0 {
                        let user_path_ptr = (*mfs).user_path.as_mut_ptr();
                        for j in 0..copy_len {
                            core::ptr::write(user_path_ptr.add(j), raw[24 + j]);
                        }
                        core::ptr::write(user_path_ptr.add(copy_len), 0);
                    }
                }
            }

            // For mknod (21), create (23) and mkdir (22) requests, extract
            // fields from the message payload and read the filename from
            // embedded data. VFS req_mknod/req_create/req_mkdir layout (raw
            // offsets):
            //   raw[0..4]   = dir_ino (u32)
            //   raw[4..6]   = mode (u16)
            //   raw[6..8]   = uid (u16)
            //   raw[8..10]  = gid (u16)
            //   raw[10..14] = device (u32, mknod only)
            //   raw[14..18] = path_len (u32)
            //   raw[18..]   = path data (up to 30 bytes, null-terminated)
            // mknod's handler reads the device from cch[2], so it is
            // parsed into that slot; create/mkdir leave it as the gid.
            if req_nr == 21 {
                unsafe {
                    let mfs = glo::mfs_ptr();
                    let raw = (*mfs).m_in.m_payload.raw;
                    parse_mknod_request(&raw, &mut (*mfs).cch, &mut (*mfs).user_path);
                }
            }

            // For create (23) and mkdir (22) requests, extract fields from
            // the message payload and read the filename from embedded data.
            // VFS req_create/req_mkdir layout (raw offsets):
            //   raw[0..4]   = dir_ino (u32)
            //   raw[4..6]   = mode (u16)
            //   raw[6..8]   = uid (u16)
            //   raw[8..10]  = gid (u16)
            //   raw[12..16] = path_len (u32)
            //   raw[16..]   = path data (up to 28 bytes, null-terminated)
            if req_nr == 22 || req_nr == 23 {
                unsafe {
                    let mfs = glo::mfs_ptr();
                    let raw = (*mfs).m_in.m_payload.raw;
                    let dir_ino = u32::from_ne_bytes(raw[0..4].try_into().unwrap_or([0u8; 4]));
                    let mode = u16::from_ne_bytes(raw[4..6].try_into().unwrap_or([0u8; 2]));
                    let uid = u16::from_ne_bytes(raw[6..8].try_into().unwrap_or([0u8; 2]));
                    let gid = u16::from_ne_bytes(raw[8..10].try_into().unwrap_or([0u8; 2]));
                    let path_len =
                        u32::from_ne_bytes(raw[12..16].try_into().unwrap_or([0u8; 4])) as usize;

                    // Store in cch[] for the handler.
                    (*mfs).cch[0] = dir_ino as i32;
                    (*mfs).cch[1] = mode as i32;
                    (*mfs).cch[2] = uid as i32;
                    (*mfs).cch[3] = gid as i32;

                    // Copy path from embedded message data (raw[16..]).
                    let copy_len = path_len.min(PATH_MAX - 1).min(28);
                    if copy_len > 0 {
                        let mut tmp = [0u8; 28];
                        let len = copy_len.min(tmp.len());
                        tmp[..len].copy_from_slice(&raw[16..16 + len]);
                        (&mut (*mfs).user_path)[..len].copy_from_slice(&tmp[..len]);
                    }
                    (&mut (*mfs).user_path)[copy_len] = 0;
                }
            }
            // Dispatch the request (handler may populate m_out payload).
            let status = crate::mfs::table::dispatch(req_nr);

            // For lookup responses (OK), populate m_out with result fields.
            if req_nr == 26 && status == 0 {
                unsafe {
                    let mfs = glo::mfs_ptr();
                    let raw = &mut (*mfs).m_out.m_payload.raw;
                    let file_size = (*mfs).lookup_res_file_size as i64;
                    raw[8..16].copy_from_slice(&file_size.to_le_bytes());
                    raw[16..20].copy_from_slice(&(*mfs).lookup_res_device.to_le_bytes());
                    raw[20..24].copy_from_slice(&(*mfs).lookup_res_inode.to_le_bytes());
                    raw[24..28].copy_from_slice(&((*mfs).lookup_res_mode as u32).to_le_bytes());
                }
            }

            // For create (23) responses, populate m_out with result fields.
            if req_nr == 23 && status >= 0 {
                unsafe {
                    let mfs = glo::mfs_ptr();
                    let raw = &mut (*mfs).m_out.m_payload.raw;
                    // fs_create stored result in cch[]:
                    //   cch[0] = i_num (inode)
                    //   cch[1] = i_mode
                    //   cch[2] = i_size
                    //   cch[3] = i_uid
                    //   cch[4] = i_gid
                    // VFS req_create expects:
                    //   payload[0..8]  = file_size (i64)
                    //   payload[8..12] = inode_nr (u32)
                    //   payload[12..14] = mode (u16)
                    raw[0..8].copy_from_slice(&((*mfs).cch[2] as i64).to_le_bytes()); // file_size
                    raw[8..12].copy_from_slice(&((*mfs).cch[0] as u32).to_le_bytes()); // inode_nr
                    raw[12..14].copy_from_slice(&((*mfs).cch[1] as u16).to_le_bytes()); // mode
                }
            }

            // For read/write responses, populate m_out with result fields.
            if (req_nr == 19 || req_nr == 20) && status >= 0 {
                unsafe {
                    let mfs = glo::mfs_ptr();
                    let raw = &mut (*mfs).m_out.m_payload.raw;
                    raw[0..8].copy_from_slice(&(*mfs).readwrite_res_pos.to_le_bytes());
                    raw[8..12].copy_from_slice(&(*mfs).readwrite_res_count.to_le_bytes());
                }
            }

            // Build the reply using m_out's payload (handler-set fields).
            let reply_payload = unsafe { (*glo::mfs_ptr()).m_out.m_payload };
            let mut reply = arch_common::ipc::Message {
                m_source: 0,
                m_type: status,
                m_payload: reply_payload,
            };
            // Store the reply in global state, then send.
            unsafe {
                (*glo::mfs_ptr()).m_out = reply.clone();
            }
            let _ = unsafe {
                minix_rt::syscall2(
                    SEND_CALL,
                    src as u64,
                    &mut reply as *mut arch_common::ipc::Message as u64,
                )
            };
        }
    }
    #[cfg(not(target_os = "minix"))]
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

    /// Pin the VFS→MFS mknod wire format: a payload built exactly as
    /// `vfs/request.rs` `build_mknod_msg` writes it must parse into the
    /// fields `fs_mknod` reads. This would have caught the pre-Stage-3
    /// break where req_mknod used a grant-based layout MFS never parsed
    /// (mknod silently created nothing).
    #[test]
    fn test_parse_mknod_request_layout() {
        let mut raw = [0u8; 48];
        raw[0..4].copy_from_slice(&7u32.to_le_bytes()); // dir_ino
        raw[4..6].copy_from_slice(&0o10000u16.to_le_bytes()); // mode (FIFO)
        raw[6..8].copy_from_slice(&0u16.to_le_bytes()); // uid
        raw[8..10].copy_from_slice(&0u16.to_le_bytes()); // gid
        raw[10..14].copy_from_slice(&0u32.to_le_bytes()); // device
        raw[14..18].copy_from_slice(&10u32.to_le_bytes()); // path_len (name + NUL)
        raw[18..28].copy_from_slice(b"shellfifo\0"); // name

        let mut cch = [0i32; 8];
        let mut user_path = [0u8; PATH_MAX];
        parse_mknod_request(&raw, &mut cch, &mut user_path);

        assert_eq!(cch[0], 7); // dir_ino
        assert_eq!(cch[1], 0o10000); // mode
        assert_eq!(cch[2], 0); // device
        assert_eq!(cch[3], 0); // uid
        assert_eq!(cch[4], 0); // gid
        assert_eq!(&user_path[..9], b"shellfifo");
        assert_eq!(user_path[9], 0);
    }

    /// A truncated/zeroed payload must parse defensively (no panic, empty
    /// name) rather than corrupting the request fields.
    #[test]
    fn test_parse_mknod_request_short_payload() {
        let raw = [0u8; 4];
        let mut cch = [9i32; 8];
        let mut user_path = [b'x'; PATH_MAX];
        parse_mknod_request(&raw, &mut cch, &mut user_path);
        assert_eq!(cch[0], 0);
        assert_eq!(cch[1], 0);
        assert_eq!(user_path[0], 0); // empty name, null-terminated
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
