//! File/dir/symlink creation — adapted from `minix/fs/ext2/open.c`

use libs::libminixfs::cache::{lmfs_get_block_ino, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::{
    DIRECTORY_BLOCK, FULL_DATA_BLOCK, NO_READ, NORMAL, VMC_NO_INODE,
};

use crate::ext2::balloc::*;
use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::ialloc::alloc_inode;
use crate::ext2::inode::*;
use crate::ext2::path::*;
use crate::ext2::read::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;
use crate::ext2::write::*;

/// fs_create — create a regular file.
pub unsafe fn fs_create() -> i32 {
    let ext2 = glo::ext2_ptr();

    // FIXME: parse mode, uid, gid, path from message
    let omode = I_REGULAR | RWX_MODES;
    let _ = (*ext2).caller_uid;
    let _ = (*ext2).caller_gid;

    // Get last directory inode
    let dir_ino = (*ext2).fs_m_in_type as u32;
    let ldirp = get_inode((*ext2).fs_dev, dir_ino);
    if ldirp.is_null() {
        return ENOENT;
    }

    // FIXME: copy last component name from grant
    let string = &(&(*ext2).user_path)[..EXT2_NAME_MAX + 1];

    let rip = new_node(ldirp, string, omode, NO_BLOCK);
    let r = (*ext2).err_code;

    if r != OK {
        put_inode(ldirp);
        if !rip.is_null() {
            put_inode(rip);
        }
        return r;
    }

    // Reply would set inode, mode, file_size, uid, gid
    let _ = rip;
    put_inode(ldirp);

    OK
}

/// fs_mkdir — create a directory.
pub unsafe fn fs_mkdir() -> i32 {
    let ext2 = glo::ext2_ptr();

    let dir_ino = (*ext2).fs_m_in_type as u32; // FIXME: proper message parsing
    let ldirp = get_inode((*ext2).fs_dev, dir_ino);
    if ldirp.is_null() {
        return ENOENT;
    }

    let string = &(&(*ext2).user_path)[..EXT2_NAME_MAX + 1];

    // Create the inode for the new directory
    let mode = I_DIRECTORY | RWX_MODES; // FIXME: parse from message
    let rip = new_node(ldirp, string, mode, NO_BLOCK);
    let mut r = (*ext2).err_code;

    if rip.is_null() || r == EEXIST {
        if !rip.is_null() {
            put_inode(rip);
        }
        put_inode(ldirp);
        return r;
    }

    let dotdot = (*ldirp).i_num; // parent's inode number
    let mut dot = (*rip).i_num; // new dir's own inode number

    // Set mode
    (*rip).i_mode = mode;

    // Enter . and .. in the new directory
    let r1 = search_dir(
        rip,
        &DOT1,
        &mut dot as *mut u32,
        ENTER,
        IGN_PERM,
        I_DIRECTORY as i32,
    );
    let mut dotdot_numb = dotdot;
    let r2 = search_dir(
        rip,
        &DOT2,
        &mut dotdot_numb as *mut u32,
        ENTER,
        IGN_PERM,
        I_DIRECTORY as i32,
    );

    if r1 == OK && r2 == OK {
        (*rip).i_links_count = (*rip).i_links_count.wrapping_add(1); // .
        (*ldirp).i_links_count = (*ldirp).i_links_count.wrapping_add(1); // ..
        (*ldirp).i_dirt = IN_DIRTY;
    } else {
        // Failed to enter . or .. — undo
        let _ = search_dir(ldirp, string, core::ptr::null_mut(), DELETE, IGN_PERM, 0);
        (*rip).i_links_count = (*rip).i_links_count.saturating_sub(1);
    }
    (*rip).i_dirt = IN_DIRTY;

    put_inode(ldirp);
    put_inode(rip);
    r
}

/// fs_mknod — create a special file (device node).
pub unsafe fn fs_mknod() -> i32 {
    let ext2 = glo::ext2_ptr();

    let dir_ino = (*ext2).fs_m_in_type as u32;
    let ldirp = get_inode((*ext2).fs_dev, dir_ino);
    if ldirp.is_null() {
        return ENOENT;
    }

    let string = &(&(*ext2).user_path)[..EXT2_NAME_MAX + 1];
    let mode = I_BLOCK_SPECIAL | RWX_MODES; // FIXME: parse from message
    let dev_num: u32 = 0; // FIXME: parse device from message

    let ip = new_node(ldirp, string, mode, dev_num);

    put_inode(ip);
    put_inode(ldirp);
    (*ext2).err_code
}

/// fs_slink — create a symbolic link.
pub unsafe fn fs_slink() -> i32 {
    let ext2 = glo::ext2_ptr();

    let dir_ino = (*ext2).fs_m_in_type as u32; // FIXME: proper message parsing

    // Temporarily open the dir
    let ldirp = get_inode((*ext2).fs_dev, dir_ino);
    if ldirp.is_null() {
        return EINVAL;
    }

    let string = &(&(*ext2).user_path)[..EXT2_NAME_MAX + 1];

    // Create the inode for the symlink
    let sip = new_node(ldirp, string, I_SYMBOLIC_LINK | RWX_MODES, NO_BLOCK);
    let mut r = (*ext2).err_code;

    if r == OK && !sip.is_null() {
        let mem_size: usize = 0; // FIXME: parse from message
        let block_size = (*(*sip).i_sp.as_ref().unwrap()).s_block_size as usize;

        if mem_size + 1 > block_size {
            r = ENAMETOOLONG;
        } else if mem_size + 1 <= MAX_FAST_SYMLINK_LENGTH {
            // Fast symlink: store in i_block
            // FIXME: copy from grant into sip->i_block
            let _ = &mut (*sip).i_block;
            (*sip).i_dirt = IN_DIRTY;
        } else {
            // Slow symlink: allocate a data block
            let bp = new_block(sip, 0);
            if !bp.is_null() {
                // FIXME: copy from grant into bp->data_ptr
                lmfs_markdirty(bp);
                lmfs_put_block(bp, DIRECTORY_BLOCK);
            } else {
                r = (*ext2).err_code;
            }
        }

        if r == OK {
            // Set symlink size
            let target_len: usize = 0; // FIXME: from copied data
            (*sip).i_size = target_len as u32;
        }

        if r != OK {
            (*sip).i_links_count = NO_LINK;
            let _ = search_dir(ldirp, string, core::ptr::null_mut(), DELETE, IGN_PERM, 0);
        }
    }

    put_inode(sip);
    put_inode(ldirp);
    r
}

/// fs_inhibread — inhibit read ahead.
pub unsafe fn fs_inhibread() -> i32 {
    let ext2 = glo::ext2_ptr();
    let ino = (*ext2).fs_m_in_type as u32;
    let rip = find_inode((*ext2).fs_dev, ino);
    if rip.is_null() {
        return EINVAL;
    }
    (*rip).i_seek = ISEEK;
    OK
}

// ── internal helper: new_node ──

unsafe fn new_node(ldirp: *mut Inode, string: &[u8], bits: u16, z0: u32) -> *mut Inode {
    let ext2 = glo::ext2_ptr();

    if (*ldirp).i_links_count == NO_LINK {
        (*ext2).err_code = ENOENT;
        return core::ptr::null_mut();
    }

    // Try to advance to see if file already exists
    let rip = advance(ldirp, string, IGN_PERM);

    if (bits & I_TYPE) == I_DIRECTORY
        && ((*ldirp).i_links_count >= LINK_MAX || (*ldirp).i_links_count >= LINK_MAX)
    {
        put_inode(rip);
        (*ext2).err_code = EMLINK;
        return core::ptr::null_mut();
    }

    if rip.is_null() && (*ext2).err_code == ENOENT {
        // Last component does not exist — allocate new inode
        let new_rip = alloc_inode(ldirp, bits);
        if new_rip.is_null() {
            return core::ptr::null_mut();
        }

        (*new_rip).i_links_count = (*new_rip).i_links_count.wrapping_add(1);
        (*new_rip).i_block[0] = z0; // device number for special files
        rw_inode(new_rip, WRITING);

        // Make directory entry
        let r = search_dir(
            ldirp,
            string,
            &mut (*new_rip).i_num as *mut u32,
            ENTER,
            IGN_PERM,
            ((*new_rip).i_mode & I_TYPE) as i32,
        );
        if r != OK {
            (*new_rip).i_links_count = (*new_rip).i_links_count.saturating_sub(1);
            (*new_rip).i_dirt = IN_DIRTY;
            put_inode(new_rip);
            (*ext2).err_code = r;
            return core::ptr::null_mut();
        }

        (*ext2).err_code = OK;
        return new_rip;
    } else if (*ext2).err_code == EENTERMOUNT || (*ext2).err_code == ELEAVEMOUNT {
        (*ext2).err_code = EEXIST;
    } else {
        if !rip.is_null() {
            (*ext2).err_code = EEXIST;
        } else {
            (*ext2).err_code = (*ext2).err_code;
        }
    }

    rip
}
