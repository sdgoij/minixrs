//! Link, unlink, rename, rdlink — adapted from `minix/fs/ext2/link.c`

use libs::libminixfs::cache::{lmfs_get_block_ino, lmfs_put_block};
use libs::libminixfs::constants::{DIRECTORY_BLOCK, NORMAL};

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::inode::*;
use crate::ext2::path::*;
use crate::ext2::read::read_map;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// Offset of the second entry name in `user_path`. A rename carries two names,
/// so each gets a slot of its own.
const NAME_SLOT: usize = EXT2_NAME_MAX + 1;

/* fs_rename()'s "the two names are the same entry" marker */
const SAME: i32 = 1000;

/// Copy a request's entry name into `user_path[off..]`, NUL-terminated.
///
/// The names travel as grants rather than in the message, because they live in
/// the caller's address space; on the target they are copied through the grant
/// VFS created. A host build has no VFS and no kernel to copy through and takes
/// `user_path` as already holding the name — the one input a host test provides
/// for itself, in the place the target's copy lands.
///
/// The length bound is the `len > NAME_MAX + 1` check every C caller makes.
unsafe fn load_name(gid: i32, len: u64, off: usize) -> i32 {
    if len == 0 || len > (EXT2_NAME_MAX + 1) as u64 {
        return ENAMETOOLONG;
    }
    #[cfg(target_os = "minix")]
    {
        let dst = (*glo::ext2_ptr()).user_path.as_mut_ptr().add(off);
        let r = crate::ext2::read::safecopy_from_grant(gid, 0, dst, len as usize);
        if r != OK {
            return r;
        }
        core::ptr::write(dst.add(len as usize), 0);
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (gid, off);
    }
    OK
}

/// fs_link — create a hard link.
///
/// Message layout (VFS `req_link`): inode (u32) at payload[0], dir_ino (u32) at
/// payload[4], name grant (i32) at payload[8], path length (u64) at payload[16].
///
/// Reference: link.c fs_link()
pub unsafe fn fs_link() -> i32 {
    let ext2 = glo::ext2_ptr();
    let raw = (*ext2).m_in.m_payload.raw;

    let r = load_name(payload_i32(&raw, 8), payload_u64(&raw, 16), 0);
    if r != OK {
        return r;
    }

    // Temporarily open the file.
    let rip = get_inode((*ext2).fs_dev, payload_u32(&raw, 0));
    if rip.is_null() {
        return EINVAL;
    }

    let mut r = OK;
    if (*rip).i_links_count >= LINK_MAX {
        r = EMLINK;
    }
    // Only the super user may link to directories.
    if r == OK && ((*rip).i_mode & I_TYPE) == I_DIRECTORY && (*ext2).caller_uid as u32 != SU_UID {
        r = EPERM;
    }
    if r != OK {
        put_inode(rip);
        return r;
    }

    // Temporarily open the directory the name goes into.
    let ip = get_inode((*ext2).fs_dev, payload_u32(&raw, 4));
    if ip.is_null() {
        put_inode(rip);
        return EINVAL;
    }
    if (*ip).i_links_count == NO_LINK {
        put_inode(rip);
        put_inode(ip);
        return ENOENT;
    }

    let string = path_name(&(*ext2).user_path);

    // If the name exists in full (even without room for it in the directory),
    // that is an error.
    let new_ip = advance(ip, string, IGN_PERM);
    if new_ip.is_null() {
        let ec = (*ext2).err_code;
        r = if ec == ENOENT { OK } else { ec };
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

/// fs_unlink — remove a link, or a directory when VFS sent REQ_RMDIR.
///
/// Message layout (VFS `req_unlink`/`req_rmdir`): the parent directory inode
/// (u32) at payload[0], name grant (i32) at payload[8], path length (u64) at
/// payload[16].
///
/// Reference: link.c fs_unlink()
pub unsafe fn fs_unlink() -> i32 {
    let ext2 = glo::ext2_ptr();
    let raw = (*ext2).m_in.m_payload.raw;

    let r = load_name(payload_i32(&raw, 8), payload_u64(&raw, 16), 0);
    if r != OK {
        return r;
    }

    // Temporarily open the directory.
    let rldirp = get_inode((*ext2).fs_dev, payload_u32(&raw, 0));
    if rldirp.is_null() {
        return EINVAL;
    }

    let string = path_name(&(*ext2).user_path);

    // The last directory exists; does the entry?
    let rip = advance(rldirp, string, IGN_PERM);
    let mut r = (*ext2).err_code;
    if r != OK {
        // Mount point?
        if r == EENTERMOUNT || r == ELEAVEMOUNT {
            put_inode(rip);
            r = EBUSY;
        }
        put_inode(rldirp);
        return r;
    }

    let read_only = match (*rip).i_sp {
        Some(ref sp) => sp.s_rd_only != 0,
        None => true,
    };

    if read_only {
        r = EROFS;
    } else if (*ext2).req_nr == REQ_UNLINK - FS_BASE {
        // Unlink may be used by the super user to do dangerous things; rmdir
        // may not.
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

/// fs_rdlink — read a symlink's target into the caller's buffer.
///
/// Message layout (VFS `req_rdlink`): inode (u32) at payload[0], buffer grant
/// (i32) at payload[8], buffer size (u64) at payload[16]. The reply carries the
/// number of bytes copied at payload[0] (u64).
///
/// Reference: link.c fs_rdlink()
pub unsafe fn fs_rdlink() -> i32 {
    let ext2 = glo::ext2_ptr();
    let raw = (*ext2).m_in.m_payload.raw;

    let mut copylen = min_u(payload_u64(&raw, 16) as usize, UMAX_FILE_POS as usize);
    let gid = payload_i32(&raw, 8);

    // Temporarily open the file.
    let rip = get_inode((*ext2).fs_dev, payload_u32(&raw, 0));
    if rip.is_null() {
        return EINVAL;
    }

    let mut bp = core::ptr::null_mut();
    let link_text: *const u8;
    let mut r = OK;

    if (*rip).i_size as usize >= MAX_FAST_SYMLINK_LENGTH {
        // Normal symlink: the target lives in the first data block.
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
        // Fast symlink: the target is stored in the inode's block array.
        link_text = (*rip).i_block.as_ptr() as *const u8;
    }

    if r == OK {
        copylen = min_u(copylen, (*rip).i_size as usize);
        #[cfg(target_os = "minix")]
        {
            r = crate::ext2::read::safecopy_to_grant(gid, 0, link_text, copylen);
        }
        #[cfg(not(target_os = "minix"))]
        {
            // There is no kernel to copy through; the walk still runs and the
            // count it resolved is recorded (in `cch[0]`) as well as replied.
            let _ = (gid, link_text);
            (*ext2).cch[0] = copylen as i32;
        }
        if r == OK {
            (&mut (*ext2).m_out.m_payload.raw)[0..8]
                .copy_from_slice(&(copylen as u64).to_le_bytes());
        }
    }

    if !bp.is_null() {
        lmfs_put_block(bp, DIRECTORY_BLOCK);
    }

    put_inode(rip);
    r
}

/// fs_rename — rename an entry.
///
/// Message layout (VFS `req_rename`): old dir (u32) at payload[0], new dir
/// (u32) at payload[4], old name length (u64) at payload[8], new name length
/// (u64) at payload[16], old name grant (i32) at payload[24], new name grant
/// (i32) at payload[28].
///
/// Reference: link.c fs_rename()
pub unsafe fn fs_rename() -> i32 {
    let ext2 = glo::ext2_ptr();
    let raw = (*ext2).m_in.m_payload.raw;

    let mut r = load_name(payload_i32(&raw, 24), payload_u64(&raw, 8), 0);
    if r != OK {
        return r;
    }
    r = load_name(payload_i32(&raw, 28), payload_u64(&raw, 16), NAME_SLOT);
    if r != OK {
        return r;
    }

    let old_name = path_name(&(*ext2).user_path);
    let new_name = path_name(&(&(*ext2).user_path)[NAME_SLOT..]);

    // Get the old dir inode.
    let old_dirp = get_inode((*ext2).fs_dev, payload_u32(&raw, 0));
    if old_dirp.is_null() {
        return (*ext2).err_code;
    }

    let mut old_ip = advance(old_dirp, old_name, IGN_PERM);
    let mut r = (*ext2).err_code;

    if r == EENTERMOUNT || r == ELEAVEMOUNT {
        put_inode(old_ip);
        old_ip = core::ptr::null_mut();
        r = if r == EENTERMOUNT { EXDEV } else { EINVAL };
    } else if old_ip.is_null() {
        put_inode(old_dirp);
        return (*ext2).err_code;
    }

    // Get the new dir inode.
    let mut new_dirp = get_inode((*ext2).fs_dev, payload_u32(&raw, 4));
    if new_dirp.is_null() {
        put_inode(old_ip);
        put_inode(old_dirp);
        return (*ext2).err_code;
    }
    if (*new_dirp).i_links_count == NO_LINK {
        // The directory does not actually exist.
        put_inode(old_ip);
        put_inode(old_dirp);
        put_inode(new_dirp);
        return ENOENT;
    }

    // The new name is not required to exist.
    let mut new_ip = advance(new_dirp, new_name, IGN_PERM);
    if (*ext2).err_code == EENTERMOUNT {
        put_inode(new_ip);
        new_ip = core::ptr::null_mut();
        r = EBUSY;
    }

    let odir = !old_ip.is_null() && ((*old_ip).i_mode & I_TYPE) == I_DIRECTORY;

    let mut same_pdir = false;

    // If it is ok, check for a variety of possible errors.
    if r == OK {
        same_pdir = old_dirp == new_dirp;

        // The old inode must not be a super directory of the new last dir.
        if odir && !same_pdir {
            let mut superdirp = new_dirp;
            dup_inode(superdirp);
            loop {
                if superdirp == old_ip {
                    put_inode(superdirp);
                    r = EINVAL;
                    break;
                }
                let next = advance(superdirp, &DOT2, IGN_PERM);
                put_inode(superdirp);
                if next == superdirp {
                    put_inode(next);
                    break;
                }
                if (*ext2).err_code == ELEAVEMOUNT {
                    // Back at the top of a mount: the cross-device case was
                    // VFS's to check.
                    put_inode(next);
                    (*ext2).err_code = OK;
                    break;
                }
                if next.is_null() {
                    // A missing "..": assume the worst.
                    r = EINVAL;
                    break;
                }
                superdirp = next;
            }
        }

        // Neither name may be "." or "..".
        if old_name == b"." || old_name == b".." || new_name == b"." || new_name == b".." {
            r = EINVAL;
        }

        // Some tests apply only if the new path exists.
        if new_ip.is_null() {
            // Only a directory gains a ".." entry here, so only a directory can
            // push its new parent over the link limit.
            if odir && (*new_dirp).i_links_count >= LINK_MAX && !same_pdir && r == OK {
                r = EMLINK;
            }
        } else {
            if old_ip == new_ip {
                r = SAME;
            }
            let ndir = ((*new_ip).i_mode & I_TYPE) == I_DIRECTORY;
            if odir && !ndir {
                r = ENOTDIR;
            }
            if !odir && ndir {
                r = EISDIR;
            }
        }
    }

    // The rename will probably work. Only two things can go wrong now: being
    // unable to remove the new file, or being unable to make the new entry.
    if r == OK && !new_ip.is_null() {
        r = if odir {
            remove_dir(new_dirp, new_ip, new_name)
        } else {
            unlink_file(new_dirp, new_ip, new_name)
        };
    }

    if r == OK {
        let mut numb = (*old_ip).i_num;

        // In the same directory the old name has to go first to free an entry
        // for the new one; otherwise the new entry is made first, so that a
        // full destination cannot leave the entry nowhere.
        if same_pdir {
            r = search_dir(
                old_dirp,
                old_name,
                core::ptr::null_mut(),
                DELETE,
                IGN_PERM,
                0,
            );
            if r == OK {
                let _ = search_dir(
                    old_dirp,
                    new_name,
                    &mut numb as *mut u32,
                    ENTER,
                    IGN_PERM,
                    ((*old_ip).i_mode & I_TYPE) as i32,
                );
            }
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

    // A directory that changed parents still points ".." at the old one.
    if r == OK && odir && !same_pdir {
        let mut numb = (*new_dirp).i_num;
        let _ = unlink_file(old_ip, core::ptr::null_mut(), &DOT2);
        if search_dir(
            old_ip,
            &DOT2,
            &mut numb as *mut u32,
            ENTER,
            IGN_PERM,
            I_DIRECTORY as i32,
        ) == OK
        {
            (*new_dirp).i_links_count += 1;
            (*new_dirp).i_dirt = IN_DIRTY;
        }
    }

    put_inode(old_dirp);
    put_inode(old_ip);
    put_inode(new_dirp);
    put_inode(new_ip);

    if r == SAME { OK } else { r }
}

/// fs_ftrunc — cut an inode down to a size, or free a byte range of it.
///
/// Message layout (VFS `req_ftrunc`): inode (u32) at payload[0], `trc_start`
/// (i64) at payload[8], `trc_end` (i64) at payload[16]. A zero `trc_end` means
/// "truncate to `trc_start`"; otherwise the half-open range `[start, end)` is
/// freed.
///
/// Reference: link.c fs_ftrunc()
pub unsafe fn fs_ftrunc() -> i32 {
    let ext2 = glo::ext2_ptr();
    let raw = (*ext2).m_in.m_payload.raw;

    let rip = find_inode((*ext2).fs_dev, payload_u32(&raw, 0));
    if rip.is_null() {
        return EINVAL;
    }

    let start = payload_i64(&raw, 8);
    let end = payload_i64(&raw, 16);

    if end == 0 {
        truncate_inode(rip, start as u64)
    } else {
        freesp_inode(rip, start as u64, end as u64)
    }
}

unsafe fn remove_dir(rldirp: *mut Inode, rip: *mut Inode, dir_name: &[u8]) -> i32 {
    // search_dir checks that rip is a directory
    let r = search_dir(rip, &[], core::ptr::null_mut(), IS_EMPTY, IGN_PERM, 0);
    if r != OK {
        return r;
    }

    if dir_name == b"." || dir_name == b".." {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_name_bounds() {
        assert_eq!(unsafe { load_name(0, 0, 0) }, ENAMETOOLONG);
        assert_eq!(
            unsafe { load_name(0, EXT2_NAME_MAX as u64 + 2, 0) },
            ENAMETOOLONG
        );
        assert_eq!(unsafe { load_name(0, 5, 0) }, OK);
    }
}
