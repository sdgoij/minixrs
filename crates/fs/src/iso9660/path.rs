//! ISO 9660 path lookup — adapted from `minix/fs/iso9660fs/path.c`

use crate::iso9660::consts::*;
use crate::iso9660::glo;
use crate::iso9660::inode;
use crate::iso9660::types::*;

/// `fs_lookup()` — VFS lookup handler.
/// Looks up a pathname and returns the resulting inode properties.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_lookup() -> i32 {
    // In the real implementation these come from fs_m_in:
    //   grant     = fs_m_in.m_vfs_fs_lookup.grant_path;
    //   len       = fs_m_in.m_vfs_fs_lookup.path_len;
    //   dir_ino   = fs_m_in.m_vfs_fs_lookup.dir_ino;
    //   root_ino  = fs_m_in.m_vfs_fs_lookup.root_ino;
    //   flags     = fs_m_in.m_vfs_fs_lookup.flags;
    //   caller_uid = fs_m_in.m_vfs_fs_lookup.uid;
    //   caller_gid = fs_m_in.m_vfs_fs_lookup.gid;
    let len: usize = 0; // stub
    let dir_ino: u32 = ROOT_INO_NR; // stub
    let root_ino: u32 = ROOT_INO_NR; // stub
    let flags: i32 = 0; // stub

    if len > PATH_MAX {
        return E2BIG;
    }
    if len < 1 {
        return EINVAL;
    }

    // sys_safecopyfrom(VFS_PROC_NR, grant, 0, user_path, len);
    // if user_path[len-1] != '\0' return EINVAL;

    let mut dir: *mut DirRecord = core::ptr::null_mut();
    let mut offset: usize = 0;
    let r = parse_path(dir_ino, root_ino, flags, &mut dir, &mut offset);

    if r == ELEAVEMOUNT {
        // fs_m_out.m_fs_vfs_lookup.offset = offset;
        // fs_m_out.m_fs_vfs_lookup.symloop = 0;
        return r;
    }

    if r != OK && r != EENTERMOUNT {
        return r;
    }

    // fs_m_out.m_fs_vfs_lookup.inode    = ID_DIR_RECORD(dir);
    // fs_m_out.m_fs_vfs_lookup.mode     = dir->d_mode;
    // fs_m_out.m_fs_vfs_lookup.file_size = dir->d_file_size;
    // fs_m_out.m_fs_vfs_lookup.symloop  = 0;
    // fs_m_out.m_fs_vfs_lookup.uid      = SYS_UID;
    // fs_m_out.m_fs_vfs_lookup.gid      = SYS_GID;

    if r == EENTERMOUNT {
        // fs_m_out.m_fs_vfs_lookup.offset = offset;
        inode::release_dir_record(dir);
    }

    r
}

/// Parse a path string into a directory record.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn parse_path(
    dir_ino: u32,
    _root_ino: u32,
    _flags: i32,
    res_inop: &mut *mut DirRecord,
    offsetp: &mut usize,
) -> i32 {
    let isofs = glo::isofs_ptr();

    let start_dir = inode::get_dir_record(dir_ino);
    if start_dir.is_null() {
        return ENOENT;
    }

    let cp: *const u8 = core::ptr::addr_of!((*isofs).user_path[0]);
    let mut cp_pos: usize = 0;
    let mut cur_dir = start_dir;

    loop {
        let cur_byte = *cp.add(cp_pos);
        if cur_byte == 0 {
            // Empty path — return current directory
            *res_inop = cur_dir;
            *offsetp += cp_pos;

            if (*cur_dir).d_mountpoint {
                return EENTERMOUNT;
            }
            return OK;
        }

        let mut string = [0u8; NAME_MAX + 1];
        let ncp_pos: usize;

        if cur_byte == b'/' {
            // Skip leading slashes
            while *cp.add(cp_pos) == b'/' {
                cp_pos += 1;
            }
            if *cp.add(cp_pos) == 0 {
                // Only slashes — look up "."
                string[..1].copy_from_slice(b".");
                ncp_pos = cp_pos;
            } else {
                ncp_pos = get_name(cp, cp_pos, &mut string);
            }
        } else {
            ncp_pos = get_name(cp, cp_pos, &mut string);
        }

        let string_len = string.iter().position(|&c| c == 0).unwrap_or(NAME_MAX);
        let string_slice = &string[..string_len];

        // Handle ".."
        if string_slice == b".." {
            let dir_recs_ptr = glo::dir_records_ptr();
            if cur_dir == dir_recs_ptr {
                // Climbing up mountpoint
                inode::release_dir_record(cur_dir);
                *res_inop = core::ptr::null_mut();
                *offsetp += cp_pos;
                return ELEAVEMOUNT;
            }
        } else if (*cur_dir).d_mountpoint {
            *res_inop = cur_dir;
            *offsetp += cp_pos;
            return EENTERMOUNT;
        }

        let old_dir = cur_dir;
        let r = advance(old_dir, &string, &mut cur_dir);

        if r != OK {
            inode::release_dir_record(old_dir);
            return r;
        }

        inode::release_dir_record(old_dir);
        cp_pos = ncp_pos;
    }
}

/// Advance to the next path component.
///
/// Given a directory and a component name, look up the component and
/// return the resulting directory record.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn advance(dirp: *mut DirRecord, string: &[u8], resp: &mut *mut DirRecord) -> i32 {
    if string.is_empty() || string[0] == 0 {
        return ENOENT;
    }

    if dirp.is_null() {
        return EINVAL;
    }

    let mut numb: u32 = 0;
    let r = search_dir(dirp, string, &mut numb);
    if r != OK {
        return r;
    }

    let rip = inode::get_dir_record(numb);
    if rip.is_null() {
        return EINVAL;
    }

    *resp = rip;
    OK
}

/// Search a directory for a component name.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn search_dir(ldir_ptr: *mut DirRecord, string: &[u8], numb: &mut u32) -> i32 {
    let v_pri = glo::v_pri_ptr();

    if ((*ldir_ptr).d_mode & I_TYPE) != I_DIRECTORY {
        return ENOTDIR;
    }

    // Handle "."
    if string == b"." {
        *numb = id_dir_record(&*ldir_ptr);
        return OK;
    }

    // Handle ".." when ldir is root
    if string == b".." && (*ldir_ptr).loc_extent_l == (*v_pri).dir_rec_root.loc_extent_l {
        *numb = ROOT_INO_NR;
        return OK;
    }

    // Read the directory content
    let pos_start = (*ldir_ptr).ext_attr_rec_length as usize;
    let block_size = (*v_pri).logical_block_size_l as usize;

    // Read the block
    let bp = inode::get_block((*ldir_ptr).loc_extent_l);
    if bp.is_null() {
        return EINVAL;
    }

    let mut pos = pos_start;
    while pos < block_size {
        let dir_tmp = inode::get_free_dir_record();
        if dir_tmp.is_null() {
            inode::put_block(bp);
            return EINVAL;
        }

        let buf = core::slice::from_raw_parts(inode::b_data(bp).add(pos), block_size - pos);
        if inode::create_dir_record(
            dir_tmp,
            buf,
            (*ldir_ptr).loc_extent_l * (*v_pri).logical_block_size_l as u32 + pos as u32,
        ) != OK
        {
            inode::put_block(bp);
            return EINVAL;
        }

        if (*dir_tmp).length == 0 {
            inode::release_dir_record(dir_tmp);
            inode::put_block(bp);
            return EINVAL;
        }

        // Extract the filename from file_id
        let mut tmp_string = [0u8; NAME_MAX + 1];
        let name_len = core::cmp::min((*dir_tmp).length_file_id as usize, NAME_MAX);
        let file_id: &[u8; ISO9660_MAX_FILE_ID_LEN] = &*core::ptr::addr_of!((*dir_tmp).file_id);
        tmp_string[..name_len].copy_from_slice(&file_id[..name_len]);

        // Remove ';' version suffix
        if let Some(sc_pos) = tmp_string[..name_len].iter().position(|&c| c == b';') {
            tmp_string[sc_pos] = 0;
        }

        // Remove trailing '.' if no extension
        let effective_len = tmp_string.iter().position(|&c| c == 0).unwrap_or(name_len);
        if effective_len > 0 && tmp_string[effective_len - 1] == b'.' {
            tmp_string[effective_len - 1] = 0;
        }

        // Compare names
        let tmp_slice = &tmp_string[..tmp_string.iter().position(|&c| c == 0).unwrap_or(0)];

        let matched = tmp_slice == string || ((*dir_tmp).file_id[0] == 1 && string == b"..");

        if matched {
            // Check if this is the root
            if (*dir_tmp).loc_extent_l == (*glo::dir_records_ptr()).loc_extent_l {
                *numb = 1;
            } else {
                // Load extended attributes if present
                if (*dir_tmp).ext_attr_rec_length != 0 {
                    (*dir_tmp).ext_attr = inode::get_free_ext_attr();
                    if !(*dir_tmp).ext_attr.is_null() {
                        inode::create_ext_attr(
                            (*dir_tmp).ext_attr,
                            core::slice::from_raw_parts(inode::b_data(bp), block_size),
                        );
                    }
                }
                *numb = id_dir_record(&*dir_tmp);
            }

            inode::release_dir_record(dir_tmp);
            inode::put_block(bp);
            return OK;
        }

        pos += (*dir_tmp).length as usize;
        inode::release_dir_record(dir_tmp);
    }

    inode::put_block(bp);
    EINVAL
}

/// Extract the first component of a path name into `string`.
/// Returns the position after the extracted component.
fn get_name(path_name: *const u8, start: usize, string: &mut [u8]) -> usize {
    let mut cp = start;

    // Skip leading slashes
    while unsafe { *path_name.add(cp) } == b'/' {
        cp += 1;
    }

    let comp_start = cp;

    // Find the end of the first component
    while unsafe { *path_name.add(cp) } != 0 && unsafe { *path_name.add(cp) } != b'/' {
        cp += 1;
    }

    let len = cp - comp_start;
    let copy_len = if len > NAME_MAX { NAME_MAX } else { len };

    if copy_len == 0 {
        string[..1].copy_from_slice(b".");
    } else {
        for (i, byte) in string.iter_mut().enumerate().take(copy_len) {
            *byte = unsafe { *path_name.add(comp_start + i) };
        }
        string[copy_len] = 0;
    }

    cp
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_get_name_basic() {
        let path = b"/usr/bin/test\0";
        let mut string = [0u8; NAME_MAX + 1];
        let end = get_name(path.as_ptr(), 0, &mut string);
        assert_eq!(&string[..3], b"usr");
        assert!(end > 0);
    }

    #[test]
    fn test_get_name_slash_only() {
        let path = b"//\0";
        let mut string = [0u8; NAME_MAX + 1];
        let _end = get_name(path.as_ptr(), 0, &mut string);
        // Should return "."
        assert_eq!(string[0], b'.');
    }

    #[test]
    fn test_advance_no_inode() {
        unsafe {
            let r = advance(core::ptr::null_mut(), b"test", &mut core::ptr::null_mut());
            assert_eq!(r, EINVAL);
        }
    }
}
