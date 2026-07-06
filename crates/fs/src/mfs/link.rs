//! Link, unlink, rename, readlink — adapted from `minix/fs/mfs/link.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::path::*;
use crate::mfs::read::*;

/* Args to unlink_file */
const SAME: i32 = 1000;

/// Remove a directory entry from `dirp` and decrement the link count on `rip`.
unsafe fn unlink_file(dirp_idx: u16, rip: Option<u16>, fname: &[u8]) -> i32 {
    let mut numb: u32 = 0;
    let rip = match rip {
        Some(idx) => {
            dup_inode(idx);
            Some(idx)
        }
        None => {
            let ec = search_dir(dirp_idx, fname, Some(&mut numb), LOOK_UP, IGN_PERM);
            if ec != OK {
                return ec;
            }
            let dev = (*glo::get_inode_ptr(dirp_idx as usize)).i_dev;
            get_inode(dev, numb)
        }
    };

    let rip = match rip {
        Some(i) => i,
        None => return EINVAL,
    };

    let r = search_dir(dirp_idx, fname, None, DELETE, IGN_PERM);
    if r == OK {
        let rp = &mut *glo::get_inode_ptr(rip as usize);
        (*rp).i_nlinks = (*rp).i_nlinks.saturating_sub(1);
        (*rp).i_update |= CTIME;
        (*rp).i_dirt = IN_DIRTY;
    }

    put_inode(Some(rip));
    r
}

/// Remove a directory: must be empty, not "." or "..", not root.
unsafe fn remove_dir(rldirp_idx: u16, rip_idx: u16, dir_name: &[u8]) -> i32 {
    let r = search_dir(rip_idx, &[], None, IS_EMPTY, IGN_PERM);
    if r != OK {
        return r;
    }
    if dir_name == b"." || dir_name == b".." {
        return EINVAL;
    }
    let rip = &*glo::get_inode_ptr(rip_idx as usize);
    if (*rip).i_num == ROOT_INODE {
        return EBUSY;
    }
    let r = unlink_file(rldirp_idx, Some(rip_idx), dir_name);
    if r != OK {
        return r;
    }
    let _ = unlink_file(rip_idx, None, &DOT1);
    let _ = unlink_file(rip_idx, None, &DOT2);
    OK
}

pub fn fs_link() -> i32 {
    unsafe {
        let ino = (*glo::mfs_ptr()).cch[0] as u32;
        let dir_ino = (*glo::mfs_ptr()).cch[1] as u32;
        let dev = (*glo::mfs_ptr()).fs_dev;
        let user_path = &(*glo::mfs_ptr()).user_path;
        let len = user_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(user_path.len());
        let string = &user_path[..len];

        let rip = match get_inode(dev, ino) {
            Some(r) => r,
            None => return EINVAL,
        };

        let mut r = OK;
        if (*glo::get_inode_ptr(rip as usize)).i_nlinks >= LINK_MAX {
            r = EMLINK;
        }
        if r == OK {
            let mode = (*glo::get_inode_ptr(rip as usize)).i_mode;
            if (mode & I_TYPE) == I_DIRECTORY && (*glo::mfs_ptr()).caller_uid != SU_UID as u16 {
                r = EPERM;
            }
        }
        if r != OK {
            put_inode(Some(rip));
            return r;
        }

        let ip = match get_inode(dev, dir_ino) {
            Some(i) => i,
            None => {
                put_inode(Some(rip));
                return EINVAL;
            }
        };
        if (*glo::get_inode_ptr(ip as usize)).i_nlinks == NO_LINK {
            put_inode(Some(rip));
            put_inode(Some(ip));
            return ENOENT;
        }

        let new_ip = advance(ip, string, IGN_PERM);
        if new_ip.is_none() {
            let ec = (*glo::mfs_ptr()).err_code;
            if ec == ENOENT {
                r = OK;
            } else {
                r = ec;
            }
        } else {
            put_inode(new_ip);
            r = EEXIST;
        }

        if r == OK {
            let mut inum = (*glo::get_inode_ptr(rip as usize)).i_num;
            r = search_dir(ip, string, Some(&mut inum), ENTER, IGN_PERM);
        }
        if r == OK {
            let rp = &mut *glo::get_inode_ptr(rip as usize);
            (*rp).i_nlinks = (*rp).i_nlinks.saturating_add(1);
            (*rp).i_update |= CTIME;
            (*rp).i_dirt = IN_DIRTY;
        }

        put_inode(Some(rip));
        put_inode(Some(ip));
        r
    }
}

pub fn fs_unlink() -> i32 {
    unsafe {
        let dir_ino = (*glo::mfs_ptr()).cch[0] as u32;
        let dev = (*glo::mfs_ptr()).fs_dev;
        let req_nr = (*glo::mfs_ptr()).req_nr;
        let user_path = &(*glo::mfs_ptr()).user_path;
        let len = user_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(user_path.len());
        let string = &user_path[..len];

        let rldirp = match get_inode(dev, dir_ino) {
            Some(d) => d,
            None => return EINVAL,
        };

        let rip = advance(rldirp, string, IGN_PERM);
        let mut r = (*glo::mfs_ptr()).err_code;
        if r != OK {
            if r == EENTERMOUNT || r == ELEAVEMOUNT {
                if let Some(ip) = rip {
                    put_inode(Some(ip));
                }
                r = EBUSY;
            }
            put_inode(Some(rldirp));
            return r;
        }

        let rip = match rip {
            Some(i) => i,
            None => {
                put_inode(Some(rldirp));
                return ENOENT;
            }
        };

        if (*glo::get_inode_ptr(rip as usize))
            .i_sp
            .as_ref()
            .map_or(true, |sp| sp.s_rd_only != 0)
        {
            r = EROFS;
        } else if req_nr == (REQ_UNLINK - FS_BASE) {
            let mode = (*glo::get_inode_ptr(rip as usize)).i_mode;
            if (mode & I_TYPE) == I_DIRECTORY {
                r = EPERM;
            }
            if r == OK {
                r = unlink_file(rldirp, Some(rip), string);
            }
        } else {
            r = remove_dir(rldirp, rip, string);
        }

        put_inode(Some(rip));
        put_inode(Some(rldirp));
        r
    }
}

pub fn fs_rdlink() -> i32 {
    unsafe {
        let ino = (*glo::mfs_ptr()).cch[0] as u32;
        let dev = (*glo::mfs_ptr()).fs_dev;

        let rip = match get_inode(dev, ino) {
            Some(r) => r,
            None => return EINVAL,
        };

        let r;
        let mode = (*glo::get_inode_ptr(rip as usize)).i_mode;
        if (mode & I_TYPE) != I_SYMBOLIC_LINK {
            r = EACCES;
        } else {
            let bp = get_block_map(rip, 0);
            if bp.is_null() {
                r = EIO;
            } else {
                let rip_ref = &*glo::get_inode_ptr(rip as usize);
                let copylen = core::cmp::min((*rip_ref).i_size as usize, 0x7FFFFFFF);
                let _data = core::slice::from_raw_parts(bp, copylen);
                (*glo::mfs_ptr()).cch[0] = copylen as i32;
                r = OK;
            }
        }

        put_inode(Some(rip));
        r
    }
}

pub fn fs_rename() -> i32 {
    unsafe {
        let dir_old = (*glo::mfs_ptr()).cch[0] as u32;
        let dir_new = (*glo::mfs_ptr()).cch[1] as u32;
        let dev = (*glo::mfs_ptr()).fs_dev;
        let user_path = &(*glo::mfs_ptr()).user_path;

        let old_name = {
            let nlen = core::cmp::min(user_path.len(), MFS_NAME_MAX);
            &user_path[0..nlen]
        };
        let new_name = {
            let start = core::cmp::min(MFS_NAME_MAX, user_path.len());
            let remain = user_path.len() - start;
            let nlen = core::cmp::min(remain, MFS_NAME_MAX);
            &user_path[start..start + nlen]
        };

        let old_dirp = match get_inode(dev, dir_old) {
            Some(d) => d,
            None => return EINVAL,
        };
        let old_ip = advance(old_dirp, old_name, IGN_PERM);
        let mut r = (*glo::mfs_ptr()).err_code;

        if r == EENTERMOUNT || r == ELEAVEMOUNT {
            if let Some(ip) = old_ip {
                put_inode(Some(ip));
            }
            r = if r == EENTERMOUNT { EXDEV } else { EINVAL };
        }
        if r != OK || old_ip.is_none() {
            put_inode(Some(old_dirp));
            return r;
        }
        let old_ip = old_ip.unwrap();

        let new_dirp = match get_inode(dev, dir_new) {
            Some(d) => d,
            None => {
                put_inode(Some(old_ip));
                put_inode(Some(old_dirp));
                return EINVAL;
            }
        };
        if (*glo::get_inode_ptr(new_dirp as usize)).i_nlinks == NO_LINK {
            put_inode(Some(old_ip));
            put_inode(Some(old_dirp));
            put_inode(Some(new_dirp));
            return ENOENT;
        }

        let new_ip = advance(new_dirp, new_name, IGN_PERM);
        if (*glo::mfs_ptr()).err_code == EENTERMOUNT {
            if let Some(ip) = new_ip {
                put_inode(Some(ip));
            }
            r = EBUSY;
        }

        let odir = ((*glo::get_inode_ptr(old_ip as usize)).i_mode & I_TYPE) == I_DIRECTORY;
        let same_pdir = old_dirp == new_dirp;

        if r == OK {
            if old_name == b"." || old_name == b".." || new_name == b"." || new_name == b".." {
                r = EINVAL;
            }
            if let Some(new_ip_val) = new_ip {
                if old_ip == new_ip_val {
                    r = SAME;
                }
                let ndir =
                    ((*glo::get_inode_ptr(new_ip_val as usize)).i_mode & I_TYPE) == I_DIRECTORY;
                if odir && !ndir {
                    r = ENOTDIR;
                }
                if !odir && ndir {
                    r = EISDIR;
                }
            } else if odir
                && !same_pdir
                && (*glo::get_inode_ptr(new_dirp as usize)).i_nlinks >= LINK_MAX
            {
                r = EMLINK;
            }
        }

        if r == OK {
            if let Some(new_ip_val) = new_ip {
                if odir {
                    r = remove_dir(new_dirp, new_ip_val, new_name);
                } else {
                    r = unlink_file(new_dirp, Some(new_ip_val), new_name);
                }
            }
        }

        if r == OK {
            let mut numb = (*glo::get_inode_ptr(old_ip as usize)).i_num;
            if same_pdir {
                r = search_dir(old_dirp, old_name, None, DELETE, IGN_PERM);
                if r == OK {
                    r = search_dir(old_dirp, new_name, Some(&mut numb), ENTER, IGN_PERM);
                }
            } else {
                r = search_dir(new_dirp, new_name, Some(&mut numb), ENTER, IGN_PERM);
                if r == OK {
                    r = search_dir(old_dirp, old_name, None, DELETE, IGN_PERM);
                }
            }
        }

        if r == OK && odir && !same_pdir {
            let mut new_inum = (*glo::get_inode_ptr(new_dirp as usize)).i_num;
            let _ = unlink_file(old_ip, None, &DOT2);
            if search_dir(old_ip, &DOT2, Some(&mut new_inum), ENTER, IGN_PERM) == OK {
                let ndp = &mut *glo::get_inode_ptr(new_dirp as usize);
                (*ndp).i_nlinks = (*ndp).i_nlinks.saturating_add(1);
                (*ndp).i_dirt = IN_DIRTY;
            }
        }

        put_inode(Some(old_dirp));
        put_inode(Some(old_ip));
        put_inode(Some(new_dirp));
        if let Some(nip) = new_ip {
            put_inode(Some(nip));
        }

        if r == SAME { OK } else { r }
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
    fn test_fs_link_returns_einval_when_no_inode() {
        init();
        assert_eq!(fs_link(), EINVAL);
    }

    #[test]
    fn test_fs_unlink_returns_einval_when_no_inode() {
        init();
        assert_eq!(fs_unlink(), EINVAL);
    }

    #[test]
    fn test_fs_rdlink_returns_einval_when_no_inode() {
        init();
        assert_eq!(fs_rdlink(), EINVAL);
    }

    #[test]
    fn test_fs_rename_returns_einval_when_no_inode() {
        init();
        // No inodes loaded, so rename returns EINVAL
        assert_eq!(fs_rename(), EINVAL);
    }
}
