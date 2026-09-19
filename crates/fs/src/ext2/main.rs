//! Main server loop — adapted from `minix/fs/ext2/main.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::inode::*;
use crate::ext2::misc::*;
use crate::ext2::table::*;

/// IPC receive/send syscall numbers. Only used when compiling for the MINIX
/// target; the library build (`cargo check`) compiles without
/// `target_os = "minix"`.
#[cfg(target_os = "minix")]
const RECEIVE_CALL: u64 = 47;
#[cfg(target_os = "minix")]
const SEND_CALL: u64 = 46;
#[allow(dead_code)]
const ANY: i32 = 0x0000ffff;

/// Virtual address of the boot filesystem image in the ramdisk driver
/// server's address space (set up by the kernel boot code). Only used by the
/// host test path, which fakes a direct-memory RAM disk at this address.
#[cfg(not(target_os = "minix"))]
const RAMDISK_IMAGE_VA: u64 = arch_common::com::RAMDISK_IMAGE_VA;
#[cfg(not(target_os = "minix"))]
const RAMDISK_IMAGE_SIZE: usize = arch_common::com::RAMDISK_IMAGE_SIZE;

/// Initialize the ext2 file server.
///
/// Reference: main.c sef_cb_init_fresh()
pub unsafe fn init_server() -> i32 {
    // Initialize globals
    glo::ext2_init_globals();

    // Set default options
    let opt_ptr = glo::OPT.get();
    (*opt_ptr).use_orlov = TRUE;
    (*opt_ptr).mfsalloc = FALSE;
    (*opt_ptr).use_reserved_blocks = FALSE;
    (*opt_ptr).block_with_super = 0;
    (*opt_ptr).use_prealloc = FALSE;

    // Init inode table
    for i in 0..NR_INODES {
        let rip = glo::get_inode_ptr(i);
        (*rip).i_count = 0;
    }

    init_inode_cache();

    // Initialise the buffer cache. The 1024-byte block size is the pre-mount
    // default the superblock read relies on (the ext2 superblock sits at byte
    // 1024); `read_super` switches the cache to the filesystem's own block
    // size once it has been read.
    libs::libminixfs::cache::lmfs_buf_pool(DEFAULT_NR_BUFS as i32);
    libs::libminixfs::cache::lmfs_set_blocksize(1024, 0);

    // Register the block I/O callback. An ext2 volume lives on a device owned
    // by a block driver, not on the boot ramdisk: each request is routed by
    // device major to the driver endpoint `fs_readsuper` registered.
    #[cfg(target_os = "minix")]
    {
        crate::block_io::bdev_init();
        libs::libminixfs::cache::lmfs_set_block_io(crate::block_io::bdev_ram_disk_io);
    }
    #[cfg(not(target_os = "minix"))]
    {
        // Host tests: direct-memory RAM disk (no driver server).
        crate::block_io::ram_disk_init(RAMDISK_IMAGE_VA as *const u8, RAMDISK_IMAGE_SIZE);
        libs::libminixfs::cache::lmfs_set_block_io(crate::block_io::ram_disk_io);
    }

    OK
}

/// Parse a REQ_MKNOD message payload (VFS `req_mknod`'s embedded-path layout)
/// into the fields `fs_mknod` reads: cch[0] = dir_ino, cch[1] = mode,
/// cch[2] = device, cch[3] = uid, cch[4] = gid, plus the entry name in
/// user_path (null-terminated).
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

    cch[0] = rd32(0) as i32;
    cch[1] = rd16(4) as i32;
    cch[2] = rd32(10) as i32;
    cch[3] = rd16(6) as i32;
    cch[4] = rd16(8) as i32;

    let path_len = rd32(14) as usize;
    let copy_len = path_len
        .min(PATH_MAX - 1)
        .min(30)
        .min(raw.len().saturating_sub(18));
    if copy_len > 0 {
        user_path[..copy_len].copy_from_slice(&raw[18..18 + copy_len]);
    }
    user_path[copy_len] = 0;
}

/// Parse a REQ_CREATE / REQ_MKDIR payload into cch[0] = dir_ino,
/// cch[1] = mode, cch[2] = uid, cch[3] = gid, with the entry name in
/// user_path (null-terminated).
///
/// Payload-relative byte offsets (VFS `build_create_msg`/`build_mkdir_msg`):
///   raw[0..4]   = dir_ino (u32)
///   raw[4..6]   = mode (u16)
///   raw[6..8]   = uid (u16)
///   raw[8..10]  = gid (u16)
///   raw[12..16] = path_len (u32)
///   raw[16..]   = name (up to 28 bytes, null-terminated)
#[cfg(any(test, target_os = "minix"))]
pub(crate) fn parse_dir_create_request(raw: &[u8], cch: &mut [i32], user_path: &mut [u8]) {
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

    cch[0] = rd32(0) as i32;
    cch[1] = rd16(4) as i32;
    cch[2] = rd16(6) as i32;
    cch[3] = rd16(8) as i32;

    let path_len = rd32(12) as usize;
    let copy_len = path_len
        .min(PATH_MAX - 1)
        .min(28)
        .min(raw.len().saturating_sub(16));
    if copy_len > 0 {
        user_path[..copy_len].copy_from_slice(&raw[16..16 + copy_len]);
    }
    user_path[copy_len] = 0;
}

/// Main message processing loop.
///
/// Path-carrying requests (mknod/create/mkdir) are unpacked into `cch` and
/// `user_path` here, as in the C original; handlers read the remaining scalar
/// fields out of `m_in` and build their reply in `m_out`.
///
/// Reference: main.c main()
pub fn ext2_main() -> i32 {
    #[cfg(target_os = "minix")]
    {
        unsafe {
            init_server();
        }

        loop {
            unsafe {
                let ext2 = glo::ext2_ptr();

                // Termination condition: VFS unmounted the volume and the
                // server took the EXIT signal.
                if (*ext2).unmountdone != 0 && (*ext2).exitsignaled != 0 {
                    break;
                }

                let mut msg = arch_common::ipc::Message {
                    m_source: 0,
                    m_type: 0,
                    m_payload: core::mem::zeroed(),
                };

                // syscall2(RECEIVE_CALL, src=ANY, msg_ptr) → sender endpoint
                let src = minix_rt::syscall2(
                    RECEIVE_CALL,
                    ANY as u64,
                    &mut msg as *mut arch_common::ipc::Message as u64,
                );
                if src < 0 {
                    continue;
                }

                let req_nr = (msg.m_type - FS_BASE) as usize;
                let (caller_uid, caller_gid) =
                    (msg.m_payload.m1.m1i1 as u16, msg.m_payload.m1.m1i2 as u16);

                (*ext2).m_in = msg;
                (*ext2).req_nr = req_nr as i32;
                (*ext2).caller_uid = caller_uid;
                (*ext2).caller_gid = caller_gid;

                let raw = &(*ext2).m_in.m_payload.raw;
                let cch = core::ptr::addr_of_mut!((*ext2).cch);
                let user_path = core::ptr::addr_of_mut!((*ext2).user_path);
                match req_nr {
                    // REQ_MKNOD
                    21 => parse_mknod_request(raw, &mut *cch, &mut *user_path),
                    // REQ_MKDIR, REQ_CREATE
                    22 | 23 => parse_dir_create_request(raw, &mut *cch, &mut *user_path),
                    _ => {}
                }

                let status = dispatch(req_nr);

                // Handlers build their reply in `m_out` (C: fs_m_out), so the
                // loop only stamps the status and sends it.
                (*ext2).m_out.m_type = status;
                let reply = core::ptr::addr_of_mut!((*ext2).m_out);
                let _ = minix_rt::syscall2(SEND_CALL, src as u64, reply as u64);
            }
        }
        OK
    }
    #[cfg(not(target_os = "minix"))]
    {
        unsafe { init_server() }
    }
}

/// Signal handler for cleanup.
///
/// Reference: main.c sef_cb_signal_handler()
pub unsafe fn signal_handler(_signo: i32) {
    let ext2 = glo::ext2_ptr();
    (*ext2).exitsignaled = 1;
    fs_sync();

    if (*ext2).unmountdone != 0 {
        // exit(0) would be called here in C
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mknod_request_layout() {
        let mut raw = [0u8; 32];
        raw[0..4].copy_from_slice(&7u32.to_le_bytes());
        raw[4..6].copy_from_slice(&0o20644u16.to_le_bytes());
        raw[6..8].copy_from_slice(&1u16.to_le_bytes());
        raw[8..10].copy_from_slice(&2u16.to_le_bytes());
        raw[10..14].copy_from_slice(&0x0103u32.to_le_bytes());
        raw[14..18].copy_from_slice(&3u32.to_le_bytes());
        raw[18..21].copy_from_slice(b"dev");

        let mut cch = [0i32; 8];
        let mut path = [0u8; PATH_MAX];
        parse_mknod_request(&raw, &mut cch, &mut path);

        assert_eq!(cch[0], 7);
        assert_eq!(cch[1], 0o20644);
        assert_eq!(cch[2], 0x0103);
        assert_eq!(cch[3], 1);
        assert_eq!(cch[4], 2);
        assert_eq!(&path[..4], b"dev\0");
    }

    #[test]
    fn test_parse_mknod_request_short_payload() {
        let raw = [0u8; 4];
        let mut cch = [0i32; 8];
        let mut path = [0xffu8; PATH_MAX];
        parse_mknod_request(&raw, &mut cch, &mut path);

        assert_eq!(cch[..5], [0, 0, 0, 0, 0]);
        assert_eq!(path[0], 0);
    }

    #[test]
    fn test_parse_dir_create_request_layout() {
        let mut raw = [0u8; 32];
        raw[0..4].copy_from_slice(&9u32.to_le_bytes());
        raw[4..6].copy_from_slice(&0o40755u16.to_le_bytes());
        raw[6..8].copy_from_slice(&5u16.to_le_bytes());
        raw[8..10].copy_from_slice(&6u16.to_le_bytes());
        raw[12..16].copy_from_slice(&4u32.to_le_bytes());
        raw[16..20].copy_from_slice(b"file");

        let mut cch = [0i32; 8];
        let mut path = [0u8; PATH_MAX];
        parse_dir_create_request(&raw, &mut cch, &mut path);

        assert_eq!(cch[0], 9);
        assert_eq!(cch[1], 0o40755);
        assert_eq!(cch[2], 5);
        assert_eq!(cch[3], 6);
        assert_eq!(&path[..5], b"file\0");
    }

    #[test]
    fn test_parse_dir_create_request_terminates_long_name() {
        // A maximum-length name still gets a NUL terminator, and a path_len
        // past the payload must not read past the end of it.
        let mut raw = [0u8; 16 + 28];
        raw[12..16].copy_from_slice(&28u32.to_le_bytes());
        raw[16..44].fill(b'x');

        let mut cch = [0i32; 8];
        let mut path = [0u8; PATH_MAX];
        parse_dir_create_request(&raw, &mut cch, &mut path);

        assert!(path[..28].iter().all(|b| *b == b'x'));
        assert_eq!(path[28], 0);
    }
}
