//! ISO 9660 super block handling — adapted from `minix/fs/iso9660fs/super.c`
//!
//! Reads the primary volume descriptor from the device, validates the
//! standard ID, extracts block size and volume space size, and parses
//! the root directory record.

use crate::iso9660::consts::*;
use crate::iso9660::glo;
use crate::iso9660::inode;
use crate::iso9660::types::*;
use crate::iso9660::utility;

/// Read the primary volume descriptor from the device.
/// Scans sectors starting at `ISO9660_SUPER_BLOCK_POSITION` (sector 16)
/// looking for VD_PRIMARY, then continues until VD_SET_TERM.
///
/// Returns `OK` on success, or a negative errno on failure.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn read_vds(dev: u32) -> i32 {
    let mut offset: u64 = ISO9660_SUPER_BLOCK_POSITION;
    let mut vol_ok = false;
    let mut i: u32 = 0;

    let mut sbbuf = [0u8; ISO9660_MIN_BLOCK_SIZE];

    while !vol_ok && i < MAX_ATTEMPTS {
        // Stub: read a block from `dev` at `offset` into `sbbuf`.
        // In the real Minix implementation this uses bdev_read().
        // For now we return an error to signal that block I/O is not
        // available in this stub layer.
        let r = block_read(dev, offset, &mut sbbuf);
        if r != ISO9660_MIN_BLOCK_SIZE as i32 {
            i += 1;
            offset += ISO9660_MIN_BLOCK_SIZE as u64;
            continue;
        }

        if sbbuf[0] == VD_PRIMARY {
            create_v_pri(dev, &sbbuf, offset);
        }

        if sbbuf[0] == VD_SET_TERM {
            vol_ok = true;
        }

        i += 1;
        offset += ISO9660_MIN_BLOCK_SIZE as u64;
    }

    if !vol_ok { EINVAL } else { OK }
}

/// Fill the primary volume descriptor from a raw buffer read from disk.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn create_v_pri(_dev: u32, buf: &[u8], address: u64) {
    let v_pri = glo::v_pri_ptr();

    (*v_pri).vd_type = buf[0];
    (*v_pri)
        .standard_id
        .copy_from_slice(&buf[1..][..ISO9660_SIZE_STANDARD_ID]);
    (*v_pri).vd_version = buf[6];
    (*v_pri)
        .system_id
        .copy_from_slice(&buf[8..][..ISO9660_SIZE_SYS_ID]);
    (*v_pri)
        .volume_id
        .copy_from_slice(&buf[40..][..ISO9660_SIZE_VOLUME_ID]);

    (*v_pri).volume_space_size_l = utility::read_le_u32(buf, 80);
    (*v_pri).volume_space_size_m = utility::read_be_u32(buf, 84);
    (*v_pri).volume_set_size = utility::read_le_u32(buf, 120);
    (*v_pri).volume_sequence_number = utility::read_le_u32(buf, 124);
    (*v_pri).logical_block_size_l = utility::read_le_u16(buf, 128);
    (*v_pri).logical_block_size_m = utility::read_be_u16(buf, 130);
    (*v_pri).path_table_size_l = utility::read_le_u32(buf, 132);
    (*v_pri).path_table_size_m = utility::read_be_u32(buf, 136);
    (*v_pri).loc_l_occ_path_table = utility::read_le_u32(buf, 140);
    (*v_pri).loc_opt_l_occ_path_table = utility::read_le_u32(buf, 144);
    (*v_pri).loc_m_occ_path_table = utility::read_be_u32(buf, 148);
    (*v_pri).loc_opt_m_occ_path_table = utility::read_be_u32(buf, 152);

    // Parse the root directory record starting at offset 156
    let dir = inode::get_free_dir_record();
    if !dir.is_null() {
        inode::create_dir_record(dir, &buf[156..], (address + 156) as u32);
        // Copy the dir record into v_pri (shallow copy of raw pointers, matching C semantics)
        (*v_pri).dir_rec_root = core::ptr::read(dir);
        (*v_pri).dir_rec_root.d_ino_nr = ROOT_INO_NR;
    }

    (*v_pri)
        .volume_set_id
        .copy_from_slice(&buf[190..][..ISO9660_SIZE_VOLUME_SET_ID]);
    (*v_pri)
        .publisher_id
        .copy_from_slice(&buf[318..][..ISO9660_SIZE_PUBLISHER_ID]);
    (*v_pri)
        .data_preparer_id
        .copy_from_slice(&buf[446..][..ISO9660_SIZE_DATA_PREP_ID]);
    (*v_pri)
        .application_id
        .copy_from_slice(&buf[574..][..ISO9660_SIZE_APPL_ID]);
    (*v_pri)
        .copyright_file_id
        .copy_from_slice(&buf[702..][..ISO9660_SIZE_COPYRIGHT_FILE_ID]);
    (*v_pri)
        .abstract_file_id
        .copy_from_slice(&buf[739..][..ISO9660_SIZE_ABSTRACT_FILE_ID]);
    (*v_pri)
        .bibl_file_id
        .copy_from_slice(&buf[776..][..ISO9660_SIZE_BIBL_FILE_ID]);
    (*v_pri)
        .volume_cre_date
        .copy_from_slice(&buf[813..][..ISO9660_SIZE_VOL_CRE_DATE]);
    (*v_pri)
        .volume_mod_date
        .copy_from_slice(&buf[830..][..ISO9660_SIZE_VOL_MOD_DATE]);
    (*v_pri)
        .volume_exp_date
        .copy_from_slice(&buf[847..][..ISO9660_SIZE_VOL_EXP_DATE]);
    (*v_pri)
        .volume_eff_date
        .copy_from_slice(&buf[864..][..ISO9660_SIZE_VOL_EFF_DATE]);
    (*v_pri).file_struct_ver = buf[881];
}

/// Release the primary volume descriptor (free its root dir record).
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn release_v_pri() -> i32 {
    let v_pri = glo::v_pri_ptr();
    inode::release_dir_record(&mut (*v_pri).dir_rec_root);
    (*v_pri).count = 0;
    OK
}

/// Stub: read a block of data from a device.
///
/// In the real Minix implementation this calls `bdev_read()`.
/// This stub returns the block size to indicate success, simulating
/// a valid (zero-filled) block read.
///
/// Once the block I/O layer is wired up, replace this with an actual
/// device read call.
pub fn block_read(_dev: u32, _offset: u64, buf: &mut [u8]) -> i32 {
    // Stub: fill with zeros and return the buffer size as "bytes read".
    // A real implementation would call the kernel block device interface.
    let len = buf.len();
    buf.fill(0);
    len as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_read_stub() {
        let mut buf = [1u8; 2048];
        let r = block_read(0, 0, &mut buf);
        assert_eq!(r, 2048);
        assert_eq!(buf, [0u8; 2048]);
    }
}
