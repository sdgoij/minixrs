//! Path lookup and directory operations — adapted from `minix/fs/mfs/path.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;

pub fn fs_lookup() -> i32 {
    todo!("fs_lookup: not yet wired")
}

pub fn advance(dirp_idx: u16, string: &[u8], chk_perm: i32) -> Option<u16> {
    if string.is_empty() || string[0] == 0 {
        unsafe {
            (*glo::mfs_ptr()).err_code = ENOENT;
        }
        return None;
    }
    let mut numb: u32 = 0;
    let r = search_dir(dirp_idx, string, Some(&mut numb), LOOK_UP, chk_perm);
    if r != OK {
        return None;
    }
    let dev = unsafe { (*glo::get_inode_ptr(dirp_idx as usize)).i_dev };
    let rip = get_inode(dev, numb)?;

    unsafe {
        let rip_ptr = &mut *glo::get_inode_ptr(rip as usize);
        let dirp_ptr = &*glo::get_inode_ptr(dirp_idx as usize);
        if (*rip_ptr).i_num == ROOT_INODE
            && (*dirp_ptr).i_num == ROOT_INODE
            && string.len() >= 2
            && string[0] == b'.'
            && string[1] == b'.'
        {
            if !(*rip_ptr)
                .i_sp
                .as_ref()
                .map_or(true, |sp| sp.s_is_root != 0)
            {
                (*glo::mfs_ptr()).err_code = ELEAVEMOUNT;
            }
        }
        if (*rip_ptr).i_mountpoint != FALSE {
            (*glo::mfs_ptr()).err_code = EENTERMOUNT;
        }
    }
    Some(rip)
}

pub fn search_dir(
    ldir_idx: u16,
    _string: &[u8],
    _numb: Option<&mut u32>,
    _flag: i32,
    _check_permissions: i32,
) -> i32 {
    unsafe {
        let ldir = &*glo::get_inode_ptr(ldir_idx as usize);
        if (*ldir).i_mode & I_TYPE != I_DIRECTORY {
            return ENOTDIR;
        }
        todo!("search_dir: buffer cache not yet wired");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
        }
    }

    #[test]
    fn test_advance_empty_string_returns_none() {
        // advance returns None immediately when the path is empty
        // and sets err_code to ENOENT.
        init();
        assert!(advance(0, &[], IGN_PERM).is_none());
        unsafe {
            assert_eq!((*crate::mfs::glo::mfs_ptr()).err_code, ENOENT);
        }
    }

    #[test]
    fn test_advance_null_terminated_string_returns_none() {
        // A string whose first byte is NUL is treated as empty.
        init();
        assert!(advance(0, &[0], IGN_PERM).is_none());
    }

    #[test]
    fn test_search_dir_with_non_directory_inode() {
        // After init, inode_table[0].i_mode == 0 (not I_DIRECTORY),
        // so search_dir returns ENOTDIR.
        init();
        assert_eq!(search_dir(0, b"name", None, LOOK_UP, IGN_PERM), ENOTDIR);
    }
}
