//! File timestamps — adapted from `minix/fs/mfs/time.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::protect::read_only;

/// Update a file's access/modification times (VFS REQ_UTIME, FS_BASE + 9).
///
/// Matching C: `fs_utime()` in `minix/fs/mfs/time.c`. Wire layout (VFS
/// `req_utime`, C `mess_vfs_fs_utime`): inode u32@payload[0], actime
/// i64@8, modtime i64@16, acnsec u32@24, modnsec u32@28. A nanosecond
/// field of UTIME_NOW (-1) flags the corresponding time so the inode
/// write-back stamps the current clock; UTIME_OMIT (-2) leaves the field
/// alone; any other value stores the given time (second resolution).
pub fn fs_utime() -> i32 {
    unsafe {
        let mfs = glo::mfs_ptr();
        let payload = &(*mfs).m_in.m_payload.raw;
        let ino = u32::from_ne_bytes(payload[0..4].try_into().unwrap_or([0u8; 4]));
        let actime = i64::from_ne_bytes(payload[8..16].try_into().unwrap_or([0u8; 8]));
        let modtime = i64::from_ne_bytes(payload[16..24].try_into().unwrap_or([0u8; 8]));
        let acnsec = u32::from_ne_bytes(payload[24..28].try_into().unwrap_or([0u8; 4]));
        let modnsec = u32::from_ne_bytes(payload[28..32].try_into().unwrap_or([0u8; 4]));

        let rip = match get_inode((*mfs).fs_dev, ino) {
            Some(i) => i,
            None => return EINVAL,
        };
        let r = read_only(rip);
        if r == OK {
            let rip_ptr = glo::get_inode_ptr(rip as usize);
            // C: rip->i_update = CTIME — discard any stale ATIME/MTIME flags.
            (*rip_ptr).i_update = CTIME;
            match acnsec {
                x if x == UTIME_NOW as u32 => (*rip_ptr).i_update |= ATIME,
                x if x == UTIME_OMIT as u32 => {}
                _ => (*rip_ptr).i_atime = actime as u32, // second resolution
            }
            match modnsec {
                x if x == UTIME_NOW as u32 => (*rip_ptr).i_update |= MTIME,
                x if x == UTIME_OMIT as u32 => {}
                _ => (*rip_ptr).i_mtime = modtime as u32,
            }
            (*rip_ptr).i_dirt = IN_DIRTY;
        }
        put_inode(Some(rip));
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reset mfs globals and hand back a writable superblock (the
    /// protect.rs test pattern).
    fn init_writable() -> *mut crate::mfs::types::SuperBlock {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            // Reset the inode hash table and unused list so that
            // get_inode / find_inode start from a clean slate.
            *crate::mfs::glo::UNUSED_INODES_HEAD.get() = Some(0);
            let p = crate::mfs::glo::HASH_INODES.get();
            for i in 0..crate::mfs::consts::INODE_HASH_SIZE {
                core::ptr::addr_of_mut!((*p)[i]).write(None);
            }
            let sp = crate::mfs::glo::get_super_ptr(0);
            (*sp).s_rd_only = 0;
            sp
        }
    }

    /// Build a utime request message (VFS req_utime wire layout).
    fn set_utime_req(ino: u32, actime: i64, modtime: i64, acnsec: u32, modnsec: u32) {
        unsafe {
            let mfs = glo::mfs_ptr();
            let raw = &mut (*mfs).m_in.m_payload.raw;
            raw[0..4].copy_from_slice(&ino.to_le_bytes());
            raw[8..16].copy_from_slice(&actime.to_le_bytes());
            raw[16..24].copy_from_slice(&modtime.to_le_bytes());
            raw[24..28].copy_from_slice(&acnsec.to_le_bytes());
            raw[28..32].copy_from_slice(&modnsec.to_le_bytes());
        }
    }

    #[test]
    fn test_fs_utime_returns_einval_when_uninitialized() {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            // Reset the inode hash table and unused list so that
            // get_inode / find_inode start from a clean slate.
            *crate::mfs::glo::UNUSED_INODES_HEAD.get() = None;
            let p = crate::mfs::glo::HASH_INODES.get();
            for i in 0..crate::mfs::consts::INODE_HASH_SIZE {
                let elem = core::ptr::addr_of_mut!((*p)[i]);
                elem.write(None);
            }
            set_utime_req(7, 1000, 2000, 0, 0);
        }
        // No inode 7 exists → get_inode fails → EINVAL.
        assert_eq!(fs_utime(), EINVAL);
    }

    #[test]
    fn test_fs_utime_utime_now_flags_atime_mtime() {
        unsafe {
            let sp = init_writable();
            let mfs = glo::mfs_ptr();
            let ino = get_inode((*mfs).fs_dev, 7).expect("inode alloc");
            let rip = glo::get_inode_ptr(ino as usize);
            (*rip).i_sp = Some(sp);
            (*rip).i_atime = 1;
            (*rip).i_mtime = 2;
            (*rip).i_update = 0;

            set_utime_req(7, 0, 0, UTIME_NOW as u32, UTIME_NOW as u32);
            assert_eq!(fs_utime(), OK);
            assert_ne!((*rip).i_update & ATIME, 0, "atime flagged");
            assert_ne!((*rip).i_update & MTIME, 0, "mtime flagged");
            assert_ne!((*rip).i_update & CTIME, 0, "ctime always flagged");
            assert_eq!((*rip).i_dirt, IN_DIRTY, "inode marked dirty");
        }
    }

    #[test]
    fn test_fs_utime_explicit_times_stored() {
        unsafe {
            let sp = init_writable();
            let mfs = glo::mfs_ptr();
            let ino = get_inode((*mfs).fs_dev, 7).expect("inode alloc");
            let rip = glo::get_inode_ptr(ino as usize);
            (*rip).i_sp = Some(sp);
            (*rip).i_atime = 1;
            (*rip).i_mtime = 2;

            set_utime_req(7, 1000, 2000, 0, 0); // acnsec/modnsec 0 → store
            assert_eq!(fs_utime(), OK);
            assert_eq!((*rip).i_atime, 1000);
            assert_eq!((*rip).i_mtime, 2000);
            // Only CTIME is flagged — stale ATIME/MTIME flags are discarded.
            assert_eq!((*rip).i_update, CTIME);
            assert_eq!((*rip).i_dirt, IN_DIRTY);
        }
    }

    #[test]
    fn test_fs_utime_omit_leaves_times_alone() {
        unsafe {
            let sp = init_writable();
            let mfs = glo::mfs_ptr();
            let ino = get_inode((*mfs).fs_dev, 7).expect("inode alloc");
            let rip = glo::get_inode_ptr(ino as usize);
            (*rip).i_sp = Some(sp);
            (*rip).i_atime = 1;
            (*rip).i_mtime = 2;

            set_utime_req(7, 0, 0, UTIME_OMIT as u32, UTIME_OMIT as u32);
            assert_eq!(fs_utime(), OK);
            assert_eq!((*rip).i_atime, 1, "atime untouched");
            assert_eq!((*rip).i_mtime, 2, "mtime untouched");
            assert_eq!((*rip).i_update, CTIME, "only ctime flagged");
        }
    }

    #[test]
    fn test_fs_utime_read_only_returns_erofs() {
        unsafe {
            let sp = init_writable();
            let mfs = glo::mfs_ptr();
            let ino = get_inode((*mfs).fs_dev, 7).expect("inode alloc");
            let rip = glo::get_inode_ptr(ino as usize);
            (*rip).i_sp = Some(sp);
            (*rip).i_atime = 1;
            (*rip).i_mtime = 2;
            (*sp).s_rd_only = 1;

            set_utime_req(7, 1000, 2000, 0, 0);
            assert_eq!(fs_utime(), EROFS);
            assert_eq!((*rip).i_atime, 1, "times untouched on r/o fs");
            assert_eq!((*rip).i_mtime, 2);
            assert_eq!((*rip).i_update, 0, "no flags set on r/o fs");
            assert_eq!((*rip).i_dirt, IN_CLEAN);
        }
    }
}
