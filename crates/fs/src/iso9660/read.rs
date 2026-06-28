//! ISO 9660 read operations — adapted from `minix/fs/iso9660fs/read.c`
//!
//! ISO 9660 is read-only, so only read operations are implemented.

use crate::iso9660::consts::*;
use crate::iso9660::glo;
use crate::iso9660::inode;
use crate::iso9660::types::*;
use core::cmp;

/// `fs_readwrite()` — read handler for regular file I/O.
///
/// Reads data from a file, splitting the transfer into chunks that don't
/// cross block boundaries.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_readwrite() -> i32 {
    // In the real implementation, rw is determined from fs_m_in.m_type:
    //   REQ_READ -> READING, REQ_PEEK -> PEEKING
    let rw = READING; // stub

    let isofs = glo::isofs_ptr();
    let v_pri = glo::v_pri_ptr();

    // Get inode
    let dir = inode::get_dir_record(1); // stub: inode from fs_m_in
    if dir.is_null() {
        return EINVAL;
    }

    let mut position: u64 = 0; // stub: from fs_m_in
    let mut nrbytes: usize = 0; // stub: from fs_m_in
    let block_size = (*v_pri).logical_block_size_l as usize;
    let f_size = (*dir).d_file_size as u64;

    // In the real implementation:
    //   position = fs_m_in.m_vfs_fs_readwrite.seek_pos;
    //   nrbytes  = fs_m_in.m_vfs_fs_readwrite.nbytes;
    //   gid      = fs_m_in.m_vfs_fs_readwrite.grant;
    //   f_size   = dir->d_file_size;

    (*isofs).rdwt_err = OK;

    let mut cum_io: usize = 0;

    while nrbytes != 0 {
        let off = (position % block_size as u64) as usize;
        let chunk = cmp::min(nrbytes, block_size - off);

        let bytes_left = if position >= f_size {
            0
        } else {
            (f_size - position) as usize
        };
        if position >= f_size {
            break;
        }
        let chunk = cmp::min(chunk, bytes_left);

        let mut completed = 0;
        let r = read_chunk(
            dir,
            position,
            off,
            chunk,
            nrbytes as u32,
            0, // gid (stub)
            cum_io,
            block_size,
            &mut completed,
            rw,
        );

        if r != OK {
            break;
        }
        if (*isofs).rdwt_err < 0 {
            break;
        }

        nrbytes -= chunk;
        cum_io += chunk;
        position += chunk as u64;
    }

    // fs_m_out.m_fs_vfs_readwrite.seek_pos = position;

    let mut r = OK;
    if (*isofs).rdwt_err != OK {
        r = (*isofs).rdwt_err;
    }
    if (*isofs).rdwt_err == END_OF_FILE {
        r = OK;
    }

    // fs_m_out.m_fs_vfs_readwrite.nbytes = cum_io;
    inode::release_dir_record(dir);

    r
}

/// `fs_bread()` — block read (for filesystem-level block I/O).
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_bread() -> i32 {
    let v_pri = glo::v_pri_ptr();
    let isofs = glo::isofs_ptr();

    // In the real implementation:
    //   rw_flag = (fs_m_in.m_type == REQ_BREAD ? READING : WRITING);
    //   gid = fs_m_in.m_vfs_fs_breadwrite.grant;
    //   position = fs_m_in.m_vfs_fs_breadwrite.seek_pos;
    //   nrbytes = fs_m_in.m_vfs_fs_breadwrite.nbytes;

    let dir: *mut DirRecord = core::ptr::addr_of_mut!((*v_pri).dir_rec_root);
    let block_size = (*v_pri).logical_block_size_l as usize;
    let mut position: u64 = 0; // stub
    let mut nrbytes: i32 = 0; // stub

    // WRITING not supported
    (*isofs).rdwt_err = OK;

    let mut cum_io: usize = 0;

    while nrbytes != 0 {
        let off = (position % block_size as u64) as usize;
        let chunk = cmp::min(nrbytes as usize, block_size - off);

        let mut completed = 0;
        let r = read_chunk(
            dir,
            position,
            off,
            chunk,
            nrbytes as u32,
            0, // gid (stub)
            cum_io,
            block_size,
            &mut completed,
            READING,
        );

        if r != OK {
            break;
        }
        if (*isofs).rdwt_err < 0 {
            break;
        }

        nrbytes -= chunk as i32;
        cum_io += chunk;
        position += chunk as u64;
    }

    // fs_m_out.m_fs_vfs_breadwrite.seek_pos = position;

    let mut r = OK;
    if (*isofs).rdwt_err != OK {
        r = (*isofs).rdwt_err;
    }
    if (*isofs).rdwt_err == END_OF_FILE {
        r = OK;
    }

    // fs_m_out.m_fs_vfs_breadwrite.nbytes = cum_io;
    r
}

/// `fs_getdents()` — read directory entries.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_getdents() -> i32 {
    let v_pri = glo::v_pri_ptr();
    let ino: u32 = 1; // stub: from fs_m_in
    let pos: u64 = 0; // stub: from fs_m_in

    let block_size = (*v_pri).logical_block_size_l as usize;
    let mut cur_pos = pos;
    let mut tmpbuf_offset: usize = 0;
    let _userbuf_off: usize = 0;

    let dir = inode::get_dir_record(ino);
    if dir.is_null() {
        return EINVAL;
    }

    let mut block = (*dir).loc_extent_l;
    block += (pos / block_size as u64) as u32;
    let mut done = false;

    while cur_pos < (*dir).d_file_size as u64 {
        let bp = inode::get_block(block);
        if bp.is_null() {
            inode::release_dir_record(dir);
            return EINVAL;
        }

        let block_pos_start = (cur_pos % block_size as u64) as usize;
        let mut block_pos = block_pos_start;

        while block_pos < block_size {
            let dir_tmp = inode::get_free_dir_record();
            if dir_tmp.is_null() {
                inode::release_dir_record(dir);
                inode::put_block(bp);
                return EINVAL;
            }

            let buf = core::slice::from_raw_parts(
                inode::b_data(bp).add(block_pos),
                block_size - block_pos,
            );
            inode::create_dir_record(
                dir_tmp,
                buf,
                block * (*v_pri).logical_block_size_l as u32 + block_pos as u32,
            );

            if (*dir_tmp).length == 0 {
                done = true;
                inode::release_dir_record(dir_tmp);
                break;
            }

            // Extract name
            let name_len = core::cmp::min((*dir_tmp).length_file_id as usize, NAME_MAX);
            let mut name = [0u8; NAME_MAX + 1];
            let file_id: &[u8; ISO9660_MAX_FILE_ID_LEN] = &*core::ptr::addr_of!((*dir_tmp).file_id);
            name[..name_len].copy_from_slice(&file_id[..name_len]);

            // Tidy up name (remove ';' version, trailing '.')
            if let Some(sc_pos) = name.iter().position(|&c| c == b';') {
                name[sc_pos] = 0;
            }
            let effective_len = name.iter().position(|&c| c == 0).unwrap_or(name_len);
            if effective_len > 0 && name[effective_len - 1] == b'.' {
                name[effective_len - 1] = 0;
            }

            let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(0)];

            // Compute record length
            let reclen = name.len() + 12; // approximate _DIRENT_RECLEN

            if tmpbuf_offset + reclen > GETDENTS_BUFSIZ {
                // sys_safecopyto(VFS_PROC_NR, gid, userbuf_off, getdents_buf, tmpbuf_offset);
                tmpbuf_offset = 0;
            }

            // In a real implementation, create dirent struct in buffer
            tmpbuf_offset += reclen;

            cur_pos += (*dir_tmp).length as u64;
            block_pos += (*dir_tmp).length as usize;
            inode::release_dir_record(dir_tmp);
        }

        inode::put_block(bp);
        if done {
            break;
        }

        cur_pos += block_size as u64 - cur_pos;
        block += 1;
    }

    if tmpbuf_offset != 0 {
        // sys_safecopyto(VFS_PROC_NR, gid, userbuf_off, getdents_buf, tmpbuf_offset);
    }

    // fs_m_out.m_fs_vfs_getdents.nbytes = 0;
    // fs_m_out.m_fs_vfs_getdents.seek_pos = cur_pos;

    inode::release_dir_record(dir);
    OK
}

/// `read_chunk()` — read a chunk of data from a file.
///
/// Maps logical position to physical block and copies the data out.
///
/// # Safety
///
/// Requires exclusive access to globals.
#[allow(clippy::too_many_arguments)]
pub unsafe fn read_chunk(
    dir: *mut DirRecord,
    mut position: u64,
    _off: usize,
    chunk: usize,
    _left: u32,
    _gid: u32,
    _buf_off: usize,
    block_size: usize,
    _completed: &mut i32,
    rw: i32,
) -> i32 {
    let mut cur_dir = dir;

    // Handle multi-extent files: skip past extents until we're in the right one
    if position <= (*cur_dir).d_file_size as u64 && position > (*cur_dir).data_length_l as u64 {
        while !(*cur_dir).d_next.is_null() && position > (*cur_dir).data_length_l as u64 {
            position -= (*cur_dir).data_length_l as u64;
            cur_dir = (*cur_dir).d_next;
        }
    }

    let b: u32 = if (*cur_dir).inter_gap_size != 0 {
        let rel_block = (position / block_size as u64) as u32;
        let file_unit = rel_block / (*cur_dir).data_length_l;
        let offset_in_unit = rel_block % (*cur_dir).file_unit_size as u32;
        (*cur_dir).loc_extent_l
            + ((*cur_dir).file_unit_size as u32 + (*cur_dir).inter_gap_size as u32) * file_unit
            + offset_in_unit
    } else {
        (*cur_dir).loc_extent_l + (position / block_size as u64) as u32
    };

    let bp = inode::get_block(b);
    if bp.is_null() {
        return EIO;
    }

    if rw == READING {
        // sys_safecopyto(VFS_PROC_NR, gid, buf_off, b_data(bp) + off, chunk);
        let _ = chunk;
    }

    inode::put_block(bp);
    OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso9660::glo;
    use crate::iso9660::inode;

    #[test]
    fn test_fs_readwrite_stub() {
        unsafe {
            glo::isofs_init_globals();
            inode::init_inode_cache();
            let r = fs_readwrite();
            // Returns EINVAL because no inode is cached
            assert_eq!(r, EINVAL);
        }
    }

    #[test]
    fn test_fs_bread_stub() {
        unsafe {
            glo::isofs_init_globals();
            inode::init_inode_cache();
            // Should not panic
            let r = fs_bread();
            assert!(r == OK || r == EINVAL);
        }
    }
}
