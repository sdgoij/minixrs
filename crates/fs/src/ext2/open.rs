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
///
/// The main loop unpacked the request into `cch` (dir_ino, mode, uid, gid)
/// and the name into `user_path`.
///
/// Reference: open.c fs_create()
pub unsafe fn fs_create() -> i32 {
    let ext2 = glo::ext2_ptr();

    let dir_ino = (*ext2).cch[0] as u32;
    let mode = (*ext2).cch[1] as u16;
    let uid = (*ext2).cch[2] as u16;
    let gid = (*ext2).cch[3] as u16;
    let omode = I_REGULAR | (mode & RWX_MODES);

    (*ext2).caller_uid = uid;
    (*ext2).caller_gid = gid;

    let ldirp = get_inode((*ext2).fs_dev, dir_ino);
    if ldirp.is_null() {
        return ENOENT;
    }

    let string = path_name(&(*ext2).user_path);
    let rip = new_node(ldirp, string, omode, NO_BLOCK);
    let r = (*ext2).err_code;

    if r != OK {
        put_inode(ldirp);
        if !rip.is_null() {
            put_inode(rip);
        }
        return r;
    }

    // VFS's create reply: file_size (i64) at payload[0], inode (u32) at
    // payload[8], mode (u16) at payload[12]. The new file's reference stays with
    // VFS (C: only the error path above puts it back; only the parent directory
    // is put here), and `REQ_PUTNODE` is how VFS hands that reference back.
    if !rip.is_null() {
        let raw = &mut (*ext2).m_out.m_payload.raw;
        raw[0..8].copy_from_slice(&((*rip).i_size as i64).to_le_bytes());
        raw[8..12].copy_from_slice(&(*rip).i_num.to_le_bytes());
        raw[12..14].copy_from_slice(&((*rip).i_mode as u16).to_le_bytes());
    }

    put_inode(ldirp);
    OK
}

/// fs_mkdir — create a directory.
///
/// Reference: open.c fs_mkdir()
pub unsafe fn fs_mkdir() -> i32 {
    let ext2 = glo::ext2_ptr();

    let dir_ino = (*ext2).cch[0] as u32;
    let mode = (*ext2).cch[1] as u16;
    let uid = (*ext2).cch[2] as u16;
    let gid = (*ext2).cch[3] as u16;
    let bits = I_DIRECTORY | (mode & RWX_MODES);

    (*ext2).caller_uid = uid;
    (*ext2).caller_gid = gid;

    let ldirp = get_inode((*ext2).fs_dev, dir_ino);
    if ldirp.is_null() {
        return ENOENT;
    }

    let string = path_name(&(*ext2).user_path);
    let rip = new_node(ldirp, string, bits, NO_BLOCK);
    let mut r = (*ext2).err_code;

    if rip.is_null() || r == EEXIST {
        if !rip.is_null() {
            put_inode(rip);
        }
        put_inode(ldirp);
        return r;
    }

    let dotdot = (*ldirp).i_num;
    let mut dot = (*rip).i_num;

    (*rip).i_mode = bits;

    // Enter . and .. in the new directory.
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
        r = OK;
    } else {
        // Failed to enter . or .. — undo the directory entry.
        let _ = search_dir(ldirp, string, core::ptr::null_mut(), DELETE, IGN_PERM, 0);
        (*rip).i_links_count = (*rip).i_links_count.saturating_sub(1);
        r = if r1 != OK { r1 } else { r2 };
    }
    (*rip).i_dirt = IN_DIRTY;

    put_inode(ldirp);
    put_inode(rip);
    r
}

/// fs_mknod — create a special file (device node).
///
/// Reference: open.c fs_mknod()
pub unsafe fn fs_mknod() -> i32 {
    let ext2 = glo::ext2_ptr();

    let dir_ino = (*ext2).cch[0] as u32;
    let mode = (*ext2).cch[1] as u16;
    let device = (*ext2).cch[2] as u32;
    let uid = (*ext2).cch[3] as u16;
    let gid = (*ext2).cch[4] as u16;

    (*ext2).caller_uid = uid;
    (*ext2).caller_gid = gid;

    let ldirp = get_inode((*ext2).fs_dev, dir_ino);
    if ldirp.is_null() {
        return ENOENT;
    }

    let string = path_name(&(*ext2).user_path);
    let ip = new_node(ldirp, string, mode, device);
    let r = (*ext2).err_code;

    put_inode(ip);
    put_inode(ldirp);
    r
}

/// fs_slink — create a symbolic link.
///
/// Message layout (VFS `req_slink`): dir inode (u32) at payload[0], name
/// length (u64) at payload[8], target length (u64) at payload[16], name grant
/// (i32) at payload[24], target grant (i32) at payload[28], uid (u16) at
/// payload[32], gid (u16) at payload[34]. Both strings arrive through grants,
/// so they can only be read on the target.
///
/// Reference: open.c fs_slink()
pub unsafe fn fs_slink() -> i32 {
    let ext2 = glo::ext2_ptr();
    let payload = (*ext2).m_in.m_payload.raw;

    let dir_ino = u32::from_ne_bytes(payload[0..4].try_into().unwrap_or([0u8; 4]));
    let name_len = u64::from_ne_bytes(payload[8..16].try_into().unwrap_or([0u8; 8])) as usize;
    let target_len = u64::from_ne_bytes(payload[16..24].try_into().unwrap_or([0u8; 8])) as usize;
    let name_gid = i32::from_ne_bytes(payload[24..28].try_into().unwrap_or([0u8; 4]));
    let target_gid = i32::from_ne_bytes(payload[28..32].try_into().unwrap_or([0u8; 4]));
    let uid = u16::from_ne_bytes(payload[32..34].try_into().unwrap_or([0u8; 2]));
    let gid = u16::from_ne_bytes(payload[34..36].try_into().unwrap_or([0u8; 2]));

    if name_len == 0 || name_len > EXT2_NAME_MAX || name_len >= PATH_MAX {
        return EINVAL;
    }

    (*ext2).caller_uid = uid;
    (*ext2).caller_gid = gid;

    let mut target = [0u8; PATH_MAX];
    let len = target_len.min(PATH_MAX - 1);
    let user_path = core::ptr::addr_of_mut!((*ext2).user_path);

    #[cfg(target_os = "minix")]
    {
        let r = crate::ext2::read::safecopy_from_grant(
            name_gid,
            0,
            (*user_path).as_mut_ptr(),
            name_len,
        );
        if r != OK {
            return r;
        }
        core::ptr::write((*user_path).as_mut_ptr().add(name_len), 0);
        if len > 0 {
            let r = crate::ext2::read::safecopy_from_grant(target_gid, 0, target.as_mut_ptr(), len);
            if r != OK {
                return r;
            }
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // No kernel to copy through on the host: an empty target is the only
        // thing this path can produce, so a non-empty one is an error rather
        // than a link with garbage in it.
        let _ = (name_gid, target_gid);
        if len > 0 {
            return EIO;
        }
    }

    let string = core::slice::from_raw_parts((*user_path).as_ptr(), name_len);

    let ldirp = get_inode((*ext2).fs_dev, dir_ino);
    if ldirp.is_null() {
        return EINVAL;
    }

    let sip = new_node(ldirp, string, I_SYMBOLIC_LINK | RWX_MODES, NO_BLOCK);
    let mut r = (*ext2).err_code;

    if r == OK && !sip.is_null() {
        let block_size = match (*sip).i_sp {
            Some(ref sp) => sp.s_block_size as usize,
            None => 0,
        };
        if len + 1 > block_size {
            r = ENAMETOOLONG;
        } else if len + 1 <= MAX_FAST_SYMLINK_LENGTH {
            // Fast symlink: the target is stored in the inode's block array.
            let dst = (*sip).i_block.as_mut_ptr() as *mut u8;
            core::ptr::copy_nonoverlapping(target.as_ptr(), dst, len);
            (*sip).i_dirt = IN_DIRTY;
        } else {
            // Slow symlink: the target goes in the first data block.
            let bp = new_block(sip, 0);
            if bp.is_null() {
                r = (*ext2).err_code;
                if r == OK {
                    r = EIO;
                }
            } else {
                core::ptr::copy_nonoverlapping(target.as_ptr(), b_data(bp), len);
                lmfs_markdirty(bp);
                lmfs_put_block(bp, DIRECTORY_BLOCK);
            }
        }
        (*sip).i_size = len as u32;

        if r != OK {
            (*sip).i_links_count = NO_LINK;
            let _ = search_dir(ldirp, string, core::ptr::null_mut(), DELETE, IGN_PERM, 0);
        }
    }

    put_inode(sip);
    put_inode(ldirp);
    r
}

/// fs_inhibread — inhibit read ahead for an inode.
///
/// Message layout (VFS `req_inhibread`): inode (u32) at payload[0].
pub unsafe fn fs_inhibread() -> i32 {
    let ext2 = glo::ext2_ptr();
    let payload = (*ext2).m_in.m_payload.raw;
    let ino = u32::from_ne_bytes(payload[0..4].try_into().unwrap_or([0u8; 4]));
    let rip = find_inode((*ext2).fs_dev, ino);
    if rip.is_null() {
        return EINVAL;
    }
    (*rip).i_seek = ISEEK;
    OK
}

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
