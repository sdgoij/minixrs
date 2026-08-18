//! Stat operations — adapted from `minix/fs/pfs/stadir.c`

use crate::mfs::types::Stat;
use crate::pfs::consts::*;
use crate::pfs::glo;
use crate::pfs::inode::*;

/// Build the `Stat` for a pipe inode, mirroring C `stat_inode`.
///
/// Pipes report `S_IFIFO` mode, the current buffered byte count as size,
/// `PIPE_BUF` as block size, and no device.
// Reference: stadir.c stat_inode()
pub fn build_stat(rip_idx: u16) -> Stat {
    unsafe {
        let inode = &*glo::get_inode_ptr(rip_idx as usize);
        let mo = inode.i_mode & I_TYPE;
        let special = mo == I_CHAR_SPECIAL || mo == I_BLOCK_SPECIAL;
        let mut mode = inode.i_mode as u32;
        if !special {
            // C wipes the I_REGULAR bit for non-special inodes (pipes never
            // carry it, but keep the layout identical to C).
            mode &= !(I_REGULAR as u32);
        }
        let size = inode.i_size;
        let blocks = if size <= 0 {
            0
        } else {
            (size as u64).div_ceil(S_BLKSIZE as u64)
        };
        Stat {
            st_dev: inode.i_dev as u64,
            st_ino: inode.i_num as u64,
            st_mode: mode,
            st_nlink: inode.i_nlinks as u32,
            st_uid: inode.i_uid as u32,
            st_gid: inode.i_gid as u32,
            st_rdev: if special {
                inode.i_rdev as u64
            } else {
                NO_DEV as u64
            },
            st_size: size,
            st_blksize: PIPE_BUF as i64,
            st_blocks: blocks as i64,
            st_atime: inode.i_atime,
            st_mtime: inode.i_mtime,
            st_ctime: inode.i_ctime,
        }
    }
}

/// Fill a `Stat` for inode `rip_idx` and copy it to the caller's buffer
/// through grant `gid`. `who_e` is the granter (VFS) endpoint.
///
/// Returns the safecopy result (0 on success, negative errno on failure).
// Reference: stadir.c stat_inode()
fn stat_inode(rip_idx: u16, who_e: i32, gid: i32) -> i32 {
    unsafe {
        if (*glo::get_inode_ptr(rip_idx as usize)).i_update != 0 {
            update_times(rip_idx);
        }
        let stat = build_stat(rip_idx);
        let bytes = core::slice::from_raw_parts(
            &stat as *const Stat as *const u8,
            core::mem::size_of::<Stat>(),
        );
        crate::block_io::safecopy_to(who_e, gid, bytes)
    }
}

/// Stat a pipe inode (REQ_STAT).
///
/// The request carries the inode number (u32 at payload[0]) and the grant
/// for the stat buffer (i32 at payload[8]); the granter is the sender.
// Reference: stadir.c fs_stat()
pub fn fs_stat() -> i32 {
    unsafe {
        let pfs = glo::pfs_ptr();
        let data_ptr = core::ptr::addr_of_mut!((*pfs).m_in_data) as *const u8;
        let inum = core::ptr::read_unaligned(data_ptr.add(0) as *const u32);
        let gid = core::ptr::read_unaligned(data_ptr.add(8) as *const i32);
        let who = (*pfs).m_source;
        let rip_idx = match find_inode(inum) {
            Some(i) => i,
            None => return -EINVAL,
        };
        let dev = (*glo::get_inode_ptr(rip_idx as usize)).i_dev;
        get_inode(dev, inum); // mark inode in use (C: get_inode)
        let r = stat_inode(rip_idx, who, gid);
        put_inode(Some(rip_idx));
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            glo::pfs_init_globals();
            init_inode_cache();
        }
    }

    #[test]
    fn test_build_stat_pipe() {
        init();
        let ip = get_inode(1, 1).unwrap();
        unsafe {
            let inode = &mut *glo::get_inode_ptr(ip as usize);
            (*inode).i_mode = I_NAMED_PIPE | 0o600;
            (*inode).i_uid = 100;
            (*inode).i_gid = 200;
            (*inode).i_size = 1234;
            (*inode).i_update = 0;
        }
        let st = build_stat(ip);
        assert_eq!(st.st_ino, 1);
        assert_eq!(st.st_mode, (I_NAMED_PIPE | 0o600) as u32);
        assert_eq!(st.st_uid, 100);
        assert_eq!(st.st_gid, 200);
        assert_eq!(st.st_size, 1234);
        assert_eq!(st.st_blksize, PIPE_BUF as i64);
        assert_eq!(st.st_rdev, NO_DEV as u64);
        assert_eq!(st.st_nlink, NO_LINK as u32);
        // ceil(1234 / 512) = 3
        assert_eq!(st.st_blocks, 3);
    }

    #[test]
    fn test_fs_stat_unknown_inode() {
        init();
        unsafe {
            let pfs = glo::pfs_ptr();
            let data = core::ptr::addr_of_mut!((*pfs).m_in_data) as *mut u8;
            core::ptr::write_unaligned(data.add(0) as *mut u32, 999);
            core::ptr::write_unaligned(data.add(8) as *mut i32, 0);
        }
        assert_eq!(fs_stat(), -EINVAL);
    }
}
