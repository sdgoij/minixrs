//! ISO 9660 stat operations — adapted from `minix/fs/iso9660fs/stadir.c`

use crate::iso9660::consts::*;
use crate::iso9660::glo;
use crate::iso9660::inode;
use crate::iso9660::types::*;
use crate::iso9660::utility;

/// `fs_stat()` — stat a file by inode.
///
/// Returns file metadata (size, mode, timestamps, etc.) via the
/// VFS reply message.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_stat() -> i32 {
    let dir = inode::get_dir_record(1); // stub: inode from fs_m_in
    if dir.is_null() {
        return EINVAL;
    }

    let r = stat_dir_record(dir, 0, VFS_PROC_NR, 0); // stub gid

    inode::release_dir_record(dir);
    r
}

/// Internal helper — fill a stat structure from a directory record.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn stat_dir_record(dir: *mut DirRecord, _pipe_pos: i32, _who_e: i32, _gid: u32) -> i32 {
    let v_pri = glo::v_pri_ptr();
    let isofs = glo::isofs_ptr();

    // Calculate total blocks in 512-byte units
    let mut blocks = (*v_pri).volume_space_size_l;
    let block_size = (*v_pri).logical_block_size_l;
    blocks *= block_size as u32 >> 9;

    // In the real implementation, fill a struct stat and copy it out:
    //   statbuf.st_dev     = fs_dev;
    //   statbuf.st_ino     = ID_DIR_RECORD(dir);
    //   statbuf.st_mode    = dir->d_mode;
    //   statbuf.st_nlink   = dir->d_count;
    //   statbuf.st_uid     = 0;
    //   statbuf.st_gid     = 0;
    //   statbuf.st_rdev    = NO_DEV;
    //   statbuf.st_size    = dir->d_file_size;
    //   statbuf.st_blksize = block_size;
    //   statbuf.st_blocks  = blocks;

    // Convert ISO date to Unix timestamp
    let _time1 = utility::iso_date_to_unix(&(*dir).rec_date);

    //   statbuf.st_atime = time1;
    //   statbuf.st_mtime = time1;
    //   statbuf.st_ctime = time1;

    //   sys_safecopyto(who_e, gid, 0, &statbuf, sizeof(statbuf));
    let _ = blocks;
    let _ = isofs;

    OK
}

/// `fs_statvfs()` — return filesystem statistics.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_statvfs() -> i32 {
    let v_pri = glo::v_pri_ptr();
    let isofs = glo::isofs_ptr();

    let block_size = (*v_pri).logical_block_size_l;

    // In the real implementation, fill a struct statvfs and copy it out:
    //   st.f_bsize   = block_size;
    //   st.f_frsize  = block_size;
    //   st.f_iosize  = block_size;
    //   st.f_blocks  = v_pri.volume_space_size_l;
    //   st.f_namemax = NAME_MAX;
    //   sys_safecopyto(fs_m_in.m_source, fs_m_in.m_vfs_fs_statvfs.grant, 0, &st, sizeof(st));
    let _ = isofs;
    let _ = block_size;

    OK
}

/// `fs_blockstats()` — return block usage statistics.
///
/// Since ISO 9660 is read-only, all blocks are used.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_blockstats(blocks: &mut u64, free: &mut u64, used: &mut u64) {
    let v_pri = glo::v_pri_ptr();
    *used = (*v_pri).volume_space_size_l as u64;
    *blocks = *used;
    *free = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso9660::glo;
    use crate::iso9660::inode;

    #[test]
    fn test_fs_stat_stub() {
        unsafe {
            glo::isofs_init_globals();
            inode::init_inode_cache();
            let r = fs_stat();
            assert_eq!(r, EINVAL); // no inode in cache
        }
    }

    #[test]
    fn test_fs_statvfs_stub() {
        unsafe {
            glo::isofs_init_globals();
            let r = fs_statvfs();
            assert_eq!(r, OK);
        }
    }

    #[test]
    fn test_fs_blockstats() {
        unsafe {
            glo::isofs_init_globals();
            let mut blocks = 0;
            let mut free = 1;
            let mut used = 2;
            fs_blockstats(&mut blocks, &mut free, &mut used);
            assert_eq!(free, 0);
            assert_eq!(blocks, used);
        }
    }
}
