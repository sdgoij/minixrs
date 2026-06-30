//! Path lookup and directory operations — adapted from `minix/fs/mfs/path.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::protect::{forbidden, read_only};
use crate::mfs::read::read_map;
use crate::mfs::types::*;
use crate::mfs::utility::conv4;
use libs::libminixfs::cache::{lmfs_dev, lmfs_get_block, lmfs_markdirty, lmfs_put_block};

/// Compare a name from a directory entry (null-terminated byte array)
/// against a byte slice, matching C `strncmp` semantics.
fn dir_name_match(name: &[u8; MFS_NAME_MAX], string: &[u8]) -> bool {
    for (i, &nc) in name.iter().enumerate() {
        let sc = string.get(i).copied().unwrap_or(0);
        if sc == 0 && nc == 0 {
            break;
        }
        if sc != nc {
            return false;
        }
    }
    true
}

/// Check if a directory entry name is "."
fn is_name_dot(name: &[u8; MFS_NAME_MAX]) -> bool {
    name[0] == b'.' && name[1] == 0
}

/// Check if a directory entry name is ".."
fn is_name_dotdot(name: &[u8; MFS_NAME_MAX]) -> bool {
    name[0] == b'.' && name[1] == b'.' && name[2] == 0
}

pub fn fs_lookup() -> i32 {
    unsafe {
        let mfs = glo::mfs_ptr();

        let dir_ino = (*mfs).lookup_dir_ino;
        let root_ino = (*mfs).lookup_root_ino;
        let flags = (*mfs).lookup_flags;
        let path_len = (*mfs).lookup_path_len;

        // Check length.
        if path_len > PATH_MAX {
            return E2BIG;
        }
        if path_len == 0 {
            return EINVAL;
        }

        // Verify null-terminated path.
        if (*mfs).user_path[path_len - 1] != 0 {
            return EINVAL;
        }

        // caller_uid and caller_gid are already set in globals.

        // ── Parse the path (inline parse_path logic) ──
        let mut cp_offset: usize = 0;
        let mut symlinks: i32 = 0;
        let mut offset: usize = 0;

        // Find starting inode.
        let rip = match find_inode((*mfs).fs_dev, dir_ino) {
            Some(i) => i,
            None => return ENOENT,
        };

        // If dir has been removed return ENOENT.
        if (*glo::get_inode_ptr(rip as usize)).i_nlinks == NO_LINK {
            return ENOENT;
        }

        dup_inode(rip);

        // If the given start inode is a mountpoint, only accept "..".
        let mut leaving_mount = (*glo::get_inode_ptr(rip as usize)).i_mountpoint != FALSE;

        let mut current_rip = rip;

        loop {
            // Skip leading slashes.
            while cp_offset < path_len && (*mfs).user_path[cp_offset] == b'/' {
                cp_offset += 1;
            }

            // If end of path (or empty), we're done.
            if cp_offset >= path_len || (*mfs).user_path[cp_offset] == 0 {
                if (*glo::get_inode_ptr(current_rip as usize)).i_mountpoint != FALSE {
                    (*mfs).lookup_res_offset = offset + cp_offset;
                    return EENTERMOUNT;
                }
                // Fill in the result inode info.
                let res = &*glo::get_inode_ptr(current_rip as usize);
                (*mfs).lookup_res_inode = (*res).i_num;
                (*mfs).lookup_res_mode = (*res).i_mode;
                (*mfs).lookup_res_file_size = (*res).i_size;
                (*mfs).lookup_res_symloop = symlinks;
                (*mfs).lookup_res_uid = (*res).i_uid;
                (*mfs).lookup_res_gid = (*res).i_gid;
                (*mfs).lookup_res_device = (*res).i_zone[0];

                return OK;
            }

            // Extract the next component.
            let comp_start = cp_offset;
            while cp_offset < path_len
                && (*mfs).user_path[cp_offset] != b'/'
                && (*mfs).user_path[cp_offset] != 0
            {
                cp_offset += 1;
            }
            let component = &(&(*mfs).user_path)[comp_start..cp_offset];
            let component_len = component.len().min(MFS_NAME_MAX);
            let cmp = &component[..component_len];

            // Handle ".."
            if component_len >= 2
                && component[0] == b'.'
                && component[1] == b'.'
                && (component_len == 2 || component[2] == 0)
            {
                let r = forbidden(current_rip, X_BIT);
                if r != OK {
                    put_inode(Some(current_rip));
                    return r;
                }

                let rip_inode = &*glo::get_inode_ptr(current_rip as usize);
                if (*rip_inode).i_num == root_ino {
                    // Ignore '..' at process root; continue.
                    offset += cp_offset;
                    continue;
                }

                if (*rip_inode).i_num == ROOT_INODE {
                    let is_root = (*rip_inode)
                        .i_sp
                        .as_ref()
                        .map_or(false, |sp| sp.s_is_root != 0);
                    if !is_root {
                        // Climbing up to parent FS.
                        put_inode(Some(current_rip));
                        (*mfs).lookup_res_offset = offset + cp_offset;
                        return ELEAVEMOUNT;
                    }
                }
            }

            // Only check for a mount point if not coming from one.
            if !leaving_mount {
                let rip_inode = &*glo::get_inode_ptr(current_rip as usize);
                if (*rip_inode).i_mountpoint != FALSE {
                    (*mfs).lookup_res_offset = offset + cp_offset;
                    let res = &*glo::get_inode_ptr(current_rip as usize);
                    (*mfs).lookup_res_inode = (*res).i_num;
                    (*mfs).lookup_res_mode = (*res).i_mode;
                    (*mfs).lookup_res_file_size = (*res).i_size;
                    (*mfs).lookup_res_symloop = symlinks;
                    (*mfs).lookup_res_uid = (*res).i_uid;
                    (*mfs).lookup_res_gid = (*res).i_gid;
                    (*mfs).lookup_res_device = (*res).i_zone[0];
                    return EENTERMOUNT;
                }
            }

            // Advance through this component.
            let dir_ip = current_rip;
            let advance_name = if leaving_mount { &DOT2[..2] } else { cmp };
            let next_rip = advance(dir_ip, advance_name, CHK_PERM);

            if next_rip.is_none() {
                put_inode(Some(dir_ip));
                return (*glo::mfs_ptr()).err_code;
            }
            current_rip = next_rip.unwrap();
            leaving_mount = false;

            // Handle symlinks.
            let rip_inode = &*glo::get_inode_ptr(current_rip as usize);
            if (*rip_inode).i_mode & I_TYPE == I_SYMBOLIC_LINK {
                // Check if we should return the symlink itself.
                let next_char = if cp_offset < path_len {
                    (*mfs).user_path[cp_offset]
                } else {
                    0
                };
                if next_char == 0 && (flags & PATH_RET_SYMLINK) != 0 {
                    put_inode(Some(dir_ip));
                    let res = &*glo::get_inode_ptr(current_rip as usize);
                    (*mfs).lookup_res_offset = offset + cp_offset;
                    (*mfs).lookup_res_inode = (*res).i_num;
                    (*mfs).lookup_res_mode = (*res).i_mode;
                    (*mfs).lookup_res_file_size = (*res).i_size;
                    (*mfs).lookup_res_symloop = symlinks;
                    (*mfs).lookup_res_uid = (*res).i_uid;
                    (*mfs).lookup_res_gid = (*res).i_gid;
                    (*mfs).lookup_res_device = (*res).i_zone[0];
                    return OK;
                }

                // Traverse symlink.
                let r = ltraverse(current_rip, cp_offset, path_len);
                cp_offset = 0;
                offset = 0;
                symlinks += 1;

                if symlinks > _POSIX_SYMLOOP_MAX as i32 {
                    put_inode(Some(dir_ip));
                    put_inode(Some(current_rip));
                    return ELOOP;
                }

                if r != OK {
                    put_inode(Some(dir_ip));
                    put_inode(Some(current_rip));
                    return r;
                }

                // If new path starts with '/', tell VFS via ESYMLINK.
                if cp_offset < path_len && (*mfs).user_path[cp_offset] == b'/' {
                    put_inode(Some(dir_ip));
                    put_inode(Some(current_rip));
                    return ESYMLINK;
                }

                put_inode(Some(current_rip));
                dup_inode(dir_ip);
                current_rip = dir_ip;
            }

            put_inode(Some(dir_ip));
            offset += cp_offset;
        }
    }
}

/// Traverse a symbolic link: copy link text into user_path, shifting suffix.
///
/// # Safety
///
/// Accesses global state.
unsafe fn ltraverse(rip_idx: u16, suffix_offset: usize, path_len: usize) -> i32 {
    let rip = &*glo::get_inode_ptr(rip_idx as usize);
    let llen = (*rip).i_size as usize;

    // Get the block containing the symlink data.
    let b = read_map(rip_idx, 0, 0);
    if b == NO_BLOCK {
        return EIO;
    }
    let bp = lmfs_get_block((*rip).i_dev, b as u64);
    if bp.is_null() {
        return EIO;
    }

    let sp = (*bp).data_ptr; // start of link text
    let mfs = &mut *glo::mfs_ptr();
    let user_path = &mut (*mfs).user_path;

    // Length of the remaining path after the symlink component.
    let slen = path_len.saturating_sub(suffix_offset);

    if slen > 0 {
        // There is path after the link. Make room for the expanded link.
        if slen + llen + 1 > PATH_MAX {
            lmfs_put_block(bp, DIRECTORY_BLOCK);
            return ENAMETOOLONG;
        }
        // Move suffix left or right to position llen.
        let src_ptr = user_path.as_ptr().add(suffix_offset);
        let dst_ptr = user_path.as_mut_ptr().add(llen);
        core::ptr::copy(src_ptr, dst_ptr, slen + 1);
    } else {
        if llen + 1 > PATH_MAX {
            lmfs_put_block(bp, DIRECTORY_BLOCK);
            return ENAMETOOLONG;
        }
        // Set terminating null.
        user_path[llen] = 0;
    }

    // Copy the expanded link to user_path.
    core::ptr::copy_nonoverlapping(sp, user_path.as_mut_ptr(), llen);

    lmfs_put_block(bp, DIRECTORY_BLOCK);
    OK
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
    string: &[u8],
    numb: Option<&mut u32>,
    flag: i32,
    check_permissions: i32,
) -> i32 {
    unsafe {
        let ldir = &*glo::get_inode_ptr(ldir_idx as usize);

        // If 'ldir_ptr' is not a pointer to a dir inode, error.
        if ((*ldir).i_mode & I_TYPE) != I_DIRECTORY {
            return ENOTDIR;
        }

        if (flag == DELETE || flag == ENTER)
            && (*ldir).i_sp.as_ref().map_or(false, |sp| sp.s_rd_only != 0)
        {
            return EROFS;
        }

        let mut r = OK;

        if flag != IS_EMPTY {
            let bits: u16 = if flag == LOOK_UP {
                X_BIT
            } else {
                W_BIT | X_BIT
            };

            // Check if string is dot1 (".") or dot2 ("..")
            let is_dot = !string.is_empty()
                && string[0] == b'.'
                && (string.len() == 1
                    || string[1] == 0
                    || (string.len() >= 2
                        && string[1] == b'.'
                        && (string.len() == 2 || string[2] == 0)));

            if is_dot {
                if flag != LOOK_UP {
                    r = read_only(ldir_idx);
                }
            } else if check_permissions != 0 {
                r = forbidden(ldir_idx, bits);
            }
        }
        if r != OK {
            return r;
        }

        let sp = match (*ldir).i_sp.as_ref() {
            Some(sp) => sp,
            None => return EINVAL,
        };
        let block_size = sp.s_block_size as usize;

        let old_slots = (*ldir).i_size as usize / DIR_ENTRY_SIZE;
        let mut new_slots = 0usize;
        let mut e_hit = false;
        let mut match_found = false;

        let mut pos: i64 = 0;
        if flag == ENTER && (*ldir).i_last_dpos < (*ldir).i_size as i64 {
            pos = (*ldir).i_last_dpos;
            new_slots = pos as usize / DIR_ENTRY_SIZE;
        }

        while (pos as i64) < (*ldir).i_size as i64 {
            // Directories don't have holes, so b cannot be NO_BLOCK.
            let b = read_map(ldir_idx, pos as i64, 0);
            debug_assert!((*ldir).i_dev != NO_DEV);
            debug_assert!(b != NO_BLOCK);

            let bp = lmfs_get_block((*ldir).i_dev, b as u64);
            debug_assert!(!bp.is_null());
            debug_assert!(lmfs_dev(bp) != NO_DEV);

            let dp_base = (*bp).data_ptr as *const Direct;
            let num_entries = nr_dir_entries(block_size);

            let mut block_e_hit = false;

            for i in 0..num_entries {
                if new_slots + 1 > old_slots {
                    if flag == ENTER {
                        block_e_hit = true;
                    }
                    break;
                }
                new_slots += 1;

                let dp = &*dp_base.add(i);

                // Check for a match.
                if flag != ENTER && dp.mfs_d_ino != NO_ENTRY {
                    if flag == IS_EMPTY {
                        // Check if not "." and not ".."
                        if !is_name_dot(&dp.mfs_d_name) && !is_name_dotdot(&dp.mfs_d_name) {
                            match_found = true;
                        }
                    } else {
                        // LOOK_UP or DELETE: compare string with dp.mfs_d_name
                        if dir_name_match(&dp.mfs_d_name, string) {
                            match_found = true;
                        }
                    }
                }

                if match_found {
                    r = OK;
                    if flag == IS_EMPTY {
                        r = ENOTEMPTY;
                    } else if flag == DELETE {
                        // Save d_ino for recovery, then erase the entry.
                        let dp_mut = &mut *((*bp).data_ptr as *mut Direct).add(i);
                        let t = MFS_NAME_MAX - core::mem::size_of::<u32>();
                        let ino_bytes = dp.mfs_d_ino.to_ne_bytes();
                        for (j, &b) in ino_bytes.iter().enumerate() {
                            if t + j < MFS_NAME_MAX {
                                dp_mut.mfs_d_name[t + j] = b;
                            }
                        }
                        dp_mut.mfs_d_ino = NO_ENTRY;
                        lmfs_markdirty(bp);
                        let ldir_mut = &mut *glo::get_inode_ptr(ldir_idx as usize);
                        (*ldir_mut).i_update |= CTIME | MTIME;
                        (*ldir_mut).i_dirt = IN_DIRTY;
                        if (pos as i64) < (*ldir_mut).i_last_dpos {
                            (*ldir_mut).i_last_dpos = pos;
                        }
                    } else {
                        // flag is LOOK_UP
                        if let Some(numb_ref) = numb {
                            *numb_ref = conv4(sp.s_native as i32, dp.mfs_d_ino as i64) as u32;
                        }
                    }
                    debug_assert!(lmfs_dev(bp) != NO_DEV);
                    lmfs_put_block(bp, DIRECTORY_BLOCK);
                    return r;
                }

                // Check for free slot for the benefit of ENTER.
                if flag == ENTER && dp.mfs_d_ino == 0 {
                    block_e_hit = true;
                    break;
                }
            }

            // The whole block has been searched or ENTER has a free slot.
            if block_e_hit {
                e_hit = true;
            }
            debug_assert!(lmfs_dev(bp) != NO_DEV);
            lmfs_put_block(bp, DIRECTORY_BLOCK);
            if e_hit {
                break;
            }
            pos += block_size as i64;
        }

        // The whole directory has now been searched.
        if flag != ENTER {
            return if flag == IS_EMPTY { OK } else { ENOENT };
        }

        // ── ENTER path ──
        // When ENTER next time, start searching for free slot from i_last_dpos.
        let ldir_mut = &mut *glo::get_inode_ptr(ldir_idx as usize);
        (*ldir_mut).i_last_dpos = pos;

        if !e_hit {
            todo!("search_dir: ENTER — directory full, new_block not yet wired; see NEXT.md");
        }

        // 'bp' now points to a directory block with space. 'dp' points to slot.
        // For now, ENTER is not fully wired.
        todo!("search_dir: ENTER not yet wired; depends on new_block and buffer cache");
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

    #[test]
    fn test_search_dir_lookup_empty_directory() {
        // An inode with I_DIRECTORY mode but no entries returns ENOENT.
        init();
        unsafe {
            let rip = &mut *crate::mfs::glo::get_inode_ptr(0);
            rip.i_mode = I_DIRECTORY;
            // With i_size == 0, the directory is empty.
            rip.i_size = 0;
            // Set up a super block so i_sp is not None.
            let sp = crate::mfs::glo::get_super_ptr(0);
            (*sp).s_block_size = 1024;
            (*sp).s_native = 1;
            rip.i_sp = Some(&mut *sp);
        }
        assert_eq!(
            search_dir(0, b"nonexistent", None, LOOK_UP, IGN_PERM),
            ENOENT
        );
    }

    #[test]
    fn test_fs_lookup_empty_path_returns_einval() {
        // fs_lookup with path_len == 0 returns EINVAL.
        init();
        unsafe {
            let mfs = &mut *crate::mfs::glo::mfs_ptr();
            (*mfs).lookup_path_len = 0;
        }
        assert_eq!(fs_lookup(), EINVAL);
    }

    #[test]
    fn test_fs_lookup_no_null_terminator_returns_einval() {
        // fs_lookup with no null terminator in user_path returns EINVAL.
        init();
        unsafe {
            let mfs = &mut *crate::mfs::glo::mfs_ptr();
            (*mfs).lookup_path_len = 5;
            (*mfs).user_path[..5].copy_from_slice(b"hello");
            // No null terminator at position path_len - 1
        }
        assert_eq!(fs_lookup(), EINVAL);
    }

    #[test]
    fn test_fs_lookup_path_too_long_returns_e2big() {
        init();
        unsafe {
            let mfs = &mut *crate::mfs::glo::mfs_ptr();
            (*mfs).lookup_path_len = PATH_MAX + 1;
        }
        assert_eq!(fs_lookup(), E2BIG);
    }

    #[test]
    fn test_search_dir_is_empty_returns_ok_on_empty_dir() {
        // IS_EMPTY on a directory with no entries returns OK.
        init();
        unsafe {
            let rip = &mut *crate::mfs::glo::get_inode_ptr(0);
            rip.i_mode = I_DIRECTORY;
            rip.i_size = 0;
            // Set up a super block so i_sp is not None.
            let sp = crate::mfs::glo::get_super_ptr(0);
            (*sp).s_block_size = 1024;
            (*sp).s_native = 1;
            rip.i_sp = Some(&mut *sp);
        }
        assert_eq!(search_dir(0, b"", None, IS_EMPTY, IGN_PERM), OK);
    }

    #[test]
    fn test_dir_name_match_identical() {
        let mut name = [0u8; MFS_NAME_MAX];
        name[..3].copy_from_slice(b"abc");
        assert!(dir_name_match(&name, b"abc"));
    }

    #[test]
    fn test_dir_name_match_mismatch() {
        let mut name = [0u8; MFS_NAME_MAX];
        name[..4].copy_from_slice(b"abcd");
        assert!(!dir_name_match(&name, b"abc"));
    }

    #[test]
    fn test_dir_name_match_string_longer() {
        let mut name = [0u8; MFS_NAME_MAX];
        name[..3].copy_from_slice(b"abc");
        assert!(!dir_name_match(&name, b"abcd"));
    }

    #[test]
    fn test_is_name_dot() {
        let mut name = [0u8; MFS_NAME_MAX];
        name[0] = b'.';
        assert!(is_name_dot(&name));
        assert!(!is_name_dotdot(&name));
    }

    #[test]
    fn test_is_name_dotdot() {
        let mut name = [0u8; MFS_NAME_MAX];
        name[0] = b'.';
        name[1] = b'.';
        assert!(is_name_dotdot(&name));
        assert!(!is_name_dot(&name));
    }

    #[test]
    fn test_fs_lookup_panics() {
        // fs_lookup no longer panics; returns EINVAL for empty path.
        init();
        unsafe {
            let mfs = &mut *crate::mfs::glo::mfs_ptr();
            (*mfs).lookup_path_len = 0;
        }
        assert_eq!(fs_lookup(), EINVAL);
    }

    #[test]
    fn test_search_dir_panics() {
        // search_dir no longer panics; returns ENOTDIR for non-directory.
        init();
        assert_eq!(search_dir(0, b"name", None, LOOK_UP, IGN_PERM), ENOTDIR);
    }
}
