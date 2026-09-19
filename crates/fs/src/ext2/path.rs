//! Path lookup and directory search — adapted from `minix/fs/ext2/path.c`

use libs::libminixfs::cache::{lmfs_get_block_ino, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::{
    DIRECTORY_BLOCK, FULL_DATA_BLOCK, NO_READ, NORMAL, VMC_NO_INODE,
};
use libs::libminixfs::credentials::fs_lookup_credentials;

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::protect::*;
use crate::ext2::read::read_map;
use crate::ext2::super_::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;
use crate::ext2::write::new_block;

/// Fill the lookup reply VFS reads back (VFS `req_lookup`): file_size (i64)
/// at payload[8], device (u32) at payload[16], inode (u32) at payload[20],
/// mode (u32) at payload[24].
unsafe fn fill_lookup_reply(rip: *mut Inode) {
    let raw = &mut (*glo::ext2_ptr()).m_out.m_payload.raw;
    let size = (*rip).i_size as i64;
    raw[8..16].copy_from_slice(&size.to_le_bytes());
    raw[16..20].copy_from_slice(&(*rip).i_block[0].to_le_bytes());
    raw[20..24].copy_from_slice(&(*rip).i_num.to_le_bytes());
    raw[24..28].copy_from_slice(&((*rip).i_mode as u32).to_le_bytes());
}

/// Expand the symbolic link `rip` in place: the link text is prepended to the
/// remaining path in `user_path`, so the walk restarts at the target.
///
/// Reference: path.c ltraverse()
unsafe fn ltraverse(rip: *mut Inode, suffix_offset: usize, path_len: usize) -> i32 {
    let ext2 = glo::ext2_ptr();
    let llen = (*rip).i_size as usize;
    let slen = path_len.saturating_sub(suffix_offset);
    let up = (*ext2).user_path.as_mut_ptr();

    if slen + llen + 1 > PATH_MAX {
        return ENAMETOOLONG;
    }

    let bp;
    let text: *const u8;
    if llen >= MAX_FAST_SYMLINK_LENGTH {
        // Normal symlink: the target lives in the first data block.
        let b = read_map(rip, 0, 0);
        if b == NO_BLOCK {
            return EIO;
        }
        bp = lmfs_get_block_ino((*rip).i_dev, b as u64, NORMAL, (*rip).i_num as u64, 0);
        if bp.is_null() {
            return EIO;
        }
        text = b_data(bp);
    } else {
        // Fast symlink: the target is stored in the inode's block array.
        bp = core::ptr::null_mut();
        text = (*rip).i_block.as_ptr() as *const u8;
    }

    // Make room for the expanded link after the suffix, then copy the target
    // in front of it.
    core::ptr::copy(up.add(suffix_offset), up.add(llen), slen + 1);
    core::ptr::copy_nonoverlapping(text, up, llen);

    if !bp.is_null() {
        lmfs_put_block(bp, DIRECTORY_BLOCK);
    }
    OK
}

/// fs_lookup — resolve a path to an inode.
///
/// Message layout (VFS `req_lookup`; the inode fields are u32 like the C's
/// `ino_t` since the port embeds the path in the message and needs the room):
/// dir_ino (u32) at payload[0], root_ino (u32) at payload[4], uid (u16) at
/// payload[8], gid (u16) at payload[10], flags (u32) at payload[12],
/// grant_ucred (i32) at payload[16] (set when `PATH_GET_UCRED` is),
/// path_len (u32) at payload[20], path at payload[24] (NUL-terminated, capped
/// at 24 bytes).
///
/// Reference: path.c fs_lookup() + parse_path()
pub unsafe fn fs_lookup() -> i32 {
    let ext2 = glo::ext2_ptr();

    let payload = (*ext2).m_in.m_payload.raw;
    let dir_ino = payload_u32(&payload, 0);
    let root_ino = payload_u32(&payload, 4);
    let uid = u16::from_ne_bytes(payload[8..10].try_into().unwrap_or([0u8; 2]));
    let gid = u16::from_ne_bytes(payload[10..12].try_into().unwrap_or([0u8; 2]));
    let flags = payload_u32(&payload, 12) as i32;
    let grant_ucred = payload_i32(&payload, 16);
    let path_len = payload_u32(&payload, 20) as usize;

    if path_len == 0 {
        return EINVAL;
    }
    if path_len > PATH_MAX {
        return E2BIG;
    }

    // The path travels embedded in the message; copy it out and terminate it.
    let up = core::ptr::addr_of_mut!((*ext2).user_path);
    let copy_len = path_len.min(24).min(PATH_MAX - 1);
    for i in 0..copy_len {
        core::ptr::write((*up).as_mut_ptr().add(i), payload[24 + i]);
    }
    core::ptr::write((*up).as_mut_ptr().add(copy_len), 0);
    let path_len = copy_len;

    // Caller's identity: uid/gid from the request, or — when VFS shipped
    // credentials because the caller belongs to supplemental groups — the
    // block it granted (C fs_lookup_credentials()).
    if flags & PATH_GET_UCRED != 0 {
        let credentials = &mut (*ext2).credentials;
        match fs_lookup_credentials(credentials, grant_ucred) {
            Ok((cred_uid, cred_gid)) => {
                (*ext2).caller_uid = cred_uid;
                (*ext2).caller_gid = cred_gid;
            }
            Err(e) => return e,
        }
    } else {
        (*ext2).caller_uid = uid;
        (*ext2).caller_gid = gid;
    }

    let mut cp_offset: usize = 0;
    let mut symlinks: i32 = 0;
    let mut offset: usize = 0;

    // Start at the given directory. get_inode (not find_inode) is used because
    // find_inode requires i_count > 0, which fails for an inode released to
    // the unused list by an earlier request.
    let rip = get_inode((*ext2).fs_dev, dir_ino);
    if rip.is_null() {
        return ENOENT;
    }
    let mut current_rip = rip;
    let mut leaving_mount = (*current_rip).i_mountpoint != FALSE;

    loop {
        // Skip leading slashes.
        while cp_offset < path_len && (*up)[cp_offset] == b'/' {
            cp_offset += 1;
        }

        // End of path: this is the inode we were looking for.
        if cp_offset >= path_len || (*up)[cp_offset] == 0 {
            if (*current_rip).i_mountpoint != FALSE {
                fill_lookup_reply(current_rip);
                put_inode(current_rip);
                return EENTERMOUNT;
            }
            // The reference stays with VFS, which is what lets its later
            // requests find this inode at all (`find_inode` only sees inodes
            // with a reference) and what `REQ_PUTNODE` gives back. The C puts it
            // back in the EENTERMOUNT case above only.
            fill_lookup_reply(current_rip);
            return OK;
        }

        // Extract the next component.
        let comp_start = cp_offset;
        while cp_offset < path_len && (*up)[cp_offset] != b'/' && (*up)[cp_offset] != 0 {
            cp_offset += 1;
        }
        let comp_len = cp_offset - comp_start;
        let component = core::slice::from_raw_parts((*up).as_ptr().add(comp_start), comp_len);

        // ".." may leave the filesystem or be ignored at the process root.
        if comp_len == 2 && component[0] == b'.' && component[1] == b'.' {
            let r = forbidden(current_rip, X_BIT);
            if r != OK {
                put_inode(current_rip);
                return r;
            }
            if (*current_rip).i_num == root_ino {
                offset += cp_offset;
                continue;
            }
            if (*current_rip).i_num == ROOT_INODE {
                let is_root = match (*current_rip).i_sp {
                    Some(ref sp) => sp.s_is_root != 0,
                    None => false,
                };
                if !is_root {
                    put_inode(current_rip);
                    return ELEAVEMOUNT;
                }
            }
        }

        // A mountpoint hands the rest of the path back to VFS.
        if !leaving_mount && (*current_rip).i_mountpoint != FALSE {
            fill_lookup_reply(current_rip);
            put_inode(current_rip);
            return EENTERMOUNT;
        }

        // Advance through this component.
        let dir_ip = current_rip;
        let next_rip = if leaving_mount {
            advance(dir_ip, &DOT2, CHK_PERM)
        } else {
            advance(dir_ip, component, CHK_PERM)
        };
        if next_rip.is_null() {
            put_inode(dir_ip);
            return (*ext2).err_code;
        }
        current_rip = next_rip;
        leaving_mount = false;

        // Follow a symlink unless the caller asked for the link itself.
        if (*current_rip).i_mode & I_TYPE == I_SYMBOLIC_LINK {
            let next_char = if cp_offset < path_len {
                (*up)[cp_offset]
            } else {
                0
            };
            if next_char == 0 && (flags & PATH_RET_SYMLINK) != 0 {
                // The link itself is the final object, so its reference goes to
                // VFS as well (the parent directory's does not).
                put_inode(dir_ip);
                fill_lookup_reply(current_rip);
                return OK;
            }

            let r = ltraverse(current_rip, cp_offset, path_len);
            cp_offset = 0;
            offset = 0;
            symlinks += 1;

            if symlinks > _POSIX_SYMLOOP_MAX {
                put_inode(dir_ip);
                put_inode(current_rip);
                return ELOOP;
            }
            if r != OK {
                put_inode(dir_ip);
                put_inode(current_rip);
                return r;
            }
            // A link to an absolute path restarts VFS's resolution.
            if cp_offset < path_len && (*up)[cp_offset] == b'/' {
                put_inode(dir_ip);
                put_inode(current_rip);
                return ESYMLINK;
            }

            put_inode(current_rip);
            dup_inode(dir_ip);
            current_rip = dir_ip;
        }

        put_inode(dir_ip);
        offset += cp_offset;
    }
}

/// Advance to the next path component.
///
/// Leaves the failure status in the server's `err_code`, as the C original's
/// callers read it (`fs_lookup`).
pub unsafe fn advance(dirp: *mut Inode, string: &[u8], chk_perm: i32) -> *mut Inode {
    if dirp.is_null() {
        return core::ptr::null_mut();
    }

    if string.is_empty() || string[0] == 0 {
        (*glo::ext2_ptr()).err_code = ENOENT;
        return core::ptr::null_mut();
    }

    if ((*dirp).i_mode & I_TYPE) != I_DIRECTORY {
        (*glo::ext2_ptr()).err_code = ENOTDIR;
        return core::ptr::null_mut();
    }

    let mut numb = 0u32;
    let r = search_dir(dirp, string, &mut numb as *mut u32, LOOK_UP, chk_perm, 0);
    // C assigns the status unconditionally here; callers read `err_code` after
    // a successful walk as well, so a stale error from an earlier request must
    // not survive this one.
    (*glo::ext2_ptr()).err_code = r;
    if r != OK {
        return core::ptr::null_mut();
    }

    if numb == 0 {
        (*glo::ext2_ptr()).err_code = ENOENT;
        return core::ptr::null_mut();
    }

    get_inode((*dirp).i_dev, numb)
}

/// Search a directory for a string, or enter/delete an entry.
pub unsafe fn search_dir(
    ldir_ptr: *mut Inode,
    string: &[u8],
    numb: *mut u32,
    flag: i32,
    check_permissions: i32,
    ftype: i32,
) -> i32 {
    if ldir_ptr.is_null() {
        return ENOENT;
    }

    // Check if it's a directory
    if ((*ldir_ptr).i_mode & I_TYPE) != I_DIRECTORY {
        return ENOTDIR;
    }

    let mut r = OK;

    // Permission checks
    if flag != IS_EMPTY {
        let bits = if flag == LOOK_UP {
            X_BIT
        } else {
            W_BIT | X_BIT
        };

        // dot1 and dot2 don't need permissions for anything but LOOK_UP
        if string == DOT1 || string == DOT2 {
            if flag != LOOK_UP {
                r = read_only(ldir_ptr);
            }
        } else if check_permissions != 0 {
            r = forbidden(ldir_ptr, bits);
        }
    }
    if r != OK {
        return r;
    }

    let block_size = (*(*ldir_ptr).i_sp.as_ref().unwrap()).s_block_size as u64;
    let file_size = (*ldir_ptr).i_size as u64;

    let mut new_slots = 0u32;
    let mut e_hit = false;
    // Where an ENTER writes its entry: the free slot the search found, or the
    // first entry of the block appended when the directory is full.
    let mut slot_pos: u64 = 0;
    let mut match_found = false;
    let mut pos: u64 = 0;

    // For ENTER, compute required space. The name length is C's `strlen()` of
    // the argument, not the slice length: the dot entries arrive as
    // NUL-terminated constants.
    let string_len = cstr_len(string);
    let required_space = if flag == ENTER {
        let mut rs = MIN_DIR_ENTRY_SIZE + string_len;
        if rs & 0x03 != 0 {
            rs += DIR_ENTRY_ALIGN as usize - (rs & 0x03);
        }
        rs
    } else {
        0
    };

    // If i_last_dpos optimization applies for ENTER
    if flag == ENTER
        && (*ldir_ptr).i_last_dpos < file_size
        && (*ldir_ptr).i_last_dentry_size <= required_space as i32
    {
        pos = (*ldir_ptr).i_last_dpos;
    }

    let mut prev_dp: *mut Ext2DiskDirDesc = core::ptr::null_mut();

    while pos < file_size {
        let block_pos = pos & !(block_size - 1);
        let b = read_map(ldir_ptr, block_pos, 0);
        if b == NO_BLOCK {
            pos += block_size;
            continue;
        }

        let bp = lmfs_get_block_ino(
            (*ldir_ptr).i_dev,
            b as u64,
            NORMAL,
            (*ldir_ptr).i_num as u64,
            block_pos,
        );
        if bp.is_null() {
            pos += block_size;
            continue;
        }

        let data = b_data(bp);
        let data_end = data.wrapping_add(block_size as usize);
        let mut dp = data as *mut Ext2DiskDirDesc;

        prev_dp = core::ptr::null_mut();

        while (dp as usize) < (data_end as usize) {
            let d_ino = core::ptr::read_unaligned(core::ptr::addr_of!((*dp).d_ino));
            let d_rec_len =
                core::ptr::read_unaligned(core::ptr::addr_of!((*dp).d_rec_len)) as usize;

            if d_rec_len == 0 || (dp as usize) + d_rec_len > (data_end as usize) {
                break;
            }

            let d_name_len =
                core::ptr::read_unaligned(core::ptr::addr_of!((*dp).d_name_len)) as usize;

            // Match occurs if string found
            if flag != ENTER && d_ino != NO_ENTRY {
                if flag == IS_EMPTY {
                    if !(d_name_len == 1
                        && core::ptr::read_unaligned(core::ptr::addr_of!((*dp).d_name[0])) == b'.')
                        && !(d_name_len == 2
                            && core::ptr::read_unaligned((*dp).d_name.as_ptr().add(0)) == b'.'
                            && core::ptr::read_unaligned((*dp).d_name.as_ptr().add(1)) == b'.')
                    {
                        match_found = true;
                    }
                } else {
                    // LOOK_UP or DELETE — match the name as C's ansi_strcmp()
                    // does: same length, then the same bytes. d_name is a
                    // flexible array declared as [u8; 1], so it has to be
                    // walked by pointer: indexing it panics past the first
                    // byte.
                    if d_name_len == string_len {
                        let mut name_match = true;
                        let name = core::ptr::addr_of!((*dp).d_name) as *const u8;
                        for (i, &sc) in string[..string_len].iter().enumerate() {
                            if core::ptr::read_unaligned(name.add(i)) != sc {
                                name_match = false;
                                break;
                            }
                        }
                        if name_match {
                            match_found = true;
                        }
                    }
                }
            }

            if match_found {
                r = OK;
                if flag == IS_EMPTY {
                    r = ENOTEMPTY;
                } else if flag == DELETE {
                    // Erase entry. The inode number is stashed in the tail of
                    // the name first, so an interrupted erase leaves something
                    // to recover the entry from.
                    if d_name_len >= core::mem::size_of::<u32>() {
                        let t = d_name_len - core::mem::size_of::<u32>();
                        let name = core::ptr::addr_of_mut!((*dp).d_name) as *mut u8;
                        core::ptr::write_unaligned(name.add(t) as *mut u32, d_ino);
                    }
                    core::ptr::write_unaligned(core::ptr::addr_of_mut!((*dp).d_ino), NO_ENTRY);
                    lmfs_markdirty(bp);

                    // Reset EXT2_INDEX_FL if not using HTree
                    let sp = (*ldir_ptr).i_sp.as_ref().unwrap();
                    if !has_compat_feature(sp, COMPAT_DIR_INDEX) {
                        (*ldir_ptr).i_flags &= !EXT2_INDEX_FL;
                    }

                    if pos < (*ldir_ptr).i_last_dpos {
                        (*ldir_ptr).i_last_dpos = pos;
                        (*ldir_ptr).i_last_dentry_size = d_rec_len as i32;
                    }
                    (*ldir_ptr).i_update |= CTIME | MTIME;
                    (*ldir_ptr).i_dirt = IN_DIRTY;

                    // Merge with previous entry if not first
                    if !prev_dp.is_null() {
                        let prev_rec_len =
                            core::ptr::read_unaligned(core::ptr::addr_of!((*prev_dp).d_rec_len));
                        let new_rec_len = prev_rec_len + d_rec_len as u16;
                        core::ptr::write_unaligned(
                            core::ptr::addr_of_mut!((*prev_dp).d_rec_len),
                            new_rec_len,
                        );
                    }
                } else if flag == LOOK_UP {
                    if !numb.is_null() {
                        *numb = d_ino;
                    }
                }

                lmfs_put_block(bp, DIRECTORY_BLOCK);
                return r;
            }

            // Check for free slot for ENTER
            if flag == ENTER && d_ino == NO_ENTRY && required_space <= d_rec_len {
                e_hit = true;
                slot_pos = pos + (dp as usize - data as usize) as u64;
                break;
            }

            // Can we shrink dentry for ENTER?
            if flag == ENTER {
                let actual_size = MIN_DIR_ENTRY_SIZE + d_name_len;
                let actual_size_aligned = if actual_size & 0x03 != 0 {
                    (actual_size + DIR_ENTRY_ALIGN as usize - 1) & !(DIR_ENTRY_ALIGN as usize - 1)
                } else {
                    actual_size
                };

                // The new entry has to fit in the slack this one leaves behind
                // (`DIR_ENTRY_SHRINK` in C). Splitting on anything less makes
                // the new slot shorter than its own contents -- a record length
                // of zero, which stops the next search at that offset.
                let new_slot_size = d_rec_len.saturating_sub(actual_size_aligned);
                if new_slot_size < required_space {
                    prev_dp = dp;
                    dp = (dp as *mut u8).wrapping_add(d_rec_len) as *mut Ext2DiskDirDesc;
                    continue;
                }

                core::ptr::write_unaligned(
                    core::ptr::addr_of_mut!((*dp).d_rec_len),
                    actual_size_aligned as u16,
                );

                // Move dp to the new slot
                let next_dp =
                    (dp as *mut u8).wrapping_add(actual_size_aligned) as *mut Ext2DiskDirDesc;
                core::ptr::write_unaligned(
                    core::ptr::addr_of_mut!((*next_dp).d_rec_len),
                    new_slot_size as u16,
                );
                core::ptr::write_unaligned(core::ptr::addr_of_mut!((*next_dp).d_ino), NO_ENTRY);
                lmfs_markdirty(bp);
                e_hit = true;
                slot_pos = pos + (next_dp as usize - data as usize) as u64;
                break;
            }

            prev_dp = dp;
            // Move to next entry
            dp = (dp as *mut u8).wrapping_add(d_rec_len) as *mut Ext2DiskDirDesc;
        }

        if e_hit {
            lmfs_put_block(bp, DIRECTORY_BLOCK);
            break;
        }
        lmfs_put_block(bp, DIRECTORY_BLOCK);
        pos += block_size;
    }

    // End of directory search
    if flag != ENTER {
        return if flag == IS_EMPTY { OK } else { ENOENT };
    }

    // ENTER: update last_dpos
    (*ldir_ptr).i_last_dpos = pos;
    (*ldir_ptr).i_last_dentry_size = required_space as i32;

    // No free slot anywhere: grow the directory by one block and use its first
    // entry. The entry is written below, so the buffer is only initialised here.
    if !e_hit {
        new_slots = 1;
        slot_pos = file_size;
        let bp = new_block(ldir_ptr, file_size);
        if bp.is_null() {
            return (*glo::ext2_ptr()).err_code;
        }
        let dp = b_data(bp) as *mut Ext2DiskDirDesc;
        core::ptr::write_unaligned(core::ptr::addr_of_mut!((*dp).d_rec_len), block_size as u16);
        core::ptr::write_unaligned(
            core::ptr::addr_of_mut!((*dp).d_name_len),
            EXT2_NAME_MAX as u8,
        ); // for failure
        lmfs_markdirty(bp);
        lmfs_put_block(bp, DIRECTORY_BLOCK);
    }

    // Write the entry into the block that holds `slot_pos`.
    let write_block_pos = slot_pos & !(block_size - 1);
    let b = read_map(ldir_ptr, write_block_pos, 0);
    if b == NO_BLOCK {
        return ENOENT;
    }

    let bp_w = lmfs_get_block_ino(
        (*ldir_ptr).i_dev,
        b as u64,
        NORMAL,
        (*ldir_ptr).i_num as u64,
        write_block_pos,
    );
    if bp_w.is_null() {
        return EIO;
    }

    let dp_at = (b_data(bp_w) as *mut u8).add((slot_pos - write_block_pos) as usize)
        as *mut Ext2DiskDirDesc;

    // Write the directory entry.
    core::ptr::write_unaligned(
        core::ptr::addr_of_mut!((*dp_at).d_name_len),
        string_len as u8,
    );
    // d_name is a flexible array declared as [u8; 1] — walk it by pointer.
    let name = core::ptr::addr_of_mut!((*dp_at).d_name) as *mut u8;
    for (i, &sc) in string[..string_len].iter().enumerate() {
        core::ptr::write_unaligned(name.add(i), sc);
    }
    // The on-disk name is length-prefixed, not terminated: only add the NUL
    // when it still lands inside this entry's slot.
    let slot_rec_len = core::ptr::read_unaligned(core::ptr::addr_of!((*dp_at).d_rec_len)) as usize;
    if MIN_DIR_ENTRY_SIZE + string_len + 1 <= slot_rec_len {
        core::ptr::write_unaligned(name.add(string_len), 0);
    }
    core::ptr::write_unaligned(
        core::ptr::addr_of_mut!((*dp_at).d_ino),
        if !numb.is_null() { *numb } else { 0 },
    );

    // File type
    if let Some(sp) = (*ldir_ptr).i_sp.as_ref() {
        if has_incompat_feature(sp, INCOMPAT_FILETYPE) {
            let file_type = match (ftype as u16) & I_TYPE {
                t if t == I_REGULAR => EXT2_FT_REG_FILE,
                t if t == I_DIRECTORY => EXT2_FT_DIR,
                t if t == I_SYMBOLIC_LINK => EXT2_FT_SYMLINK,
                t if t == I_BLOCK_SPECIAL => EXT2_FT_BLKDEV,
                t if t == I_CHAR_SPECIAL => EXT2_FT_CHRDEV,
                t if t == I_NAMED_PIPE => EXT2_FT_FIFO,
                _ => EXT2_FT_UNKNOWN,
            };
            core::ptr::write_unaligned(core::ptr::addr_of_mut!((*dp_at).d_file_type), file_type);
        }
    }

    lmfs_markdirty(bp_w);
    lmfs_put_block(bp_w, DIRECTORY_BLOCK);
    (*ldir_ptr).i_update |= CTIME | MTIME;
    (*ldir_ptr).i_dirt = IN_DIRTY;

    if new_slots == 1 {
        (*ldir_ptr).i_size += block_size as u32;
        rw_inode(ldir_ptr, WRITING);
    }

    OK
}
