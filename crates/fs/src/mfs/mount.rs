//! Mount/unmount operations — adapted from `minix/fs/mfs/mount.c`

use core::sync::atomic::{AtomicI32, Ordering};

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::super_block::*;
use crate::mfs::types::*;

static CLEANMOUNT: AtomicI32 = AtomicI32::new(1);

pub fn fs_readsuper() -> i32 {
    unsafe {
        // Read device number from the incoming message (m1i1).
        // VFS's req_readsuper writes dev at PAYLOAD_OFF + 0 → m1.m1i1.
        let mfs = glo::mfs_ptr();
        let dev = (*mfs).m_in.m_payload.m1.m1i1 as u32;

        // fs_readsuper called
        for i in 0..8 {
            let sp = glo::get_super_ptr(i);
            if (*sp).s_dev == NO_DEV {
                (*sp).s_dev = dev;
                let r = read_super(&mut *sp);
                if r != OK {
                    (*sp).s_dev = NO_DEV;
                    return r;
                }
                if (*sp).s_flags & MFSFLAG_CLEAN != 0 {
                    CLEANMOUNT.store(1, Ordering::Relaxed);
                }
                let root_rip = match get_inode(dev, ROOT_INODE) {
                    Some(rip) => rip,
                    None => {
                        (*sp).s_dev = NO_DEV;
                        return EINVAL;
                    }
                };

                // Fill reply payload with root inode info for VFS.
                // VFS req_readsuper expects reply fields at:
                //   file_size (i64) at PAYLOAD_OFF+0 → m1.m1i1 (low) + m1i2 (high)
                //   dev       (u32) at PAYLOAD_OFF+8 → m1.m1i3
                //   inode_nr  (u32) at PAYLOAD_OFF+12 → m1.m1i4
                //   flags     (u32) at PAYLOAD_OFF+16 → m1.m1i5
                //   mode (u16) at PAYLOAD_OFF+20 → low 16 of m1i6
                let root_inode = &*glo::get_inode_ptr(root_rip as usize);
                (*mfs).m_out.m_payload.m1.m1i1 = root_inode.i_size;
                (*mfs).m_out.m_payload.m1.m1i2 = if root_inode.i_size < 0 { -1 } else { 0 };
                (*mfs).m_out.m_payload.m1.m1i3 = dev as i32;
                (*mfs).m_out.m_payload.m1.m1i4 = ROOT_INODE as i32;
                (*mfs).m_out.m_payload.m1.m1i5 = 0; // flags: not read-only during boot
                (*mfs).m_out.m_payload.m1.m1i6 = root_inode.i_mode as i32;

                return OK;
            }
        }
        EINVAL
    }
}

pub fn fs_unmount() -> i32 {
    unsafe {
        let mfs = glo::mfs_ptr();
        if (*mfs).super_blocks[0].s_dev != (*mfs).fs_dev {
            return EINVAL;
        }
        let mut count = 0;
        for i in 0..NR_INODES {
            let inode = &*glo::get_inode_ptr(i);
            if (*inode).i_count > 0 && (*inode).i_dev == (*mfs).fs_dev {
                count += (*inode).i_count;
            }
        }
        let root_ip = find_inode((*mfs).fs_dev, ROOT_INODE);
        if root_ip.is_none() || count > 1 {
            return if count > 1 { EBUSY } else { EINVAL };
        }
        put_inode(root_ip);
        if CLEANMOUNT.load(Ordering::Relaxed) != 0 && (*mfs).super_blocks[0].s_rd_only == 0 {
            (*mfs).super_blocks[0].s_flags |= MFSFLAG_CLEAN;
        }
        (*mfs).super_blocks[0].s_dev = NO_DEV;
        (*mfs).unmountdone = TRUE;
        OK
    }
}

pub fn fs_mountpoint() -> i32 {
    unsafe {
        let inode_nr: u32 = 0;
        let mfs = glo::mfs_ptr();
        let rip = match get_inode((*mfs).fs_dev, inode_nr) {
            Some(idx) => idx,
            None => return EINVAL,
        };
        let inode = &*glo::get_inode_ptr(rip as usize);
        let mut r = OK;
        if (*inode).i_mountpoint != FALSE {
            r = EBUSY;
        }
        let bits = (*inode).i_mode & I_TYPE;
        if bits == I_BLOCK_SPECIAL || bits == I_CHAR_SPECIAL {
            r = ENOTDIR;
        }
        put_inode(Some(rip));
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: initialize global MFS state so functions that
    /// dereference mfs_ptr / the inode table can be called.
    /// Tests in this module are NOT thread-safe (the C port is
    /// single-threaded); run with `--test-threads=1` if needed.
    fn init() {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            // Reset the inode hash table and unused list so that
            // get_inode / find_inode start from a clean slate
            // (mfs_init_globals only resets MFS_STORAGE, not these
            //  separate static mut variables).
            *crate::mfs::glo::UNUSED_INODES_HEAD.get() = None;
            let p = crate::mfs::glo::HASH_INODES.get();
            for i in 0..crate::mfs::consts::INODE_HASH_SIZE {
                let elem = core::ptr::addr_of_mut!((*p)[i]);
                elem.write(None);
            }
        }
    }

    #[test]
    fn test_fs_unmount_returns_einval_when_uninitialized() {
        // After init, no filesystem is mounted:
        //   super_blocks[0].s_dev == NO_DEV (same as fs_dev == NO_DEV),
        //   all inodes have i_count == 0,
        //   root inode is not in the hash table → EINVAL.
        init();
        assert_eq!(fs_unmount(), EINVAL);
    }

    #[test]
    fn test_fs_mountpoint_returns_einval_when_uninitialized() {
        // After init, fs_dev == NO_DEV and the inode hash table is
        // empty, so get_inode fails → EINVAL.
        init();
        assert_eq!(fs_mountpoint(), EINVAL);
    }
}
