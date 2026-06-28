//! File timestamps — adapted from `minix/fs/mfs/time.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::protect::*;

pub fn fs_utime() -> i32 {
    unsafe {
        let _inode_nr: u32 = 0;
        let _acnsec: i64 = 0;
        let _actime: i64 = 0;
        let _modnsec: i64 = 0;
        let _modtime: i64 = 0;
        let rip_idx = match get_inode((*glo::mfs_ptr()).fs_dev, _inode_nr) {
            Some(i) => i,
            None => return EINVAL,
        };
        let r = read_only(rip_idx);
        if r != OK {
            put_inode(Some(rip_idx));
            return r;
        }

        let rip = &mut *glo::get_inode_ptr(rip_idx as usize);
        (*rip).i_update = CTIME;
        if _acnsec == UTIME_NOW {
            (*rip).i_update |= ATIME;
        } else if _acnsec != UTIME_OMIT {
            (*rip).i_atime = _actime as u32;
        }
        if _modnsec == UTIME_NOW {
            (*rip).i_update |= MTIME;
        } else if _modnsec != UTIME_OMIT {
            (*rip).i_mtime = _modtime as u32;
        }

        (*rip).i_dirt = IN_DIRTY;
        put_inode(Some(rip_idx));
        OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            // Reset the inode hash table and unused list so that
            // get_inode / find_inode start from a clean slate.
            crate::mfs::glo::UNUSED_INODES_HEAD = None;
            let p = &raw mut crate::mfs::glo::HASH_INODES;
            for i in 0..crate::mfs::consts::INODE_HASH_SIZE {
                let elem = core::ptr::addr_of_mut!((*p)[i]);
                elem.write(None);
            }
        }
    }

    #[test]
    fn test_fs_utime_returns_einval_when_uninitialized() {
        // After init, fs_dev == NO_DEV and the inode hash table is
        // empty, so get_inode fails → EINVAL.
        init();
        assert_eq!(fs_utime(), EINVAL);
    }
}
