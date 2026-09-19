//! stat/statvfs — adapted from `minix/fs/ext2/stadir.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::inode::*;
use crate::ext2::super_::*;
use crate::ext2::types::Inode;
use crate::stat::{Stat, Statvfs};

/// Build the `Stat` for `rip`.
///
/// Reference: stadir.c stat_inode()
pub unsafe fn build_stat(rip: *mut Inode) -> Option<Stat> {
    // An inode without a mounted superblock has no block size to report
    // (the C original dereferences rip->i_sp unconditionally).
    let sp = match (*rip).i_sp {
        Some(ref sp) => sp,
        None => return None,
    };

    let special = {
        let mo = (*rip).i_mode & I_TYPE;
        mo == I_CHAR_SPECIAL || mo == I_BLOCK_SPECIAL
    };

    Some(Stat {
        st_dev: (*rip).i_dev as u64,
        st_ino: (*rip).i_num as u64,
        st_mode: (*rip).i_mode as u32,
        st_nlink: (*rip).i_links_count as u32,
        st_uid: (*rip).i_uid as u32,
        st_gid: (*rip).i_gid as u32,
        st_rdev: if special {
            (*rip).i_block[0] as u64
        } else {
            NO_DEV as u64
        },
        st_size: (*rip).i_size as i64,
        st_blksize: sp.s_block_size as i64,
        st_blocks: (*rip).i_blocks as i64,
        st_atime: (*rip).i_atime as i64,
        st_mtime: (*rip).i_mtime as i64,
        st_ctime: (*rip).i_ctime as i64,
    })
}

/// Fill a `Stat` for `rip` and copy it to the caller's buffer through grant
/// `gid`. `who_e` is the granter (VFS) endpoint.
///
/// Reference: stadir.c stat_inode()
fn stat_inode(rip: *mut Inode, who_e: i32, gid: i32) -> i32 {
    unsafe {
        // Update the atime/ctime/mtime fields in the inode, if need be.
        if (*rip).i_update != 0 {
            update_times(rip);
        }

        let stat = match build_stat(rip) {
            Some(stat) => stat,
            None => return EINVAL,
        };

        let bytes = core::slice::from_raw_parts(
            &stat as *const Stat as *const u8,
            core::mem::size_of::<Stat>(),
        );
        crate::block_io::safecopy_to(who_e, gid, bytes)
    }
}

/// fs_stat — stat a file by inode number.
///
/// Message layout (VFS `req_stat`): inode (u32) at payload[0], grant (i32) at
/// payload[8].
///
/// Reference: stadir.c fs_stat()
pub unsafe fn fs_stat() -> i32 {
    let ext2 = glo::ext2_ptr();
    let inode_nr = (*ext2).m_in.m_payload.m1.m1i1 as u32;
    let gid = (*ext2).m_in.m_payload.m1.m1i3;
    let who = (*ext2).m_in.m_source;

    let rip = get_inode((*ext2).fs_dev, inode_nr);
    if rip.is_null() {
        return EINVAL;
    }

    let r = stat_inode(rip, who, gid);
    put_inode(rip);
    r
}

/// fs_statvfs — stat the mounted filesystem.
///
/// Message layout (VFS `req_statvfs`): grant (i32) at payload[0].
///
/// Reference: stadir.c fs_statvfs()
pub unsafe fn fs_statvfs() -> i32 {
    let ext2 = glo::ext2_ptr();
    let gid = (*ext2).m_in.m_payload.m1.m1i1;
    let who = (*ext2).m_in.m_source;

    let sp = get_super((*ext2).fs_dev);
    if sp.is_null() {
        return EINVAL;
    }
    let sp = &*sp;

    let mut st = Statvfs::default();
    st.f_bsize = sp.s_block_size as u32;
    st.f_frsize = sp.s_block_size as u32;
    st.f_blocks = sp.s_blocks_count as u64;
    st.f_bfree = sp.s_free_blocks_count as u64;
    st.f_bavail = sp.s_free_blocks_count.saturating_sub(sp.s_r_blocks_count) as u64;
    st.f_files = sp.s_inodes_count as u64;
    st.f_ffree = sp.s_free_inodes_count as u64;
    st.f_favail = sp.s_free_inodes_count as u64;
    st.f_namemax = EXT2_NAME_MAX as u64;

    let bytes = core::slice::from_raw_parts(
        &st as *const Statvfs as *const u8,
        core::mem::size_of::<Statvfs>(),
    );
    crate::block_io::safecopy_to(who, gid, bytes)
}

/// fs_blockstats — get block statistics.
pub unsafe fn fs_blockstats(blocks: &mut u64, free: &mut u64, used: &mut u64) {
    let ext2 = glo::ext2_ptr();
    let sp = get_super((*ext2).fs_dev);
    if sp.is_null() {
        return;
    }
    *blocks = (*sp).s_blocks_count as u64;
    *free = (*sp).s_free_blocks_count as u64;
    *used = *blocks - *free;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stat_layout_is_wire_sized() {
        // The layout is written straight into a user buffer, so its size is
        // part of the protocol.
        assert_eq!(core::mem::size_of::<Stat>(), 88);
        assert_eq!(core::mem::offset_of!(Stat, st_size), 40);
        assert_eq!(core::mem::offset_of!(Stat, st_blocks), 56);
    }

    #[test]
    fn test_fs_stat_without_super_returns_einval() {
        unsafe {
            // Only the ext2 globals: the block cache is shared with the other
            // filesystem servers' tests, so don't initialise it here.
            glo::ext2_init_globals();
            let ext2 = glo::ext2_ptr();
            (*ext2).fs_dev = NO_DEV;
            (*ext2).m_in.m_source = 1;
            (*ext2).m_in.m_payload.m1.m1i1 = 1;
            (*ext2).m_in.m_payload.m1.m1i3 = 0;
            assert_eq!(fs_stat(), EINVAL);
        }
    }

    #[test]
    fn test_fs_statvfs_without_super_returns_einval() {
        unsafe {
            glo::ext2_init_globals();
            let ext2 = glo::ext2_ptr();
            (*ext2).fs_dev = NO_DEV;
            (*ext2).m_in.m_source = 1;
            (*ext2).m_in.m_payload.m1.m1i1 = 0;
            assert_eq!(fs_statvfs(), EINVAL);
        }
    }

    #[test]
    fn test_fs_blockstats_counts_used_blocks() {
        unsafe {
            glo::ext2_init_globals();
            let sp = crate::ext2::mount::allocate_superblock();
            (*sp).s_dev = 3;
            (*sp).s_blocks_count = 100;
            (*sp).s_free_blocks_count = 30;
            glo::SUPERBLOCK.store(sp, core::sync::atomic::Ordering::Relaxed);
            let ext2 = glo::ext2_ptr();
            (*ext2).fs_dev = 3;

            let (mut blocks, mut free, mut used) = (0u64, 0u64, 0u64);
            fs_blockstats(&mut blocks, &mut free, &mut used);
            assert_eq!((blocks, free, used), (100, 30, 70));
        }
    }
}
