//! stat/statvfs — adapted from `minix/fs/mfs/stadir.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::{get_inode, put_inode, update_times};
use crate::mfs::stats::*;
use crate::mfs::super_block::get_super;
use crate::mfs::types::*;
use libs::libminixfs::cache::lmfs_fs_block_size;

/// Estimate the number of 512-byte blocks used by a file: data zones plus
/// indirect zones. Disregards holes (conservative estimate), matching the
/// C implementation which never reads indirect blocks during stat.
///
/// Reference: stadir.c estimate_blocks()
pub fn estimate_blocks(rip_idx: u16) -> i64 {
    unsafe {
        let inode = &*glo::get_inode_ptr(rip_idx as usize);
        let sp = match (*inode).i_sp {
            Some(sp) => &*sp,
            None => return 0,
        };
        let zone_size = (sp.s_block_size as i64) << sp.s_log_zone_size;
        let zones = (inode.i_size as i64 + zone_size - 1) / zone_size;
        let nr_indirs = inode.i_nindirs as i64;
        let sq_indirs = nr_indirs * nr_indirs;
        let sindirs = (zones - inode.i_ndzones as i64 + nr_indirs - 1) / nr_indirs;
        let dindirs = (sindirs - 1 + sq_indirs - 1) / sq_indirs;
        (zones + sindirs + dindirs) * (zone_size / 512)
    }
}

/// Build the `Stat` for inode `rip_idx`.
///
/// Extracted from `stat_inode` so the field mapping is host-testable.
pub fn build_stat(rip_idx: u16) -> Stat {
    unsafe {
        let inode = &*glo::get_inode_ptr(rip_idx as usize);
        let mo = inode.i_mode & I_TYPE;
        let special = mo == I_CHAR_SPECIAL || mo == I_BLOCK_SPECIAL;
        Stat {
            st_dev: inode.i_dev as u64,
            st_ino: inode.i_num as u64,
            st_mode: inode.i_mode as u32,
            st_nlink: inode.i_nlinks as u32,
            st_uid: inode.i_uid as u32,
            st_gid: inode.i_gid as u32,
            st_rdev: if special {
                inode.i_zone[0] as u64
            } else {
                NO_DEV as u64
            },
            st_size: inode.i_size as i64,
            st_blksize: lmfs_fs_block_size() as i64,
            st_blocks: estimate_blocks(rip_idx),
            st_atime: inode.i_atime as i64,
            st_mtime: inode.i_mtime as i64,
            st_ctime: inode.i_ctime as i64,
        }
    }
}

/// Fill a `Stat` for inode `rip_idx` and copy it to the caller's buffer
/// through grant `gid`. `who_e` is the granter (VFS) endpoint.
///
/// Reference: stadir.c stat_inode()
fn stat_inode(rip_idx: u16, who_e: i32, gid: i32) -> i32 {
    unsafe {
        if (*glo::get_inode_ptr(rip_idx as usize)).i_update != 0 {
            update_times(rip_idx);
        }
        let stat = build_stat(rip_idx);
        let stat_bytes = core::slice::from_raw_parts(
            &stat as *const Stat as *const u8,
            core::mem::size_of::<Stat>(),
        );
        crate::block_io::safecopy_to(who_e, gid, stat_bytes)
    }
}

/// Stat a file by inode number.
///
/// Message layout (VFS `req_stat`): inode (u32) at payload[0], grant (i32)
/// at payload[8].
///
/// Reference: stadir.c fs_stat()
pub fn fs_stat() -> i32 {
    unsafe {
        let mfs = glo::mfs_ptr();
        let inode_nr = (*mfs).m_in.m_payload.m1.m1i1 as u32;
        let gid = (*mfs).m_in.m_payload.m1.m1i3;
        let who = (*mfs).m_in.m_source;
        let rip = match get_inode((*mfs).fs_dev, inode_nr) {
            Some(i) => i,
            None => return EINVAL,
        };
        let r = stat_inode(rip, who, gid);
        put_inode(Some(rip));
        r
    }
}

/// Stat the mounted filesystem.
///
/// Message layout (VFS `req_statvfs`): grant (i32) at payload[0].
///
/// Reference: stadir.c fs_statvfs()
pub fn fs_statvfs() -> i32 {
    unsafe {
        let mfs = glo::mfs_ptr();
        let gid = (*mfs).m_in.m_payload.m1.m1i1;
        let who = (*mfs).m_in.m_source;
        let sp = get_super((*mfs).fs_dev);
        if sp.is_null() {
            return EINVAL;
        }
        let sp = &*sp;

        let mut st = Statvfs::default();
        let mut used: u64 = 0;
        fs_blockstats(&mut st.f_blocks, &mut st.f_bfree, &mut used);
        st.f_bavail = st.f_bfree;

        st.f_bsize = (sp.s_block_size as u32) << sp.s_log_zone_size as u32;
        st.f_frsize = sp.s_block_size as u32;
        st.f_files = sp.s_ninodes as u64;
        st.f_ffree = count_free_bits(sp, IMAP) as u64;
        st.f_favail = st.f_ffree;
        st.f_namemax = MFS_DIRSIZ as u64;

        let st_bytes = core::slice::from_raw_parts(
            &st as *const Statvfs as *const u8,
            core::mem::size_of::<Statvfs>(),
        );
        crate::block_io::safecopy_to(who, gid, st_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_blocks_empty_file() {
        // Empty file: no zones, no indirect blocks → 0.
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            let inode = glo::get_inode_ptr(0);
            (*inode).i_size = 0;
            (*inode).i_ndzones = 7;
            (*inode).i_nindirs = 512;
            (*inode).i_sp = None;
            assert_eq!(estimate_blocks(0), 0);
        }
    }

    #[test]
    fn test_estimate_blocks_small_file() {
        // 1 zone file (4 KiB block): 1 data zone, no indirects → 8 * 512.
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            let inode = glo::get_inode_ptr(0);
            (*inode).i_size = 100;
            (*inode).i_ndzones = 7;
            (*inode).i_nindirs = 512;
            (*inode).i_sp = Some(crate::mfs::glo::get_super_ptr(0));
            let sp = crate::mfs::glo::get_super_ptr(0);
            (*sp).s_block_size = 4096;
            (*sp).s_log_zone_size = 0;
            assert_eq!(estimate_blocks(0), 8);
        }
    }

    #[test]
    fn test_estimate_blocks_spills_to_indirect() {
        // 8 zones at 4 KiB: 7 direct + 1 indirect entry → 1 indirect zone,
        // so 9 zones total → 72 * 512.
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            let inode = glo::get_inode_ptr(0);
            (*inode).i_size = 8 * 4096;
            (*inode).i_ndzones = 7;
            (*inode).i_nindirs = 512;
            (*inode).i_sp = Some(crate::mfs::glo::get_super_ptr(0));
            let sp = crate::mfs::glo::get_super_ptr(0);
            (*sp).s_block_size = 4096;
            (*sp).s_log_zone_size = 0;
            // zones=8, sindirs=(8-7+511)/512=1, dindirs=0 → 9 zones → 72
            assert_eq!(estimate_blocks(0), 72);
        }
    }

    #[test]
    fn test_build_stat_field_mapping() {
        // Regular file: every Stat field maps from the Inode fields.
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            let inode = glo::get_inode_ptr(0);
            (*inode).i_mode = 0o100644;
            (*inode).i_nlinks = 2;
            (*inode).i_uid = 0;
            (*inode).i_gid = 0;
            (*inode).i_size = 100;
            (*inode).i_atime = 11;
            (*inode).i_mtime = 22;
            (*inode).i_ctime = 33;
            (*inode).i_dev = 3;
            (*inode).i_num = 7;
            (*inode).i_zone[0] = 999;
            (*inode).i_ndzones = 7;
            (*inode).i_nindirs = 512;
            (*inode).i_sp = Some(crate::mfs::glo::get_super_ptr(0));
            let sp = crate::mfs::glo::get_super_ptr(0);
            (*sp).s_block_size = 4096;
            (*sp).s_log_zone_size = 0;

            let st = build_stat(0);
            assert_eq!(st.st_dev, 3);
            assert_eq!(st.st_ino, 7);
            assert_eq!(st.st_mode, 0o100644);
            assert_eq!(st.st_nlink, 2);
            assert_eq!(st.st_uid, 0);
            assert_eq!(st.st_gid, 0);
            // Regular file: st_rdev is NO_DEV, not zone[0].
            assert_eq!(st.st_rdev, NO_DEV as u64);
            assert_eq!(st.st_size, 100);
            assert_eq!(st.st_blocks, 8); // 1 zone @ 4 KiB = 8 * 512
            assert_eq!(st.st_atime, 11);
            assert_eq!(st.st_mtime, 22);
            assert_eq!(st.st_ctime, 33);
        }
    }

    #[test]
    fn test_build_stat_special_device() {
        // Character special: st_rdev carries the device zone.
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            let inode = glo::get_inode_ptr(0);
            (*inode).i_mode = 0o020666;
            (*inode).i_zone[0] = 0x1234;
            (*inode).i_sp = None;
            let st = build_stat(0);
            assert_eq!(st.st_rdev, 0x1234);
        }
    }

    #[test]
    fn test_fs_stat_uninitialized_returns_einval() {
        // With an empty inode table, get_inode fails → EINVAL.
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            let mfs = glo::mfs_ptr();
            (*mfs).fs_dev = NO_DEV;
            (*mfs).m_in.m_source = 1;
            (*mfs).m_in.m_type = REQ_STAT;
            (*mfs).m_in.m_payload.m1.m1i1 = 1;
            (*mfs).m_in.m_payload.m1.m1i3 = 0;
            assert_eq!(fs_stat(), EINVAL);
        }
    }

    #[test]
    fn test_fs_statvfs_uninitialized_returns_einval() {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            let mfs = glo::mfs_ptr();
            (*mfs).fs_dev = NO_DEV;
            (*mfs).m_in.m_source = 1;
            (*mfs).m_in.m_type = REQ_STATVFS;
            (*mfs).m_in.m_payload.m1.m1i1 = 0;
            assert_eq!(fs_statvfs(), EINVAL);
        }
    }
}
