//! Miscellaneous operations — adapted from `minix/fs/mfs/misc.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;

pub fn fs_flush() -> i32 {
    OK
}

pub fn fs_sync() -> i32 {
    unsafe {
        for i in 0..NR_INODES {
            let rip = &*glo::get_inode_ptr(i);
            if (*rip).i_count > 0 && (*rip).i_dirt == IN_DIRTY {
                rw_inode(i as u16, WRITING);
            }
        }
        // Blocks flush last: rw_inode leaves its results in the block
        // cache (matches fs_sync in the C reference).
        libs::libminixfs::cache::lmfs_flushall();
    }
    OK
}

pub fn fs_new_driver() -> i32 {
    #[cfg(target_os = "none")]
    {
        // FS_NEW_DRIVER (REQ_NEW_DRIVER) message layout (VFS req_newdriver):
        //   payload raw[0..4]   = device (u32)
        //   payload raw[8..12]  = label grant id (i32)
        //   payload raw[16..20] = label length (u32)
        let mfs = unsafe { &*glo::mfs_ptr() };
        let dev = unsafe {
            u32::from_ne_bytes(mfs.m_in.m_payload.raw[0..4].try_into().unwrap_or([0u8; 4]))
        };
        let label_grant = unsafe {
            i32::from_ne_bytes(mfs.m_in.m_payload.raw[8..12].try_into().unwrap_or([0u8; 4]))
        };
        let label_len = unsafe {
            u32::from_ne_bytes(
                mfs.m_in.m_payload.raw[16..20]
                    .try_into()
                    .unwrap_or([0u8; 4]),
            ) as usize
        };
        let copy_len = label_len.min(LABEL_MAX - 1);
        let mut label = [0u8; LABEL_MAX];
        let r =
            crate::block_io::safecopy_from(mfs.m_in.m_source, label_grant, &mut label[..copy_len]);
        if r != 0 {
            return EINVAL;
        }
        crate::block_io::bdev_driver(dev, &label[..copy_len])
    }
    #[cfg(not(target_os = "none"))]
    {
        OK
    }
}

pub fn fs_bpeek() -> i32 {
    // Block peek stub: the real implementation delegates to
    // lmfs_do_bpeek(&fs_m_in). Return OK for now.
    OK
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_fs_sync() {
        assert_eq!(fs_sync(), OK);
    }
}
