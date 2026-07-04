//! ISO 9660 constants — adapted from `minix/fs/iso9660fs/const.h`

/// Buffer size for getdents.
pub const GETDENTS_BUFSIZ: usize = 261;

/// Standard identifier for ISO 9660 volumes.
pub const ISO9660_STANDARD_ID: &[u8; 5] = b"CD001";

/// Number of directory record cache entries.
pub const NR_DIR_RECORDS: usize = 256;

/// Number of extended attribute record cache entries.
pub const NR_ATTR_RECS: usize = 256;

/// Error code / sentinel for "no address".
pub const NO_ADDRESS: i32 = -1;

/// Sentinel for "no free inodes".
pub const NO_FREE_INODES: i32 = -1;

/// Byte offset of the primary volume descriptor (sector 16 at 2048 bytes/sector).
pub const ISO9660_SUPER_BLOCK_POSITION: u64 = 32768;

/// Minimum logical block size for ISO 9660.
pub const ISO9660_MIN_BLOCK_SIZE: usize = 2048;

/// Maximum file identifier length.
pub const ISO9660_MAX_FILE_ID_LEN: usize = 32;

pub const ISO9660_SIZE_STANDARD_ID: usize = 5;
pub const ISO9660_SIZE_BOOT_SYS_ID: usize = 32;
pub const ISO9660_SIZE_BOOT_ID: usize = 32;
pub const ISO9660_SIZE_SYS_ID: usize = 32;
pub const ISO9660_SIZE_VOLUME_ID: usize = 32;
pub const ISO9660_SIZE_VOLUME_SET_ID: usize = 128;
pub const ISO9660_SIZE_PUBLISHER_ID: usize = 128;
pub const ISO9660_SIZE_DATA_PREP_ID: usize = 128;
pub const ISO9660_SIZE_APPL_ID: usize = 128;
pub const ISO9660_SIZE_COPYRIGHT_FILE_ID: usize = 37;
pub const ISO9660_SIZE_ABSTRACT_FILE_ID: usize = 37;
pub const ISO9660_SIZE_BIBL_FILE_ID: usize = 37;
pub const ISO9660_SIZE_VOL_CRE_DATE: usize = 17;
pub const ISO9660_SIZE_VOL_MOD_DATE: usize = 17;
pub const ISO9660_SIZE_VOL_EXP_DATE: usize = 17;
pub const ISO9660_SIZE_VOL_EFF_DATE: usize = 17;
pub const ISO9660_SIZE_ESCAPE_SQC: usize = 32;
pub const ISO9660_SIZE_PART_ID: usize = 32;
pub const ISO9660_SIZE_SYSTEM_USE: usize = 64;

/// End-of-file sentinel.
pub const END_OF_FILE: i32 = -104;

/// Directory type flag in file_flags.
pub const D_DIRECTORY: u8 = 0x2;

/// Mask for type bits in file_flags.
pub const D_TYPE: u8 = 0x8E;

/// No device constant.
pub const NO_DEV: u32 = 0xFFFF;

/// Maximum pathname length.
pub const PATH_MAX: usize = 1024;

/// Super-user UID and GID.
pub const SUPER_USER: u16 = 0;
pub const SYS_UID: u16 = 0;
pub const SYS_GID: u16 = 0;

/// Flags for parse_path.
pub const PATH_PENULTIMATE: i32 = 0o01;
pub const PATH_NONSYMBOLIC: i32 = 0o04;

/// Mode bits (from Minix stat.h).
pub const S_IFDIR: u16 = 0o040000;
pub const S_IFREG: u16 = 0o100000;

/// In-core mode type mask.
pub const I_TYPE: u16 = 0o170000;
pub const I_DIRECTORY: u16 = 0o040000;
pub const I_REGULAR: u16 = 0o100000;

/// Permission bits.
pub const R_BIT: u16 = 4;
pub const W_BIT: u16 = 2;
pub const X_BIT: u16 = 1;

/// READ / WRITE / PEEK flags.
pub const READING: i32 = 0;
pub const WRITING: i32 = 1;
pub const PEEKING: i32 = 2;

/// Block type constants for lmfs.
pub const NORMAL: i32 = 0;
pub const NO_READ: i32 = 1;
pub const FULL_DATA_BLOCK: i32 = 4;
pub const DIRECTORY_BLOCK: i32 = 1;
pub const INODE_BLOCK: i32 = 3;

/// VFS call number base.
pub const FS_BASE: i32 = 200;
pub const REQ_READ: i32 = FS_BASE + 19;
pub const REQ_PEEK: i32 = FS_BASE + 32;
pub const REQ_BREAD: i32 = FS_BASE + 11;

/// Dispatch table size.
pub const NREQS: usize = 34;

/// Invalid UID/GID sentinels.
pub const INVAL_UID: u16 = 0xFFFF;
pub const INVAL_GID: u16 = 0xFFFF;

/// NAME_MAX for file components.
pub const NAME_MAX: usize = 255;

/// VFS process number.
pub const VFS_PROC_NR: i32 = 0; // placeholder — real value from <minix/com.h>

/// Boolean constants.
pub const FALSE: i32 = 0;
pub const TRUE: i32 = 1;

pub const OK: i32 = 0;
pub const EINVAL: i32 = 22;
pub const EPERM: i32 = 1;
pub const ENOSPC: i32 = 28;
pub const EROFS: i32 = 30;
pub const ENOENT: i32 = 2;
pub const EFBIG: i32 = 27;
pub const EACCES: i32 = 13;
pub const ENOTDIR: i32 = 20;
pub const EBUSY: i32 = 16;
pub const EIO: i32 = 5;
pub const E2BIG: i32 = 7;
pub const ENAMETOOLONG: i32 = 36;
pub const EENTERMOUNT: i32 = 101;
pub const ELEAVEMOUNT: i32 = 102;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_is_zero() {
        assert_eq!(OK, 0);
    }

    #[test]
    fn standard_id_is_cd001() {
        assert_eq!(ISO9660_STANDARD_ID, b"CD001");
    }

    #[test]
    fn super_block_position_is_32768() {
        assert_eq!(ISO9660_SUPER_BLOCK_POSITION, 32768);
    }

    #[test]
    fn min_block_size_is_2048() {
        assert_eq!(ISO9660_MIN_BLOCK_SIZE, 2048);
    }

    #[test]
    fn d_type_mask_is_0x8e() {
        assert_eq!(D_TYPE, 0x8E);
    }

    #[test]
    fn errno_values_are_distinct() {
        let errnos = [
            OK,
            EINVAL,
            EPERM,
            ENOSPC,
            EROFS,
            ENOENT,
            EFBIG,
            EACCES,
            ENOTDIR,
            EBUSY,
            EIO,
            E2BIG,
            ENAMETOOLONG,
            EENTERMOUNT,
            ELEAVEMOUNT,
        ];
        for i in 0..errnos.len() {
            for j in (i + 1)..errnos.len() {
                assert_ne!(
                    errnos[i], errnos[j],
                    "duplicate errno {} at positions {} and {}",
                    errnos[i], i, j,
                );
            }
        }
    }
}
