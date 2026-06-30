//! Link, unlink, rename, rdlink — adapted from `minix/fs/ext2/link.c`

use libs::libminixfs::cache::{lmfs_get_block_ino, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::{DIRECTORY_BLOCK, NORMAL, VMC_NO_INODE};

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::path::*;
use crate::ext2::read::read_map;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// fs_link — create a hard link.
pub unsafe fn fs_link() -> i32 {
    let ext2 = glo::ext2_ptr();

    // FIXME: parse from message
    // rip = get_inode(fs_dev, m_vfs_fs_link.inode)
    // ip  = get_inode(fs_dev, m_vfs_fs_link.dir_ino)
    // string = copied from grant
    let rip = get_inode((*ext2).fs_dev, (*ext2).fs_m_in_type as u32);
    if rip.is_null() {
        return EINVAL;
    }

    let mut r = OK;
    if (*rip).i_links_count >= LINK_MAX {
        r = EMLINK;
    }
    if r == OK && ((*rip).i_mode & I_TYPE) == I_DIRECTORY && (*ext2).caller_uid as u32 != SU_UID {
        r = EPERM;
    }
    if r != OK {
        put_inode(rip);
        return r;
    }

    // Parent directory — FIXME: parse dir_ino from message
    let dir_ino = (*ext2).cch[0] as u32;
    let ip = get_inode((*ext2).fs_dev, dir_ino);
    if ip.is_null() {
        put_inode(rip);
        return EINVAL;
    }

    if (*ip).i_links_count == NO_LINK {
        put_inode(rip);
        put_inode(ip);
        return ENOENT;
    }

    // string from user_path — FIXME: proper grant copy
    let string = &(&(*ext2).user_path)[..EXT2_NAME_MAX + 1];

    // Check if name2 exists
    let new_ip = advance(ip, string, IGN_PERM);
    if new_ip.is_null() {
        let err = (*ext2).err_code;
        if err == ENOENT {
            r = OK;
        } else {
            r = err;
        }
    } else {
        put_inode(new_ip);
        r = EEXIST;
    }

    if r == OK {
        r = search_dir(
            ip,
            string,
            &mut (*rip).i_num as *mut u32,
            ENTER,
            IGN_PERM,
            ((*rip).i_mode & I_TYPE) as i32,
        );
    }

    if r == OK {
        (*rip).i_links_count += 1;
        (*rip).i_update |= CTIME;
        (*rip).i_dirt = IN_DIRTY;
    }

    put_inode(rip);
    put_inode(ip);
    r
}

/// fs_unlink — remove a link.
pub unsafe fn fs_unlink() -> i32 {
    let ext2 = glo::ext2_ptr();

    let dir_ino = (*ext2).fs_m_in_type as u32; // FIXME: proper message parsing
    let rldirp = get_inode((*ext2).fs_dev, dir_ino);
    if rldirp.is_null() {
        return EINVAL;
    }

    let string = &(&(*ext2).user_path)[..EXT2_NAME_MAX + 1];
    let rip = advance(rldirp, string, IGN_PERM);
    let mut r = (*ext2).err_code;

    if r != OK {
        if r == EENTERMOUNT || r == ELEAVEMOUNT {
            put_inode(rip);
            r = EBUSY;
        }
        put_inode(rldirp);
        return r;
    }

    // REQ_UNLINK vs REQ_RMDIR — use type from message
    let req_type = (*ext2).req_nr;
    if req_type == REQ_UNLINK {
        if ((*rip).i_mode & I_TYPE) == I_DIRECTORY {
            r = EPERM;
        }
        if r == OK {
            r = unlink_file(rldirp, rip, string);
        }
    } else {
        r = remove_dir(rldirp, rip, string);
    }

    put_inode(rip);
    put_inode(rldirp);
    r
}

/// fs_rdlink — read a symbolic link.
pub unsafe fn fs_rdlink() -> i32 {
    let ext2 = glo::ext2_ptr();

    let ino = (*ext2).fs_m_in_type as u32; // FIXME: proper message parsing
    let rip = get_inode((*ext2).fs_dev, ino);
    if rip.is_null() {
        return EINVAL;
    }

    let max_symlink_len = MAX_FAST_SYMLINK_LENGTH;
    let mut r = OK;

    // Determine link text source
    let link_text: *const u8;
    let mut bp = core::ptr::null_mut();

    if (*rip).i_size as usize >= max_symlink_len {
        // Normal symlink — read from data block
        let b = read_map(rip, 0, 0);
        if b == NO_BLOCK {
            r = EIO;
            link_text = core::ptr::null();
        } else {
            bp = lmfs_get_block_ino((*rip).i_dev, b as u64, NORMAL, (*rip).i_num as u64, 0);
            if bp.is_null() {
                r = EIO;
                link_text = core::ptr::null();
            } else {
                link_text = b_data(bp);
            }
        }
    } else {
        // Fast symlink — stored in inode i_block array
        link_text = (*rip).i_block.as_ptr() as *const u8;
    }

    if r == OK {
        // FIXME: copy to user via grant
        // For now, just succeed (reply not wired)
        let _ = link_text;
    }

    if !bp.is_null() {
        lmfs_put_block(bp, DIRECTORY_BLOCK);
    }

    put_inode(rip);
    r
}

/// fs_rename — rename a file.
pub unsafe fn fs_rename() -> i32 {
    let ext2 = glo::ext2_ptr();

    // FIXME: parse old/new dir + name from message
    let old_dir_ino = (*ext2).fs_m_in_type as u32;
    let new_dir_ino = (*ext2).cch[0] as u32;

    let old_dirp = get_inode((*ext2).fs_dev, old_dir_ino);
    if old_dirp.is_null() {
        return EINVAL;
    }

    let old_name = &(&(*ext2).user_path)[..EXT2_NAME_MAX + 1];
    let old_ip = advance(old_dirp, old_name, IGN_PERM);
    let mut r = (*ext2).err_code;

    if old_ip.is_null() || (r != OK && r != ENOENT) {
        put_inode(old_dirp);
        return if r == OK { ENOENT } else { r };
    }

    if r == EENTERMOUNT || r == ELEAVEMOUNT {
        put_inode(old_ip);
        put_inode(old_dirp);
        return EXDEV;
    }
    r = OK;

    let new_dirp = get_inode((*ext2).fs_dev, new_dir_ino);
    if new_dirp.is_null() || (*new_dirp).i_links_count == NO_LINK {
        put_inode(old_ip);
        put_inode(old_dirp);
        if !new_dirp.is_null() {
            put_inode(new_dirp);
        }
        return ENOENT;
    }

    let new_name = &(&(*ext2).user_path)[..EXT2_NAME_MAX]; // second name
    let new_ip = advance(new_dirp, new_name, IGN_PERM);

    let odir = ((*old_ip).i_mode & I_TYPE) == I_DIRECTORY;

    if r == OK && new_ip.is_null() && (*ext2).err_code != ENOENT {
        r = (*ext2).err_code;
    }

    if r == OK && !new_ip.is_null() {
        let ndir = ((*new_ip).i_mode & I_TYPE) == I_DIRECTORY;
        if odir && !ndir {
            r = ENOTDIR;
        } else if !odir && ndir {
            r = EISDIR;
        }
    }

    if r == OK {
        let same_pdir = old_dirp == new_dirp;
        let mut numb = (*old_ip).i_num;

        if !new_ip.is_null() {
            if odir {
                r = remove_dir(new_dirp, new_ip, new_name);
            } else {
                r = unlink_file(new_dirp, new_ip, new_name);
            }
        }

        if r == OK {
            if same_pdir {
                let _ = search_dir(
                    old_dirp,
                    old_name,
                    core::ptr::null_mut(),
                    DELETE,
                    IGN_PERM,
                    0,
                );
                let _ = search_dir(
                    old_dirp,
                    new_name,
                    &mut { 0u32 } as *mut u32,
                    ENTER,
                    IGN_PERM,
                    ((*old_ip).i_mode & I_TYPE) as i32,
                );
            } else {
                r = search_dir(
                    new_dirp,
                    new_name,
                    &mut numb as *mut u32,
                    ENTER,
                    IGN_PERM,
                    ((*old_ip).i_mode & I_TYPE) as i32,
                );
                if r == OK {
                    let _ = search_dir(
                        old_dirp,
                        old_name,
                        core::ptr::null_mut(),
                        DELETE,
                        IGN_PERM,
                        0,
                    );
                }
            }
        }
    }

    put_inode(old_dirp);
    put_inode(old_ip);
    put_inode(new_dirp);
    if !new_ip.is_null() {
        put_inode(new_ip);
    }

    if r == EXDEV || r == EINVAL || r == EISDIR || r == ENOTDIR {
        r
    } else {
        OK
    }
}

/// fs_ftrunc — truncate a file.
pub unsafe fn fs_ftrunc() -> i32 {
    let ext2 = glo::ext2_ptr();

    let ino = (*ext2).fs_m_in_type as u32; // FIXME: proper message parsing
    let rip = find_inode((*ext2).fs_dev, ino);
    if rip.is_null() {
        return EINVAL;
    }

    let start: u64 = 0; // FIXME: parse trc_start from message
    let end: u64 = 0; // FIXME: parse trc_end from message

    let r = if end == 0 {
        truncate_inode(rip, start)
    } else {
        // freesp_inode not yet implemented
        EINVAL
    };

    r
}

// ── Static helpers from link.c ──

unsafe fn remove_dir(rldirp: *mut Inode, rip: *mut Inode, dir_name: &[u8]) -> i32 {
    // search_dir checks that rip is a directory
    let r = search_dir(rip, &[], core::ptr::null_mut(), IS_EMPTY, IGN_PERM, 0);
    if r != OK {
        return r;
    }

    if dir_name == DOT1 || dir_name == DOT2 {
        return EINVAL;
    }
    if (*rip).i_num == ROOT_INODE {
        return EBUSY;
    }

    let r = unlink_file(rldirp, rip, dir_name);
    if r != OK {
        return r;
    }

    let _ = unlink_file(rip, core::ptr::null_mut(), &DOT1);
    let _ = unlink_file(rip, core::ptr::null_mut(), &DOT2);
    OK
}

unsafe fn unlink_file(dirp: *mut Inode, rip: *mut Inode, file_name: &[u8]) -> i32 {
    let mut r;
    let mut numb = 0u32;

    let rip_owned: *mut Inode;
    if rip.is_null() {
        let err = search_dir(dirp, file_name, &mut numb as *mut u32, LOOK_UP, IGN_PERM, 0);
        if err != OK {
            return err;
        }
        rip_owned = get_inode((*dirp).i_dev, numb);
        if rip_owned.is_null() {
            return (*glo::ext2_ptr()).err_code;
        }
    } else {
        rip_owned = rip;
        dup_inode(rip_owned);
    }

    r = search_dir(dirp, file_name, core::ptr::null_mut(), DELETE, IGN_PERM, 0);

    if r == OK {
        (*rip_owned).i_links_count = (*rip_owned).i_links_count.saturating_sub(1);
        (*rip_owned).i_update |= CTIME;
        (*rip_owned).i_dirt = IN_DIRTY;
    }

    put_inode(rip_owned);
    r
}
