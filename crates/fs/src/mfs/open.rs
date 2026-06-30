//! File creation ops — adapted from `minix/fs/mfs/open.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::path::*;
use crate::mfs::super_block::get_block_size;
use crate::mfs::write::*;
use libs::libminixfs::cache::lmfs_put_block;

/// Internal: create a new inode under `ldirp_idx` with name `string` and mode `bits`.
unsafe fn new_node(ldirp_idx: u16, string: &[u8], bits: u16, z0: u32) -> Option<u16> {
    let ldirp = &*glo::get_inode_ptr(ldirp_idx as usize);

    if (*ldirp).i_nlinks == NO_LINK {
        (*glo::mfs_ptr()).err_code = ENOENT;
        return None;
    }

    let rip = advance(ldirp_idx, string, IGN_PERM);

    if bits & I_TYPE == I_DIRECTORY && (*ldirp).i_nlinks >= LINK_MAX {
        if let Some(r) = rip {
            put_inode(Some(r));
        }
        (*glo::mfs_ptr()).err_code = EMLINK;
        return None;
    }

    if rip.is_none() && (*glo::mfs_ptr()).err_code == ENOENT {
        let new_rip = alloc_inode((*ldirp).i_dev, bits)?;

        {
            let rp = &mut *glo::get_inode_ptr(new_rip as usize);
            (*rp).i_nlinks = (*rp).i_nlinks.saturating_add(1);
            (*rp).i_zone[0] = z0;
        }
        rw_inode(new_rip, WRITING);

        let mut inum = (*glo::get_inode_ptr(new_rip as usize)).i_num;
        let r = search_dir(ldirp_idx, string, Some(&mut inum), ENTER, IGN_PERM);
        if r != OK {
            let rp = &mut *glo::get_inode_ptr(new_rip as usize);
            (*rp).i_nlinks = (*rp).i_nlinks.saturating_sub(1);
            (*rp).i_dirt = IN_DIRTY;
            put_inode(Some(new_rip));
            (*glo::mfs_ptr()).err_code = r;
            return None;
        }

        (*glo::mfs_ptr()).err_code = OK;
        return Some(new_rip);
    }

    let ec = (*glo::mfs_ptr()).err_code;
    if ec == EENTERMOUNT || ec == ELEAVEMOUNT {
        (*glo::mfs_ptr()).err_code = EEXIST;
    } else {
        (*glo::mfs_ptr()).err_code = if rip.is_some() { EEXIST } else { ec };
    }
    rip
}

pub fn fs_create() -> i32 {
    unsafe {
        let dir_ino = (*glo::mfs_ptr()).cch[0] as u32;
        let mode = (*glo::mfs_ptr()).cch[1] as u16;
        let uid = (*glo::mfs_ptr()).cch[2] as u16;
        let gid = (*glo::mfs_ptr()).cch[3] as u16;
        let dev = (*glo::mfs_ptr()).fs_dev;
        let user_path = &(*glo::mfs_ptr()).user_path;
        let len = user_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(user_path.len());
        let string = &user_path[..len];

        (*glo::mfs_ptr()).caller_uid = uid;
        (*glo::mfs_ptr()).caller_gid = gid;

        let ldirp = match get_inode(dev, dir_ino) {
            Some(d) => d,
            None => return ENOENT,
        };

        let rip = new_node(ldirp, string, mode, NO_ZONE);
        let r = (*glo::mfs_ptr()).err_code;

        if r != OK {
            put_inode(Some(ldirp));
            if let Some(rp) = rip {
                put_inode(Some(rp));
            }
            return r;
        }

        if let Some(rp) = rip {
            let rip_ref = &*glo::get_inode_ptr(rp as usize);
            (*glo::mfs_ptr()).cch[0] = (*rip_ref).i_num as i32;
            (*glo::mfs_ptr()).cch[1] = (*rip_ref).i_mode as i32;
            (*glo::mfs_ptr()).cch[2] = (*rip_ref).i_size;
            (*glo::mfs_ptr()).cch[3] = (*rip_ref).i_uid as i32;
            (*glo::mfs_ptr()).cch[4] = (*rip_ref).i_gid as i32;
            put_inode(Some(rp));
        }

        put_inode(Some(ldirp));
        if rip.is_some() {
            OK
        } else {
            (*glo::mfs_ptr()).err_code
        }
    }
}

pub fn fs_mkdir() -> i32 {
    unsafe {
        let dir_ino = (*glo::mfs_ptr()).cch[0] as u32;
        let mode = (*glo::mfs_ptr()).cch[1] as u16;
        let uid = (*glo::mfs_ptr()).cch[2] as u16;
        let gid = (*glo::mfs_ptr()).cch[3] as u16;
        let dev = (*glo::mfs_ptr()).fs_dev;
        let user_path = &(*glo::mfs_ptr()).user_path;
        let len = user_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(user_path.len());
        let string = &user_path[..len];

        (*glo::mfs_ptr()).caller_uid = uid;
        (*glo::mfs_ptr()).caller_gid = gid;

        let ldirp = match get_inode(dev, dir_ino) {
            Some(d) => d,
            None => return ENOENT,
        };

        let rip = new_node(ldirp, string, mode | I_DIRECTORY, 0);

        if rip.is_none() || (*glo::mfs_ptr()).err_code == EEXIST {
            if let Some(r) = rip {
                put_inode(Some(r));
            }
            let ec = (*glo::mfs_ptr()).err_code;
            put_inode(Some(ldirp));
            return ec;
        }

        let rip = rip.unwrap();
        let dotdot = (*glo::get_inode_ptr(ldirp as usize)).i_num;
        let dot = (*glo::get_inode_ptr(rip as usize)).i_num;

        {
            let rp = &mut *glo::get_inode_ptr(rip as usize);
            (*rp).i_mode = mode | I_DIRECTORY;
        }

        let mut dot_mut = dot;
        let mut dotdot_mut = dotdot;
        let r1 = search_dir(rip, &DOT1, Some(&mut dot_mut), ENTER, IGN_PERM);
        let r2 = search_dir(rip, &DOT2, Some(&mut dotdot_mut), ENTER, IGN_PERM);

        if r1 == OK && r2 == OK {
            let rp = &mut *glo::get_inode_ptr(rip as usize);
            (*rp).i_nlinks = (*rp).i_nlinks.saturating_add(1);
            let lp = &mut *glo::get_inode_ptr(ldirp as usize);
            (*lp).i_nlinks = (*lp).i_nlinks.saturating_add(1);
            (*lp).i_dirt = IN_DIRTY;
        } else {
            let _ = search_dir(ldirp, string, None, DELETE, IGN_PERM);
            let rp = &mut *glo::get_inode_ptr(rip as usize);
            (*rp).i_nlinks = (*rp).i_nlinks.saturating_sub(1);
        }

        {
            let rp = &mut *glo::get_inode_ptr(rip as usize);
            (*rp).i_dirt = IN_DIRTY;
        }

        let ec = (*glo::mfs_ptr()).err_code;
        put_inode(Some(ldirp));
        put_inode(Some(rip));
        ec
    }
}

pub fn fs_mknod() -> i32 {
    unsafe {
        let dir_ino = (*glo::mfs_ptr()).cch[0] as u32;
        let mode = (*glo::mfs_ptr()).cch[1] as u16;
        let device = (*glo::mfs_ptr()).cch[2] as u32;
        let uid = (*glo::mfs_ptr()).cch[3] as u16;
        let gid = (*glo::mfs_ptr()).cch[4] as u16;
        let dev = (*glo::mfs_ptr()).fs_dev;
        let user_path = &(*glo::mfs_ptr()).user_path;
        let len = user_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(user_path.len());
        let string = &user_path[..len];

        (*glo::mfs_ptr()).caller_uid = uid;
        (*glo::mfs_ptr()).caller_gid = gid;

        let ldirp = match get_inode(dev, dir_ino) {
            Some(d) => d,
            None => return ENOENT,
        };

        let ip = new_node(ldirp, string, mode, device);
        let ec = (*glo::mfs_ptr()).err_code;
        if let Some(ip_val) = ip {
            put_inode(Some(ip_val));
        }
        put_inode(Some(ldirp));
        ec
    }
}

pub fn fs_slink() -> i32 {
    unsafe {
        let dir_ino = (*glo::mfs_ptr()).cch[0] as u32;
        let uid = (*glo::mfs_ptr()).cch[1] as u16;
        let gid = (*glo::mfs_ptr()).cch[2] as u16;
        let mem_size = (*glo::mfs_ptr()).cch[4] as usize;
        let dev = (*glo::mfs_ptr()).fs_dev;
        let user_path = &(*glo::mfs_ptr()).user_path;
        let len = user_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(user_path.len());
        let string = &user_path[..len];

        (*glo::mfs_ptr()).caller_uid = uid;
        (*glo::mfs_ptr()).caller_gid = gid;

        let ldirp = match get_inode(dev, dir_ino) {
            Some(d) => d,
            None => return EINVAL,
        };

        let sip = new_node(ldirp, string, I_SYMBOLIC_LINK | RWX_MODES, 0);
        let mut r = (*glo::mfs_ptr()).err_code;

        if r == OK {
            if let Some(sip_idx) = sip {
                let block_size = get_block_size(dev) as usize;
                if block_size > 0 && block_size <= mem_size {
                    r = ENAMETOOLONG;
                } else {
                    let bp = new_block(sip_idx, 0);
                    if bp.is_null() {
                        r = (*glo::mfs_ptr()).err_code;
                        if r == OK {
                            r = EIO;
                        }
                    } else {
                        let sip_ref = &mut *glo::get_inode_ptr(sip_idx as usize);
                        (*sip_ref).i_size = 0;
                        lmfs_put_block(bp as *mut libs::libminixfs::types::Buf, DIRECTORY_BLOCK);
                    }
                }
            }
        }

        if r != OK {
            if let Some(sip_idx) = sip {
                let s = &mut *glo::get_inode_ptr(sip_idx as usize);
                (*s).i_nlinks = NO_LINK;
                let _ = search_dir(ldirp, string, None, DELETE, IGN_PERM);
            }
        }

        if let Some(sip_idx) = sip {
            put_inode(Some(sip_idx));
        }
        put_inode(Some(ldirp));
        r
    }
}

pub fn fs_inhibread() -> i32 {
    EINVAL
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
    fn test_fs_inhibread_returns_einval_when_uninitialized() {
        init();
        assert_eq!(fs_inhibread(), EINVAL);
    }

    #[test]
    fn test_fs_create_returns_enoent_when_no_dev() {
        init();
        assert_eq!(fs_create(), ENOENT);
    }

    #[test]
    fn test_fs_mkdir_returns_enoent_when_no_dev() {
        init();
        assert_eq!(fs_mkdir(), ENOENT);
    }

    #[test]
    fn test_fs_mknod_returns_enoent_when_no_dev() {
        init();
        assert_eq!(fs_mknod(), ENOENT);
    }

    #[test]
    fn test_fs_slink_returns_einval_when_no_dev() {
        init();
        assert_eq!(fs_slink(), EINVAL);
    }
}
