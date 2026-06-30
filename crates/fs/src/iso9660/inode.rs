//! ISO 9660 inode/dir-record cache — adapted from `minix/fs/iso9660fs/inode.c`
//!
//! Manages a cache of directory records (`DirRecord`) and extended attribute
//! records (`ExtAttrRec`). These are stored in global arrays indexed by
//! physical address and looked up by inode number (which equals the physical
//! address).

use libs::libminixfs::cache::{lmfs_get_block, lmfs_nr_bufs, lmfs_put_block};
use libs::libminixfs::constants::FULL_DATA_BLOCK;
use libs::libminixfs::types::Buf;

use crate::iso9660::consts::*;
use crate::iso9660::glo;
use crate::iso9660::types::*;
use crate::iso9660::utility;

/// Helper to get the raw data pointer from a Buf.
///
/// # Safety
///
/// `bp` must point to a valid, initialized `Buf` obtained from `lmfs_get_block`.
pub unsafe fn b_data(bp: *mut Buf) -> *mut u8 {
    (*bp).data_ptr
}

/// Load a directory record from disk at the given physical address.
/// Initialize the inode cache (dir records and ext attr records).
/// Sets all entries' d_count / count to 0.
///
/// # Safety
///
/// Must be called exactly once at startup.
pub unsafe fn init_inode_cache() {
    let dirs = glo::dir_records_ptr();
    for i in 0..NR_DIR_RECORDS {
        (*dirs.add(i)).d_count = 0;
    }
    let eas = glo::ext_attr_recs_ptr();
    for i in 0..NR_ATTR_RECS {
        (*eas.add(i)).count = 0;
    }
}

/// Find a directory record by physical address (used as inode number).
/// Searches the cache first; if not found, loads it from disk.
///
/// Returns a pointer to the `DirRecord` with an incremented reference count,
/// or `null` if not found.
///
/// # Safety
///
/// Caller must ensure exclusive access to globals.
pub unsafe fn get_dir_record(id_dir_record: u32) -> *mut DirRecord {
    let dirs = glo::dir_records_ptr();
    let mut dir: *mut DirRecord = core::ptr::null_mut();

    // Search through the cache
    for i in 0..NR_DIR_RECORDS {
        let dr = &*dirs.add(i);
        if dr.d_ino_nr == id_dir_record && dr.d_count > 0 {
            dir = dirs.add(i);
            (*dir).d_count += 1;
            break;
        }
    }

    if dir.is_null() {
        dir = load_dir_record_from_disk(id_dir_record);
    }

    dir
}

/// `fs_putnode()` — VFS putnode handler.
///
/// Finds the inode specified by the request message and decreases its
/// reference count.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_putnode() -> i32 {
    // In the real implementation:
    //   inode = fs_m_in.m_vfs_fs_putnode.inode;
    //   count = fs_m_in.m_vfs_fs_putnode.count;
    let ino: u32 = 1; // stub
    let count: i32 = 1; // stub

    let dir = get_dir_record(ino);
    if dir.is_null() {
        return EINVAL;
    }

    if count <= 0 {
        return EINVAL;
    }

    if count as u8 > (*dir).d_count {
        return EINVAL;
    }

    if (*dir).d_count > 1 {
        // Keep at least one reference
        (*dir).d_count = (*dir).d_count - count as u8 + 1;
    }

    release_dir_record(dir);

    OK
}

/// `fs_getnode()` — VFS getnode handler (stub).
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_getnode() -> i32 {
    // In the real implementation, look up or create a dir record by inode.
    OK
}

/// Release a directory record (decrement its reference count).
/// If the count reaches 0, also releases associated extended attributes
/// and linked file sections (d_next).
///
/// # Safety
///
/// Caller must ensure that `dr` is a valid pointer to a global dir record.
pub unsafe fn release_dir_record(dr: *mut DirRecord) -> i32 {
    if dr.is_null() {
        return EINVAL;
    }

    (*dr).d_count = (*dr).d_count.saturating_sub(1);
    if (*dr).d_count == 0 {
        // Release extended attributes
        if !(*dr).ext_attr.is_null() {
            (*(*dr).ext_attr).count = 0;
        }
        (*dr).ext_attr = core::ptr::null_mut();
        (*dr).d_mountpoint = false;
        (*dr).d_prior = core::ptr::null_mut();

        // Recursively release next sections
        if !(*dr).d_next.is_null() {
            release_dir_record((*dr).d_next);
        }
        (*dr).d_next = core::ptr::null_mut();
    }
    OK
}

/// Get a free directory record from the cache.
/// Returns a pointer with count set to 1, or null if all are in use.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn get_free_dir_record() -> *mut DirRecord {
    let dirs = glo::dir_records_ptr();
    for i in 0..NR_DIR_RECORDS {
        let dr = &mut *dirs.add(i);
        if dr.d_count == 0 {
            dr.d_count = 1;
            dr.ext_attr = core::ptr::null_mut();
            return dirs.add(i);
        }
    }
    core::ptr::null_mut()
}

/// Get a free extended attribute record from the cache.
/// Returns a pointer with count set to 1, or null if all are in use.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn get_free_ext_attr() -> *mut ExtAttrRec {
    let eas = glo::ext_attr_recs_ptr();
    for i in 0..NR_ATTR_RECS {
        let ea = &mut *eas.add(i);
        if ea.count == 0 {
            ea.count = 1;
            return eas.add(i);
        }
    }
    core::ptr::null_mut()
}

/// Fill an `ExtAttrRec` from raw device data.
///
/// # Safety
///
/// `ext` must be a valid pointer to an `ExtAttrRec`.
pub unsafe fn create_ext_attr(ext: *mut ExtAttrRec, buf: &[u8]) -> i32 {
    if ext.is_null() {
        return EINVAL;
    }

    (*ext).own_id = utility::read_le_u32(buf, 0);
    (*ext).group_id = utility::read_le_u32(buf, 4);
    (*ext).permissions = utility::read_le_u16(buf, 8);
    (*ext)
        .file_cre_date
        .copy_from_slice(&buf[10..][..ISO9660_SIZE_VOL_CRE_DATE]);
    (*ext)
        .file_mod_date
        .copy_from_slice(&buf[27..][..ISO9660_SIZE_VOL_MOD_DATE]);
    (*ext)
        .file_exp_date
        .copy_from_slice(&buf[44..][..ISO9660_SIZE_VOL_EXP_DATE]);
    (*ext)
        .file_eff_date
        .copy_from_slice(&buf[61..][..ISO9660_SIZE_VOL_EFF_DATE]);
    (*ext).rec_format = buf[78];
    (*ext).rec_attrs = buf[79];
    (*ext).rec_length = utility::read_le_u32(buf, 80);
    (*ext)
        .system_id
        .copy_from_slice(&buf[84..][..ISO9660_SIZE_SYS_ID]);
    (*ext)
        .system_use
        .copy_from_slice(&buf[116..][..ISO9660_SIZE_SYSTEM_USE]);
    (*ext).ext_attr_rec_ver = buf[180];
    (*ext).len_esc_seq = buf[181];

    OK
}

/// Fill a `DirRecord` from raw device data.
///
/// # Safety
///
/// `dir` must be a valid pointer to a `DirRecord`.
pub unsafe fn create_dir_record(dir: *mut DirRecord, buf: &[u8], address: u32) -> i32 {
    if dir.is_null() || buf.is_empty() {
        return EINVAL;
    }

    let size = buf[0];
    (*dir).length = size;
    (*dir).ext_attr_rec_length = buf[1];
    (*dir).loc_extent_l = utility::read_le_u32(buf, 2);
    (*dir).loc_extent_m = utility::read_be_u32(buf, 6);
    (*dir).data_length_l = utility::read_le_u32(buf, 10);
    (*dir).data_length_m = utility::read_be_u32(buf, 14);
    (*dir).rec_date.copy_from_slice(&buf[18..25]);
    (*dir).file_flags = buf[25];
    (*dir).file_unit_size = buf[26];
    (*dir).inter_gap_size = buf[27];
    (*dir).vol_seq_number = utility::read_le_u16(buf, 28);
    (*dir).length_file_id = buf[32];

    let name_len = core::cmp::min((*dir).length_file_id as usize, ISO9660_MAX_FILE_ID_LEN);
    let name_end = 33 + name_len;
    if name_end <= buf.len() {
        // SAFETY: raw pointer deref, then explicitly borrow the field slice
        let file_id: &mut [u8; ISO9660_MAX_FILE_ID_LEN] =
            &mut *core::ptr::addr_of_mut!((*dir).file_id);
        file_id[..name_len].copy_from_slice(&buf[33..name_end]);
    }
    if name_len < ISO9660_MAX_FILE_ID_LEN {
        let file_id: &mut [u8; ISO9660_MAX_FILE_ID_LEN] =
            &mut *core::ptr::addr_of_mut!((*dir).file_id);
        file_id[name_len..].fill(0);
    }

    (*dir).ext_attr = core::ptr::null_mut();

    // Set memory attrs
    if ((*dir).file_flags & D_TYPE) == D_DIRECTORY {
        (*dir).d_mode = I_DIRECTORY;
    } else {
        (*dir).d_mode = I_REGULAR;
    }

    // Read-only permissions for all
    (*dir).d_mode |= R_BIT | X_BIT;
    (*dir).d_mode |= (R_BIT | X_BIT) << 3;
    (*dir).d_mode |= (R_BIT | X_BIT) << 6;

    (*dir).d_mountpoint = false;
    (*dir).d_next = core::ptr::null_mut();
    (*dir).d_prior = core::ptr::null_mut();
    (*dir).d_file_size = (*dir).data_length_l;

    // Physical address = inode number
    (*dir).d_phy_addr = address;
    (*dir).d_ino_nr = address;

    OK
}

/// Load a directory record from disk at the given physical address.
/// Also loads additional file sections (multi-extent files).
///
/// # Safety
///
/// Requires exclusive access to globals. Returns null on failure.
pub unsafe fn load_dir_record_from_disk(address: u32) -> *mut DirRecord {
    let block_size: u32 = (*glo::v_pri_ptr()).logical_block_size_l as u32;
    if block_size == 0 {
        return core::ptr::null_mut();
    }
    let block_nr = address / block_size;
    let offset = (address % block_size) as usize;

    // Use libminixfs block cache
    if lmfs_nr_bufs() == 0 {
        return core::ptr::null_mut();
    }
    let bp = lmfs_get_block((*glo::isofs_ptr()).fs_dev, block_nr as u64);
    if bp.is_null() {
        return core::ptr::null_mut();
    }
    let bp_data = core::slice::from_raw_parts((*bp).data_ptr as *const u8, block_size as usize);

    let dir = get_free_dir_record();
    if dir.is_null() {
        return core::ptr::null_mut();
    }

    create_dir_record(dir, bp_data, address);

    // Load additional file sections if present (multi-extent)
    let mut new_pos = offset + (*dir).length as usize;
    let mut dir_parent = dir;
    let mut new_address = address + (*dir).length as u32;

    while new_pos < block_size as usize {
        let dir_next = get_free_dir_record();
        if dir_next.is_null() {
            break;
        }

        create_dir_record(dir_next, &bp_data[new_pos..], new_address);

        if (*dir_next).length > 0 {
            let name = core::slice::from_raw_parts(
                (*dir_next).file_id.as_ptr(),
                (*dir_next).length_file_id as usize,
            );
            let old_name = core::slice::from_raw_parts(
                (*dir_parent).file_id.as_ptr(),
                (*dir_parent).length_file_id as usize,
            );

            if name == old_name {
                (*dir_parent).d_next = dir_next;
                (*dir_next).d_prior = dir_parent;

                // Update file sizes
                let mut dir_tmp = dir_next;
                let mut size = (*dir_tmp).data_length_l;
                while !(*dir_tmp).d_prior.is_null() {
                    dir_tmp = (*dir_tmp).d_prior;
                    size += (*dir_tmp).data_length_l;
                    (*dir_tmp).d_file_size = size;
                }

                new_pos += (*dir_parent).length as usize;
                new_address += (*dir_next).length as u32;
                dir_parent = dir_next;
            } else {
                release_dir_record(dir_next);
                break;
            }
        } else {
            release_dir_record(dir_next);
            break;
        }
    }

    lmfs_put_block(bp, FULL_DATA_BLOCK);
    dir
}

// ── Block I/O wrappers ──

/// Get a block from the libminixfs block cache.
///
/// # Safety
///
/// Requires exclusive access to the ISO 9660 global state for `fs_dev`.
/// Returns null on cache miss or error.
pub unsafe fn get_block(block_nr: u32) -> *mut Buf {
    if lmfs_nr_bufs() == 0 {
        return core::ptr::null_mut();
    }
    lmfs_get_block((*glo::isofs_ptr()).fs_dev, block_nr as u64)
}

/// Release a block back to the block cache.
///
/// # Safety
///
/// `bp` must be a valid pointer from `get_block` or `lmfs_get_block`.
pub unsafe fn put_block(bp: *mut Buf) {
    lmfs_put_block(bp, FULL_DATA_BLOCK);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso9660::glo;

    #[test]
    fn test_init_inode_cache() {
        // Safety: single-threaded test
        unsafe {
            glo::isofs_init_globals();
            init_inode_cache();
            let dirs = glo::dir_records_ptr();
            for i in 0..NR_DIR_RECORDS.min(10) {
                assert_eq!((*dirs.add(i)).d_count, 0);
            }
        }
    }

    #[test]
    fn test_get_free_dir_record() {
        unsafe {
            glo::isofs_init_globals();
            init_inode_cache();
            let dr = get_free_dir_record();
            assert!(!dr.is_null());
            assert_eq!((*dr).d_count, 1);
            assert!((*dr).ext_attr.is_null());
        }
    }

    #[test]
    fn test_get_free_ext_attr() {
        unsafe {
            glo::isofs_init_globals();
            init_inode_cache();
            let ea = get_free_ext_attr();
            assert!(!ea.is_null());
            assert_eq!((*ea).count, 1);
        }
    }

    #[test]
    fn test_create_dir_record() {
        unsafe {
            glo::isofs_init_globals();
            init_inode_cache();
            let dr = get_free_dir_record();
            let mut buf = [0u8; 64];
            buf[0] = 34; // length
            buf[1] = 0; // ext_attr_rec_length
            buf[2..6].copy_from_slice(&10u32.to_le_bytes()); // loc_extent_l
            buf[6..10].copy_from_slice(&10u32.to_be_bytes()); // loc_extent_m
            buf[10..14].copy_from_slice(&100u32.to_le_bytes()); // data_length_l
            buf[14..18].copy_from_slice(&100u32.to_be_bytes()); // data_length_m
            buf[25] = 0; // file_flags (regular)
            buf[32] = 4; // length_file_id
            buf[33..37].copy_from_slice(b"TEST");

            let r = create_dir_record(dr, &buf, 12345);
            assert_eq!(r, OK);
            assert_eq!((*dr).length, 34);
            assert_eq!((*dr).loc_extent_l, 10);
            assert_eq!((*dr).data_length_l, 100);
            assert_eq!((*dr).d_file_size, 100);
            assert_eq!((*dr).d_phy_addr, 12345);
            assert_eq!((*dr).d_ino_nr, 12345);
            assert!((*dr).d_mode & I_REGULAR != 0);
        }
    }

    #[test]
    fn test_release_dir_record_null() {
        unsafe {
            let r = release_dir_record(core::ptr::null_mut());
            assert_eq!(r, EINVAL);
        }
    }
}
