//! Path lookup and directory search — adapted from `minix/fs/ext2/path.c`

use libs::libminixfs::cache::{lmfs_get_block_ino, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::{
    DIRECTORY_BLOCK, FULL_DATA_BLOCK, NO_READ, NORMAL, VMC_NO_INODE,
};

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

/// fs_lookup — VFS lookup handler.
pub unsafe fn fs_lookup() -> i32 {
    let ext2 = glo::ext2_ptr();

    // FIXME: parse dir_ino, name, flags from message
    let dir_ino = (*ext2).fs_m_in_type as u32;

    if dir_ino == 0 {
        (*ext2).err_code = ENOENT;
        return ENOENT;
    }

    let rip = get_inode((*ext2).fs_dev, dir_ino);
    if rip.is_null() {
        (*ext2).err_code = ENOENT;
        return ENOENT;
    }

    // FIXME: parse path from grant and user_path
    // For now just return what we have
    let _ = rip;

    // Reply would be set in fs_m_out
    OK
}

/// Advance to the next path component.
pub unsafe fn advance(dirp: *mut Inode, string: &[u8], chk_perm: i32) -> *mut Inode {
    if dirp.is_null() {
        return core::ptr::null_mut();
    }

    if ((*dirp).i_mode & I_TYPE) != I_DIRECTORY {
        return core::ptr::null_mut();
    }

    let mut numb = 0u32;
    let r = search_dir(dirp, string, &mut numb as *mut u32, LOOK_UP, chk_perm, 0);
    if r != OK {
        return core::ptr::null_mut();
    }

    if numb == 0 {
        return core::ptr::null_mut();
    }

    let rip = get_inode((*dirp).i_dev, numb);
    rip
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
    let mut match_found = false;
    let mut pos: u64 = 0;

    // For ENTER, compute required space
    let string_len = string.len();
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

        while (dp as usize) < (data_end as usize) || {
            // Check that dp doesn't wrap/walk off
            let d_rec_len = core::ptr::read_unaligned(core::ptr::addr_of!((*dp).d_rec_len));
            (dp as usize) < (data_end as usize)
        } {
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
                    // LOOK_UP or DELETE — match name
                    if d_name_len == string.len() {
                        let mut name_match = true;
                        for i in 0..string.len() {
                            if core::ptr::read_unaligned(&(*dp).d_name[i]) != string[i] {
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
                    // Erase entry
                    if d_name_len >= core::mem::size_of::<u32>() {
                        // We're just clearing d_ino
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
            if flag == ENTER && d_ino == NO_ENTRY {
                if required_space <= d_rec_len {
                    e_hit = true;
                    break;
                }
            }

            // Can we shrink dentry for ENTER?
            if flag == ENTER && required_space + MIN_DIR_ENTRY_SIZE <= d_rec_len {
                // Split the existing entry
                let actual_size = MIN_DIR_ENTRY_SIZE + d_name_len;
                let actual_size_aligned = if actual_size & 0x03 != 0 {
                    (actual_size + DIR_ENTRY_ALIGN as usize - 1) & !(DIR_ENTRY_ALIGN as usize - 1)
                } else {
                    actual_size
                };

                let new_slot_size = d_rec_len - actual_size_aligned;
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
                dp = next_dp;
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

    // If no free slot, extend directory
    if !e_hit {
        new_slots = 1;
        let bp = new_block(ldir_ptr, file_size);
        if bp.is_null() {
            return (*glo::ext2_ptr()).err_code;
        }
        // Initialize the new block
        let data = b_data(bp);
        let dp = data as *mut Ext2DiskDirDesc;
        core::ptr::write_unaligned(core::ptr::addr_of_mut!((*dp).d_rec_len), block_size as u16);
        core::ptr::write_unaligned(
            core::ptr::addr_of_mut!((*dp).d_name_len),
            EXT2_NAME_MAX as u8,
        ); // for failure
        lmfs_markdirty(bp);
        lmfs_put_block(bp, DIRECTORY_BLOCK);
        // Get it back for writing the entry
        let bp2 = lmfs_get_block_ino(
            (*ldir_ptr).i_dev,
            read_map(ldir_ptr, file_size, 0) as u64,
            NORMAL,
            (*ldir_ptr).i_num as u64,
            file_size,
        );
        let dp2 = b_data(bp2) as *mut Ext2DiskDirDesc;
        core::ptr::write_unaligned(core::ptr::addr_of_mut!((*dp2).d_rec_len), block_size as u16);
        core::ptr::write_unaligned(
            core::ptr::addr_of_mut!((*dp2).d_name_len),
            EXT2_NAME_MAX as u8,
        );
        let _ = dp2;
        lmfs_put_block(bp2, DIRECTORY_BLOCK);
    }

    // Now we need to write the entry. Get the block again.
    let block_size_u = block_size as usize;
    let mut write_pos = (*ldir_ptr).i_last_dpos as u64;
    if write_pos >= file_size {
        write_pos = 0;
        while write_pos < file_size {
            write_pos += block_size;
        }
        // write_pos now points to the last block
        write_pos -= block_size;
    }

    let write_block_pos = write_pos & !(block_size - 1);
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

    let data_w = b_data(bp_w);
    let dp_w = data_w as *mut Ext2DiskDirDesc;

    // Find the right position
    let off_in_block = (write_pos - write_block_pos) as usize;
    let dp_at = (dp_w as *mut u8).add(off_in_block) as *mut Ext2DiskDirDesc;

    // Write the directory entry
    core::ptr::write_unaligned(
        core::ptr::addr_of_mut!((*dp_at).d_name_len),
        string_len as u8,
    );
    for i in 0..string.len() {
        core::ptr::write_unaligned(core::ptr::addr_of_mut!((*dp_at).d_name[i]), string[i]);
    }
    if string.len() < EXT2_NAME_MAX {
        core::ptr::write_unaligned(core::ptr::addr_of_mut!((*dp_at).d_name[string.len()]), 0);
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
