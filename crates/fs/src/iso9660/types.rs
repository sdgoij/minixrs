//! ISO 9660 core types — adapted from `minix/fs/iso9660fs/inode.h` and `super.h`

use crate::iso9660::consts::*;

/// ISO 9660 directory record (in-memory + on-disk fields).
///
/// The on-disk portion (length through file_id) is laid out as defined
/// by the ISO 9660 standard. The memory-only fields (d_count, d_mode,
/// etc.) follow.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct DirRecord {
    /// Length of this directory record.
    pub length: u8,
    /// Length of extended attribute record.
    pub ext_attr_rec_length: u8,
    /// Location of extent (little-endian).
    pub loc_extent_l: u32,
    /// Location of extent (big-endian).
    pub loc_extent_m: u32,
    /// Data length (little-endian).
    pub data_length_l: u32,
    /// Data length (big-endian).
    pub data_length_m: u32,
    /// Recording date (7 bytes: year-1900, month, day, hour, min, sec, gmt_offset).
    pub rec_date: [u8; 7],
    /// File flags.
    pub file_flags: u8,
    /// File unit size for interleave mode.
    pub file_unit_size: u8,
    /// Interleave gap size.
    pub inter_gap_size: u8,
    /// Volume sequence number.
    pub vol_seq_number: u16,
    /// Length of file identifier.
    pub length_file_id: u8,
    /// File identifier (padded to max length).
    pub file_id: [u8; ISO9660_MAX_FILE_ID_LEN],

    /// Pointer to extended attributes (in-memory only).
    pub ext_attr: *mut ExtAttrRec,

    /// Reference count.
    pub d_count: u8,
    /// File mode (type + permissions).
    pub d_mode: u16,
    /// Physical address of this record on disk.
    pub d_phy_addr: u32,
    /// Inode number (set to address).
    pub d_ino_nr: u32,
    /// Whether this is a mount point.
    pub d_mountpoint: bool,
    /// Next file section (for multi-extent files).
    pub d_next: *mut DirRecord,
    /// Prior file section / parent.
    pub d_prior: *mut DirRecord,
    /// Total file size across all extents.
    pub d_file_size: u32,
}

/// ISO 9660 extended attribute record.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct ExtAttrRec {
    /// Owner identifier.
    pub own_id: u32,
    /// Group identifier.
    pub group_id: u32,
    /// POSIX permissions.
    pub permissions: u16,
    /// File creation date.
    pub file_cre_date: [u8; ISO9660_SIZE_VOL_CRE_DATE],
    /// File modification date.
    pub file_mod_date: [u8; ISO9660_SIZE_VOL_MOD_DATE],
    /// File expiration date.
    pub file_exp_date: [u8; ISO9660_SIZE_VOL_EXP_DATE],
    /// File effective date.
    pub file_eff_date: [u8; ISO9660_SIZE_VOL_EFF_DATE],
    /// Record format.
    pub rec_format: u8,
    /// Record attributes.
    pub rec_attrs: u8,
    /// Record length.
    pub rec_length: u32,
    /// System identifier.
    pub system_id: [u8; ISO9660_SIZE_SYS_ID],
    /// System use data.
    pub system_use: [u8; ISO9660_SIZE_SYSTEM_USE],
    /// Extended attribute record version.
    pub ext_attr_rec_ver: u8,
    /// Length of escape sequences.
    pub len_esc_seq: u8,
    /// Reference count (in-memory only).
    pub count: i32,
}

/// ISO 9660 Primary Volume Descriptor.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct Iso9660VdPri {
    /// Volume descriptor type.
    pub vd_type: u8,
    /// Standard identifier ("CD001").
    pub standard_id: [u8; ISO9660_SIZE_STANDARD_ID],
    /// Volume descriptor version.
    pub vd_version: u8,
    /// System identifier.
    pub system_id: [u8; ISO9660_SIZE_SYS_ID],
    /// Volume identifier.
    pub volume_id: [u8; ISO9660_SIZE_VOLUME_ID],
    /// Volume space size (little-endian).
    pub volume_space_size_l: u32,
    /// Volume space size (big-endian).
    pub volume_space_size_m: u32,
    /// Volume set size.
    pub volume_set_size: u32,
    /// Volume sequence number.
    pub volume_sequence_number: u32,
    /// Logical block size (little-endian).
    pub logical_block_size_l: u16,
    /// Logical block size (big-endian).
    pub logical_block_size_m: u16,
    /// Path table size (little-endian).
    pub path_table_size_l: u32,
    /// Path table size (big-endian).
    pub path_table_size_m: u32,
    /// Location of L-occ path table.
    pub loc_l_occ_path_table: u32,
    /// Location of optional L-occ path table.
    pub loc_opt_l_occ_path_table: u32,
    /// Location of M-occ path table.
    pub loc_m_occ_path_table: u32,
    /// Location of optional M-occ path table.
    pub loc_opt_m_occ_path_table: u32,
    /// Root directory record (embedded, not pointer).
    pub dir_rec_root: DirRecord,
    /// Volume set identifier.
    pub volume_set_id: [u8; ISO9660_SIZE_VOLUME_SET_ID],
    /// Publisher identifier.
    pub publisher_id: [u8; ISO9660_SIZE_PUBLISHER_ID],
    /// Data preparer identifier.
    pub data_preparer_id: [u8; ISO9660_SIZE_DATA_PREP_ID],
    /// Application identifier.
    pub application_id: [u8; ISO9660_SIZE_APPL_ID],
    /// Copyright file identifier.
    pub copyright_file_id: [u8; ISO9660_SIZE_COPYRIGHT_FILE_ID],
    /// Abstract file identifier.
    pub abstract_file_id: [u8; ISO9660_SIZE_ABSTRACT_FILE_ID],
    /// Bibliographic file identifier.
    pub bibl_file_id: [u8; ISO9660_SIZE_BIBL_FILE_ID],
    /// Volume creation date.
    pub volume_cre_date: [u8; ISO9660_SIZE_VOL_CRE_DATE],
    /// Volume modification date.
    pub volume_mod_date: [u8; ISO9660_SIZE_VOL_MOD_DATE],
    /// Volume expiration date.
    pub volume_exp_date: [u8; ISO9660_SIZE_VOL_EXP_DATE],
    /// Volume effective date.
    pub volume_eff_date: [u8; ISO9660_SIZE_VOL_EFF_DATE],
    /// File structure version.
    pub file_struct_ver: u8,
    /// Reference count.
    pub count: i32,
}

pub const VD_BOOT_RECORD: u8 = 0;
pub const VD_PRIMARY: u8 = 1;
pub const VD_SUPPL: u8 = 2;
pub const VD_PART: u8 = 3;
pub const VD_SET_TERM: u8 = 255;

/// Maximum attempts to read volume descriptors.
pub const MAX_ATTEMPTS: u32 = 20;

/// Root inode number.
pub const ROOT_INO_NR: u32 = 1;

/// Macro equivalent to ID_DIR_RECORD: use d_ino_nr as the inode number.
/// In the C source: `#define ID_DIR_RECORD(dir) dir->d_ino_nr`
pub fn id_dir_record(dir: &DirRecord) -> u32 {
    dir.d_ino_nr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_record_size() {
        // DirRecord should have a reasonable size
        let s = core::mem::size_of::<DirRecord>();
        assert!(s > 0);
        assert!(s < 1024);
    }

    #[test]
    fn ext_attr_rec_size() {
        let s = core::mem::size_of::<ExtAttrRec>();
        assert!(s > 0);
        assert!(s < 1024);
    }

    #[test]
    fn iso9660_vd_pri_size() {
        let s = core::mem::size_of::<Iso9660VdPri>();
        assert!(s > 0);
        assert!(s < 2048);
    }

    #[test]
    fn vd_constants_are_distinct() {
        assert_ne!(VD_BOOT_RECORD, VD_PRIMARY);
        assert_ne!(VD_PRIMARY, VD_SUPPL);
        assert_ne!(VD_PRIMARY, VD_SET_TERM);
    }

    #[test]
    fn root_ino_is_one() {
        assert_eq!(ROOT_INO_NR, 1);
    }

    #[test]
    fn id_dir_record_returns_ino() {
        let dr = DirRecord {
            length: 0,
            ext_attr_rec_length: 0,
            loc_extent_l: 0,
            loc_extent_m: 0,
            data_length_l: 0,
            data_length_m: 0,
            rec_date: [0; 7],
            file_flags: 0,
            file_unit_size: 0,
            inter_gap_size: 0,
            vol_seq_number: 0,
            length_file_id: 0,
            file_id: [0; ISO9660_MAX_FILE_ID_LEN],
            ext_attr: core::ptr::null_mut(),
            d_count: 0,
            d_mode: 0,
            d_phy_addr: 42,
            d_ino_nr: 42,
            d_mountpoint: false,
            d_next: core::ptr::null_mut(),
            d_prior: core::ptr::null_mut(),
            d_file_size: 0,
        };
        assert_eq!(id_dir_record(&dr), 42);
    }
}
