//! ISO 9660 global state — adapted from `minix/fs/iso9660fs/glo.h`
//!
//! All global state is accessed through raw pointers to satisfy
//! Rust 2024's `deny(static_mut_refs)`.

use crate::iso9660::consts::*;
use crate::iso9660::types::*;
use core::mem::MaybeUninit;

/// Global ISO 9660 FS state.
#[repr(C)]
pub struct Iso9660Global {
    pub err_code: i32,
    pub rdwt_err: i32,
    pub caller_uid: u16,
    pub caller_gid: u16,
    pub req_nr: i32,
    pub path_processed: i16,
    pub user_path: [u8; PATH_MAX + 1],
    pub symloop: i32,
    pub unmountdone: bool,
    pub fs_dev: u32,
    pub fs_dev_label: [u8; 16],
    /// Primary volume descriptor.
    pub v_pri: Iso9660VdPri,
    /// Directory record cache.
    pub dir_records: [DirRecord; NR_DIR_RECORDS],
    /// Extended attribute record cache.
    pub ext_attr_recs: [ExtAttrRec; NR_ATTR_RECS],
}

/// Raw storage — only accessed via `addr_of_mut!` / raw pointers.
static mut ISOFS_STORAGE: MaybeUninit<Iso9660Global> = MaybeUninit::uninit();

/// Initialize globals. Must be called once before any access.
///
/// # Safety
///
/// Must be called exactly once, before any other code accesses globals.
pub unsafe fn isofs_init_globals() {
    let p: *mut Iso9660Global = core::ptr::addr_of_mut!(ISOFS_STORAGE).cast();
    p.write(Iso9660Global {
        err_code: 0,
        rdwt_err: 0,
        caller_uid: INVAL_UID,
        caller_gid: INVAL_GID,
        req_nr: 0,
        path_processed: 0,
        user_path: [0; PATH_MAX + 1],
        symloop: 0,
        unmountdone: false,
        fs_dev: NO_DEV,
        fs_dev_label: [0; 16],
        v_pri: Iso9660VdPri {
            vd_type: 0,
            standard_id: [0; ISO9660_SIZE_STANDARD_ID],
            vd_version: 0,
            system_id: [0; ISO9660_SIZE_SYS_ID],
            volume_id: [0; ISO9660_SIZE_VOLUME_ID],
            volume_space_size_l: 0,
            volume_space_size_m: 0,
            volume_set_size: 0,
            volume_sequence_number: 0,
            logical_block_size_l: 0,
            logical_block_size_m: 0,
            path_table_size_l: 0,
            path_table_size_m: 0,
            loc_l_occ_path_table: 0,
            loc_opt_l_occ_path_table: 0,
            loc_m_occ_path_table: 0,
            loc_opt_m_occ_path_table: 0,
            dir_rec_root: DirRecord {
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
                d_phy_addr: 0,
                d_ino_nr: 0,
                d_mountpoint: false,
                d_next: core::ptr::null_mut(),
                d_prior: core::ptr::null_mut(),
                d_file_size: 0,
            },
            volume_set_id: [0; ISO9660_SIZE_VOLUME_SET_ID],
            publisher_id: [0; ISO9660_SIZE_PUBLISHER_ID],
            data_preparer_id: [0; ISO9660_SIZE_DATA_PREP_ID],
            application_id: [0; ISO9660_SIZE_APPL_ID],
            copyright_file_id: [0; ISO9660_SIZE_COPYRIGHT_FILE_ID],
            abstract_file_id: [0; ISO9660_SIZE_ABSTRACT_FILE_ID],
            bibl_file_id: [0; ISO9660_SIZE_BIBL_FILE_ID],
            volume_cre_date: [0; ISO9660_SIZE_VOL_CRE_DATE],
            volume_mod_date: [0; ISO9660_SIZE_VOL_MOD_DATE],
            volume_exp_date: [0; ISO9660_SIZE_VOL_EXP_DATE],
            volume_eff_date: [0; ISO9660_SIZE_VOL_EFF_DATE],
            file_struct_ver: 0,
            count: 0,
        },
        dir_records: core::array::from_fn(|_| DirRecord {
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
            d_phy_addr: 0,
            d_ino_nr: 0,
            d_mountpoint: false,
            d_next: core::ptr::null_mut(),
            d_prior: core::ptr::null_mut(),
            d_file_size: 0,
        }),
        ext_attr_recs: core::array::from_fn(|_| ExtAttrRec {
            own_id: 0,
            group_id: 0,
            permissions: 0,
            file_cre_date: [0; ISO9660_SIZE_VOL_CRE_DATE],
            file_mod_date: [0; ISO9660_SIZE_VOL_MOD_DATE],
            file_exp_date: [0; ISO9660_SIZE_VOL_EXP_DATE],
            file_eff_date: [0; ISO9660_SIZE_VOL_EFF_DATE],
            rec_format: 0,
            rec_attrs: 0,
            rec_length: 0,
            system_id: [0; ISO9660_SIZE_SYS_ID],
            system_use: [0; ISO9660_SIZE_SYSTEM_USE],
            ext_attr_rec_ver: 0,
            len_esc_seq: 0,
            count: 0,
        }),
    });
}

/// Get a raw pointer to ISO FS global state.
///
/// # Safety
///
/// Caller must ensure no aliasing violations.
pub unsafe fn isofs_ptr() -> *mut Iso9660Global {
    core::ptr::addr_of_mut!(ISOFS_STORAGE).cast()
}

/// Helper to get a raw pointer to the primary volume descriptor.
///
/// # Safety
///
/// Caller must ensure no aliasing violations.
pub unsafe fn v_pri_ptr() -> *mut Iso9660VdPri {
    let isofs = core::ptr::addr_of_mut!(ISOFS_STORAGE).cast::<Iso9660Global>();
    core::ptr::addr_of_mut!((*isofs).v_pri)
}

/// Helper to get a raw pointer to a specific dir record.
///
/// # Safety
///
/// `idx` must be < NR_DIR_RECORDS. Caller must ensure no aliasing violations.
pub unsafe fn dir_record_ptr(idx: usize) -> *mut DirRecord {
    let isofs = core::ptr::addr_of_mut!(ISOFS_STORAGE).cast::<Iso9660Global>();
    let base = core::ptr::addr_of_mut!((*isofs).dir_records[0]);
    base.add(idx)
}

/// Helper to get a raw pointer to the dir_records array base.
///
/// # Safety
///
/// Caller must ensure no aliasing violations.
pub unsafe fn dir_records_ptr() -> *mut DirRecord {
    let isofs = core::ptr::addr_of_mut!(ISOFS_STORAGE).cast::<Iso9660Global>();
    core::ptr::addr_of_mut!((*isofs).dir_records[0])
}

/// Helper to get a raw pointer to the ext_attr_recs array base.
///
/// # Safety
///
/// Caller must ensure no aliasing violations.
pub unsafe fn ext_attr_recs_ptr() -> *mut ExtAttrRec {
    let isofs = core::ptr::addr_of_mut!(ISOFS_STORAGE).cast::<Iso9660Global>();
    core::ptr::addr_of_mut!((*isofs).ext_attr_recs[0])
}
